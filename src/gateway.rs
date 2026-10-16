//! The D-Bus gateway between a sandbox's private bus and the host bus.
//!
//! The gateway owns the portal and notification names on the private bus.
//! Each sandbox client that calls one gets its own connection to the host
//! bus, so host services see per-client lifetimes (inhibitors, portal
//! sessions) and the portal request paths it derives from the caller map
//! one-to-one onto the paths the client predicted from its own name. That
//! connection, opened by the connector (see `connector.rs`) in the
//! sandbox's PID namespace, registers the client's app id with the host
//! portal before its first call.
//!
//! Each `dbus-talk` name gets its own private-bus connection that owns it,
//! so calls addressed to that connection's unique name (as GDBus proxies
//! do) are unambiguous, and replies come from the connection the bus
//! expects them from. Calls to those names are forwarded unfiltered through
//! the same per-client host connection; signals from them are not.

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};

use crate::dbus::conn::Undecodable;
use crate::dbus::message::{self, Message};
use crate::dbus::{Conn, Value};
use crate::portal::{self, FileRequest, GATEWAY_NAMES, Policy, Verdict};

const NAME_FLAG_DO_NOT_QUEUE: u32 = 4;
const PRIMARY_OWNER: u32 = 1;

/// What the gateway needs from the supervisor.
pub trait Host: Send + Sync {
    /// The app id for a process (pid as seen inside the sandbox).
    fn app_id_for_pid(&self, sandbox_pid: u32) -> Option<String>;
    /// What may reach the host, from the sandbox's current config.
    fn policy(&self) -> Policy;
    /// A new socket connected to the host bus, opened from inside the
    /// sandbox's PID namespace, so host services read the process ids a
    /// client passes (Realtime) in that namespace.
    fn connect_host(&self) -> io::Result<OwnedFd>;
    /// Makes a host path the user chose in a file dialog visible in the
    /// sandbox and returns its sandbox path. `create` makes an empty file
    /// first (save dialogs).
    fn expose(&self, host_path: &Path, create: bool) -> Result<PathBuf>;
    fn log(&self, message: &str);
}

/// The private-bus connection a call came in on, and its reply goes out
/// on: the portal connection, or the connection of one `dbus-talk` name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    Portal,
    Talk(usize),
}

struct Pending {
    caller_serial: u32,
    file: Option<FileRequest>,
    via: Via,
}

/// The private-bus connection that owns one `dbus-talk` name.
struct Talk {
    name: String,
    conn: Conn,
}

struct Client {
    /// The client's unique name on the private bus.
    name: String,
    host: Conn,
    /// Path segments used to translate request and session paths.
    private_seg: String,
    host_seg: String,
    pending: Mutex<HashMap<u32, Pending>>,
    /// File chooser request objects (host paths) awaiting a response.
    file_requests: Mutex<HashMap<String, FileRequest>>,
    /// The host's current owners of the gateway names (well-known name to
    /// unique name): only their signals reach the client.
    owners: Mutex<HashMap<String, String>>,
}

pub struct Gateway {
    private: Conn,
    talk: Vec<Talk>,
    /// A second private-bus connection for synchronous queries, so the
    /// reader loop never has to wait on its own connection.
    control: Mutex<Conn>,
    host: Arc<dyn Host>,
    clients: Mutex<HashMap<String, Arc<Client>>>,
    /// Held while a client's host connection is being set up, so calls
    /// arriving at once on several gateway connections make only one.
    creating: Mutex<()>,
}

impl Gateway {
    /// Connects to the private bus at `bus_socket`, takes the gateway names
    /// and the `talk` names, and starts serving, one thread per connection.
    /// Each thread ends when the private bus closes its connection.
    pub fn start(bus_socket: &Path, talk: &[String], host: Arc<dyn Host>) -> Result<()> {
        let private = Conn::connect_path(bus_socket).context("connect to sandbox bus")?;
        let control = Conn::connect_path(bus_socket).context("connect to sandbox bus")?;
        for name in GATEWAY_NAMES {
            own(&private, name)?;
        }
        let talk = talk
            .iter()
            .map(|name| {
                let conn = Conn::connect_path(bus_socket).context("connect to sandbox bus")?;
                own(&conn, name)?;
                Ok(Talk {
                    name: name.clone(),
                    conn,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        private.bus_call(
            "AddMatch",
            &[Value::Str(
                "type='signal',sender='org.freedesktop.DBus',member='NameOwnerChanged'".into(),
            )],
        )?;
        let gw = Arc::new(Self {
            private,
            talk,
            control: Mutex::new(control),
            host,
            clients: Mutex::new(HashMap::new()),
            creating: Mutex::new(()),
        });
        for via in std::iter::once(Via::Portal).chain((0..gw.talk.len()).map(Via::Talk)) {
            let gw = gw.clone();
            std::thread::Builder::new()
                .name("dbus-gateway".into())
                .spawn(move || gw.serve(via))?;
        }
        Ok(())
    }

    fn conn(&self, via: Via) -> &Conn {
        match via {
            Via::Portal => &self.private,
            Via::Talk(i) => &self.talk[i].conn,
        }
    }

    fn serve(self: Arc<Self>, via: Via) {
        loop {
            let (msg, fds) = match self.conn(via).recv() {
                Ok(m) => m,
                Err(e) => {
                    if let Some(u) = Undecodable::of(&e) {
                        self.host.log(&format!(
                            "dbus gateway: skipped a message from {}: {}",
                            u.message.sender.as_deref().unwrap_or("?"),
                            u.reason
                        ));
                        self.reply_error(
                            via,
                            &u.message,
                            "org.freedesktop.DBus.Error.InvalidArgs",
                            "undecodable message",
                        );
                        continue;
                    }
                    self.host.log(&format!("dbus gateway stopped: {e}"));
                    return;
                }
            };
            match msg.kind {
                message::SIGNAL if msg.is("org.freedesktop.DBus", "NameOwnerChanged") => self.name_changed(&msg),
                message::METHOD_CALL => {
                    let mut msg = msg;
                    let dest = match via {
                        Via::Portal => portal::service_for(
                            msg.destination.as_deref().unwrap_or_default(),
                            self.private.unique_name(),
                            msg.path.as_deref().unwrap_or_default(),
                        ),
                        // Everything this connection receives is for its name.
                        Via::Talk(i) => Some(self.talk[i].name.as_str()),
                    };
                    if let Some(dest) = dest {
                        msg.destination = Some(dest.into());
                        if let Err(e) = self.forward_call(msg.clone(), fds, via) {
                            self.host.log(&format!("dbus gateway: {e:#}"));
                            self.reply_error(via, &msg, "org.freedesktop.DBus.Error.Failed", &format!("{e:#}"));
                        }
                    } else {
                        self.reply_error(via, &msg, "org.freedesktop.DBus.Error.UnknownObject", "no such object");
                    }
                }
                _ => {}
            }
        }
    }

    fn reply_error(&self, via: Via, to: &Message, name: &str, text: &str) {
        if to.expects_reply() {
            let _ = self.conn(via).send(&Message::error(to, name, text), &[]);
        }
    }

    /// Drops the host connection of a client that left the private bus.
    fn name_changed(&self, msg: &Message) {
        let Ok(args) = msg.args() else { return };
        let (Some(name), Some(new)) = (
            args.first().and_then(Value::as_str),
            args.get(2).and_then(Value::as_str),
        ) else {
            return;
        };
        if name.starts_with(':')
            && new.is_empty()
            && let Some(c) = self.clients.lock().unwrap().remove(name)
        {
            c.host.shutdown();
        }
    }

    fn forward_call(self: &Arc<Self>, msg: Message, fds: Vec<OwnedFd>, via: Via) -> Result<()> {
        let sender = msg.sender.clone().context("call without sender")?;
        let verdict = match via {
            Via::Portal => portal::check(&self.host.policy(), &msg),
            // The user listed this name: calls to it pass unchanged.
            Via::Talk(_) => Verdict::Forward { args: None, file: None },
        };
        let (args, file) = match verdict {
            Verdict::Forward { args, file } => (args, file),
            Verdict::Deny(why) => {
                self.host.log(&format!(
                    "dbus denied {}.{} on {} for {sender}: {why}",
                    msg.interface.as_deref().unwrap_or("?"),
                    msg.member.as_deref().unwrap_or("?"),
                    msg.path.as_deref().unwrap_or("?"),
                ));
                self.reply_error(via, &msg, "org.freedesktop.DBus.Error.AccessDenied", &why);
                return Ok(());
            }
        };
        let client = self.client(&sender)?;
        let mut out = Message {
            kind: message::METHOD_CALL,
            flags: msg.flags,
            path: msg.path.clone(),
            interface: msg.interface.clone(),
            member: msg.member.clone(),
            destination: msg.destination.clone(),
            signature: msg.signature.clone(),
            body: msg.body.clone(),
            ..Default::default()
        };
        if let Some(args) = args {
            out.set_body(&args);
        }
        if via == Via::Portal {
            portal::rewrite_paths(&mut out, &client.private_seg, &client.host_seg);
        }
        let borrowed: Vec<_> = fds.iter().map(|f| f.as_fd()).collect();
        let mut pending = client.pending.lock().unwrap();
        let serial = client.host.send(&out, &borrowed)?;
        if msg.expects_reply() {
            pending.insert(
                serial,
                Pending {
                    caller_serial: msg.serial,
                    file,
                    via,
                },
            );
        }
        Ok(())
    }

    /// The host connection for `sender`, created on first use.
    fn client(self: &Arc<Self>, sender: &str) -> Result<Arc<Client>> {
        if let Some(c) = self.clients.lock().unwrap().get(sender) {
            return Ok(c.clone());
        }
        let _creating = self.creating.lock().unwrap();
        if let Some(c) = self.clients.lock().unwrap().get(sender) {
            return Ok(c.clone());
        }
        let host = self
            .host
            .connect_host()
            .and_then(Conn::over)
            .context("connect to host bus")?;
        if let Some(app_id) = self.app_id(sender) {
            let register = Message::method_call(
                portal::PORTAL_NAME,
                portal::PORTAL_PATH,
                portal::REGISTRY_IFACE,
                "Register",
                &[Value::Str(app_id.clone()), Value::Dict("s".into(), "v".into(), vec![])],
            );
            if let Err(e) = host.call(&register) {
                self.host
                    .log(&format!("dbus gateway: register {app_id} with the host portal: {e}"));
            }
        }
        // Subscribed before asking, so no change of owner falls in between.
        let mut owners = HashMap::new();
        for name in GATEWAY_NAMES {
            host.bus_call("AddMatch", &[Value::Str(format!("type='signal',sender='{name}'"))])?;
            host.bus_call(
                "AddMatch",
                &[Value::Str(format!(
                    "type='signal',sender='org.freedesktop.DBus',member='NameOwnerChanged',arg0='{name}'"
                ))],
            )?;
            // Not running yet is fine: activation shows as a change of owner.
            if let Some(owner) = host
                .bus_call("GetNameOwner", &[Value::Str(name.into())])
                .ok()
                .and_then(|r| r.args().ok())
                .and_then(|a| a.first().and_then(|v| v.as_str().map(str::to_string)))
            {
                owners.insert(name.to_string(), owner);
            }
        }
        let client = Arc::new(Client {
            name: sender.into(),
            private_seg: portal::escape_unique(sender),
            host_seg: portal::escape_unique(host.unique_name()),
            host,
            pending: Mutex::new(HashMap::new()),
            file_requests: Mutex::new(HashMap::new()),
            owners: Mutex::new(owners),
        });
        self.clients.lock().unwrap().insert(sender.into(), client.clone());
        // A call can be read on one gateway connection after the caller's
        // departure was handled on another. Checked after the insert, a
        // departure either finds this client in the map or shows here.
        if !self.on_bus(sender) {
            self.clients.lock().unwrap().remove(sender);
            bail!("{sender} has left the sandbox bus");
        }
        let gw = self.clone();
        let c = client.clone();
        std::thread::Builder::new()
            .name("dbus-host".into())
            .spawn(move || gw.serve_host(&c))?;
        Ok(client)
    }

    /// Whether `name` is still connected to the private bus.
    fn on_bus(&self, name: &str) -> bool {
        let control = self.control.lock().unwrap();
        control
            .bus_call("NameHasOwner", &[Value::Str(name.into())])
            .ok()
            .and_then(|r| r.args().ok())
            .is_some_and(|a| a.first() == Some(&Value::Bool(true)))
    }

    fn app_id(&self, sender: &str) -> Option<String> {
        let control = self.control.lock().unwrap();
        let reply = control
            .bus_call("GetConnectionUnixProcessID", &[Value::Str(sender.into())])
            .ok()?;
        let pid = reply.args().ok()?.first()?.as_u32()?;
        self.host.app_id_for_pid(pid)
    }

    /// Relays replies and signals from one client's host connection.
    fn serve_host(&self, client: &Client) {
        loop {
            let (msg, fds) = match client.host.recv() {
                Ok(m) => m,
                Err(e) => {
                    if let Some(u) = Undecodable::of(&e) {
                        self.host
                            .log(&format!("dbus gateway: skipped a host message: {}", u.reason));
                        // A caller waiting for this reply gets an error instead.
                        if let Some(serial) = u.message.reply_serial {
                            let error = Message {
                                kind: message::ERROR,
                                error_name: Some("org.freedesktop.DBus.Error.Failed".into()),
                                reply_serial: Some(serial),
                                ..Default::default()
                            };
                            let _ = self.relay_reply(client, error, Vec::new());
                        }
                        continue;
                    }
                    self.clients.lock().unwrap().remove(&client.name);
                    return;
                }
            };
            let r = match msg.kind {
                message::METHOD_RETURN | message::ERROR => self.relay_reply(client, msg, fds),
                message::SIGNAL if msg.sender.as_deref() == Some(crate::dbus::conn::BUS_NAME) => {
                    if msg.is(crate::dbus::conn::BUS_NAME, "NameOwnerChanged")
                        && let Ok(args) = msg.args()
                    {
                        owner_changed(&mut client.owners.lock().unwrap(), &args);
                    }
                    Ok(())
                }
                message::SIGNAL => self.relay_signal(client, msg, fds),
                message::METHOD_CALL => {
                    if msg.expects_reply() {
                        let e = Message::error(&msg, "org.freedesktop.DBus.Error.UnknownMethod", "not supported");
                        let _ = client.host.send(&e, &[]);
                    }
                    Ok(())
                }
                _ => Ok(()),
            };
            if let Err(e) = r {
                self.host.log(&format!("dbus gateway: {e:#}"));
            }
        }
    }

    fn relay_reply(&self, client: &Client, msg: Message, fds: Vec<OwnedFd>) -> Result<()> {
        let Some(serial) = msg.reply_serial else { return Ok(()) };
        let Some(pending) = client.pending.lock().unwrap().remove(&serial) else {
            return Ok(());
        };
        if let (Some(kind), message::METHOD_RETURN) = (pending.file, msg.kind)
            && let Some(handle) = msg
                .args()
                .ok()
                .and_then(|a| a.first().and_then(|v| v.as_str().map(str::to_string)))
        {
            client.file_requests.lock().unwrap().insert(handle, kind);
        }
        let mut out = Message {
            kind: msg.kind,
            flags: message::FLAG_NO_REPLY_EXPECTED,
            error_name: msg.error_name.clone(),
            reply_serial: Some(pending.caller_serial),
            destination: Some(client.name.clone()),
            signature: msg.signature.clone(),
            body: msg.body.clone(),
            ..Default::default()
        };
        if pending.via == Via::Portal {
            portal::rewrite_paths(&mut out, &client.host_seg, &client.private_seg);
        }
        let borrowed: Vec<_> = fds.iter().map(|f| f.as_fd()).collect();
        // The bus only accepts the reply from the connection that was called.
        self.conn(pending.via).send(&out, &borrowed)?;
        Ok(())
    }

    fn relay_signal(&self, client: &Client, msg: Message, fds: Vec<OwnedFd>) -> Result<()> {
        // Any host process can send a signal straight to this connection;
        // only the current owners of the gateway names are passed on.
        let from_owner = msg
            .sender
            .as_deref()
            .is_some_and(|s| client.owners.lock().unwrap().values().any(|o| o == s));
        if !from_owner
            || !portal::signal_forwarded(
                msg.path.as_deref().unwrap_or_default(),
                msg.interface.as_deref().unwrap_or_default(),
            )
        {
            return Ok(());
        }
        let mut out = Message {
            kind: message::SIGNAL,
            flags: message::FLAG_NO_REPLY_EXPECTED,
            path: msg.path.clone(),
            interface: msg.interface.clone(),
            member: msg.member.clone(),
            destination: Some(client.name.clone()),
            signature: msg.signature.clone(),
            body: msg.body.clone(),
            ..Default::default()
        };
        if msg.is("org.freedesktop.portal.Request", "Response") {
            let kind = msg
                .path
                .as_deref()
                .and_then(|p| client.file_requests.lock().unwrap().remove(p));
            if let Some(kind) = kind {
                out.set_body(&map_file_results(self.host.as_ref(), msg.args()?, kind));
            }
        }
        portal::rewrite_paths(&mut out, &client.host_seg, &client.private_seg);
        let borrowed: Vec<_> = fds.iter().map(|f| f.as_fd()).collect();
        self.private.send(&out, &borrowed)?;
        Ok(())
    }
}

/// Applies a `NameOwnerChanged(name, old, new)` to the gateway names'
/// owners; other names are ignored.
fn owner_changed(owners: &mut HashMap<String, String>, args: &[Value]) {
    let (Some(name), Some(new)) = (
        args.first().and_then(Value::as_str),
        args.get(2).and_then(Value::as_str),
    ) else {
        return;
    };
    if !GATEWAY_NAMES.contains(&name) {
        return;
    }
    if new.is_empty() {
        owners.remove(name);
    } else {
        owners.insert(name.into(), new.into());
    }
}

/// Takes `name` on the private bus for `conn`, failing if anything else
/// owns it.
fn own(conn: &Conn, name: &str) -> Result<()> {
    let reply = conn.bus_call(
        "RequestName",
        &[Value::Str(name.into()), Value::U32(NAME_FLAG_DO_NOT_QUEUE)],
    )?;
    if reply.args().ok().and_then(|a| a.first().and_then(Value::as_u32)) != Some(PRIMARY_OWNER) {
        bail!("could not own {name} on the sandbox bus");
    }
    Ok(())
}

/// Replaces host file URIs in a file chooser response with sandbox paths.
/// URIs that cannot be exposed are dropped.
fn map_file_results(host: &dyn Host, mut args: Vec<Value>, kind: FileRequest) -> Vec<Value> {
    if args.first().and_then(Value::as_u32) != Some(0) {
        return args;
    }
    let Some(results) = args.get_mut(1) else { return args };
    let expose = |uri: &str| -> Option<String> {
        let path = portal::uri_to_path(uri)?;
        match host.expose(&path, kind == FileRequest::Save) {
            Ok(p) => Some(portal::path_to_uri(&p)),
            Err(e) => {
                host.log(&format!("dbus gateway: expose {}: {e:#}", path.display()));
                None
            }
        }
    };
    if kind == FileRequest::Screenshot {
        // A host path the sandbox cannot open is never passed on.
        let uri = results.dict_get("uri").and_then(Value::as_str).and_then(expose);
        match uri {
            Some(uri) => results.dict_set("uri", Value::Str(uri)),
            None => results.dict_remove("uri"),
        }
        return args;
    }
    let Some(Value::Array(_, uris)) = results.dict_get("uris").cloned() else {
        return args;
    };
    let mapped = uris
        .iter()
        .filter_map(Value::as_str)
        .filter_map(expose)
        .map(Value::Str)
        .collect();
    results.dict_set("uris", Value::Array("s".into(), mapped));
    args
}

/// The host bus address: `PSBOX_HOST_BUS`, else the session bus.
pub fn host_bus_address() -> io::Result<String> {
    for var in ["PSBOX_HOST_BUS", "DBUS_SESSION_BUS_ADDRESS"] {
        if let Ok(v) = std::env::var(var)
            && !v.is_empty()
        {
            return Ok(v);
        }
    }
    let rt = std::env::var("XDG_RUNTIME_DIR").map_err(|_| io::Error::other("no session bus address"))?;
    Ok(format!("unix:path={rt}/bus"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeHost;

    impl Host for FakeHost {
        fn app_id_for_pid(&self, _: u32) -> Option<String> {
            None
        }
        fn policy(&self) -> Policy {
            Policy::default()
        }
        fn connect_host(&self) -> io::Result<OwnedFd> {
            Err(io::ErrorKind::Unsupported.into())
        }
        fn expose(&self, host_path: &Path, create: bool) -> Result<PathBuf> {
            if host_path.starts_with("/forbidden") {
                bail!("nope");
            }
            let tag = if create { "save" } else { "open" };
            Ok(Path::new("/run/user/1000/doc")
                .join(tag)
                .join(host_path.file_name().unwrap()))
        }
        fn log(&self, _: &str) {}
    }

    fn response(code: u32, uris: &[&str]) -> Vec<Value> {
        let mut results = Value::Dict("s".into(), "v".into(), vec![]);
        results.dict_set(
            "uris",
            Value::Array("s".into(), uris.iter().map(|u| Value::Str((*u).into())).collect()),
        );
        results.dict_set("choices", Value::Array("(ss)".into(), vec![]));
        vec![Value::U32(code), results]
    }

    fn uris(args: &[Value]) -> Vec<String> {
        let Some(Value::Array(_, items)) = args[1].dict_get("uris") else {
            panic!("no uris")
        };
        items.iter().map(|v| v.as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn maps_chosen_files_into_the_sandbox() {
        let out = map_file_results(
            &FakeHost,
            response(
                0,
                &[
                    "file:///home/u/My%20Notes.txt",
                    "file:///forbidden/x",
                    "https://not-a-file",
                ],
            ),
            FileRequest::Open,
        );
        assert_eq!(uris(&out), vec!["file:///run/user/1000/doc/open/My%20Notes.txt"]);
        assert!(out[1].dict_get("choices").is_some());
        let saved = map_file_results(&FakeHost, response(0, &["file:///home/u/new.txt"]), FileRequest::Save);
        assert_eq!(uris(&saved), vec!["file:///run/user/1000/doc/save/new.txt"]);
    }

    #[test]
    fn screenshots_are_exposed_or_withheld() {
        let shot = |uri: &str| {
            let mut results = Value::Dict("s".into(), "v".into(), vec![]);
            results.dict_set("uri", Value::Str(uri.into()));
            map_file_results(&FakeHost, vec![Value::U32(0), results], FileRequest::Screenshot)
        };
        assert_eq!(
            shot("file:///home/u/Pictures/shot.png")[1].dict_get("uri"),
            Some(&Value::Str("file:///run/user/1000/doc/open/shot.png".into()))
        );
        assert_eq!(shot("file:///forbidden/shot.png")[1].dict_get("uri"), None);
    }

    #[test]
    fn tracks_the_owners_of_the_gateway_names() {
        let mut owners = HashMap::new();
        let change = |name: &str, new: &str| {
            vec![
                Value::Str(name.into()),
                Value::Str(String::new()),
                Value::Str(new.into()),
            ]
        };
        owner_changed(&mut owners, &change(portal::PORTAL_NAME, ":1.40"));
        owner_changed(&mut owners, &change("org.example.Other", ":1.41"));
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[portal::PORTAL_NAME], ":1.40");
        owner_changed(&mut owners, &change(portal::PORTAL_NAME, ""));
        assert!(owners.is_empty());
    }

    #[test]
    fn leaves_cancelled_responses_alone() {
        let args = response(1, &["file:///home/u/a"]);
        assert_eq!(map_file_results(&FakeHost, args.clone(), FileRequest::Open), args);
    }
}
