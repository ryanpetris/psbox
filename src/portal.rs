//! What the D-Bus gateway forwards to the host, and how forwarded messages
//! are rewritten. Everything here is pure so it can be tested directly.
//!
//! The host's portal treats psbox as an unsandboxed host application, so the
//! gateway is the place that keeps portal access sandbox-appropriate: only
//! listed interfaces pass, interfaces that could run host commands or act
//! silently are refused or forced interactive, and host file paths are
//! never handed to the sandbox as-is.

use std::path::{Path, PathBuf};

use crate::dbus::message::Message;
use crate::dbus::value::Value;

pub const PORTAL_NAME: &str = "org.freedesktop.portal.Desktop";
pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
pub const NOTIFICATIONS_NAME: &str = "org.freedesktop.Notifications";
pub const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";
pub const REGISTRY_IFACE: &str = "org.freedesktop.host.portal.Registry";

/// Names the gateway owns on the sandbox bus.
pub const GATEWAY_NAMES: [&str; 2] = [PORTAL_NAME, NOTIFICATIONS_NAME];

const REQUEST_PREFIX: &str = "/org/freedesktop/portal/desktop/request/";
const SESSION_PREFIX: &str = "/org/freedesktop/portal/desktop/session/";

/// Portal interfaces forwarded without changes.
const PLAIN: [&str; 15] = [
    "org.freedesktop.portal.Account",
    "org.freedesktop.portal.Email",
    "org.freedesktop.portal.FileChooser",
    "org.freedesktop.portal.GlobalShortcuts",
    "org.freedesktop.portal.Inhibit",
    "org.freedesktop.portal.MemoryMonitor",
    "org.freedesktop.portal.NetworkMonitor",
    "org.freedesktop.portal.Notification",
    "org.freedesktop.portal.PowerProfileMonitor",
    "org.freedesktop.portal.Print",
    "org.freedesktop.portal.ProxyResolver",
    "org.freedesktop.portal.Secret",
    "org.freedesktop.portal.Settings",
    "org.freedesktop.portal.Trash",
    "org.freedesktop.portal.OpenURI",
];

/// Whether `name` may be D-Bus activated inside the sandbox. Portals belong
/// to the host (through the gateway); started inside, they would act on the
/// sandbox's view instead. systemd's session-bus stub only fails there.
pub fn activation_allowed(name: &str) -> bool {
    !GATEWAY_NAMES.contains(&name)
        && name != "org.freedesktop.systemd1"
        && !name.starts_with("org.freedesktop.portal.")
        && !name.starts_with("org.freedesktop.impl.portal.")
}

#[derive(Debug, Clone)]
pub struct Policy {
    /// Allow the Camera portal (the `camera` device class).
    pub camera: bool,
    /// Allow the Realtime portal (the `audio` device class).
    pub realtime: bool,
    /// URI schemes `OpenURI.OpenURI` may open on the host.
    pub uri_schemes: Vec<String>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            camera: false,
            realtime: false,
            uri_schemes: vec!["http".into(), "https".into(), "mailto".into()],
        }
    }
}

/// The gateway service a call is for. GDBus proxies address the owner's
/// unique name instead of the well-known name, so calls to the gateway's own
/// unique name are attributed by object path.
pub fn service_for(dest: &str, gateway_unique: &str, path: &str) -> Option<&'static str> {
    if let Some(name) = GATEWAY_NAMES.iter().find(|n| **n == dest) {
        return Some(name);
    }
    if dest != gateway_unique {
        return None;
    }
    if path == PORTAL_PATH || path.starts_with(REQUEST_PREFIX) || path.starts_with(SESSION_PREFIX) {
        Some(PORTAL_NAME)
    } else if path == NOTIFICATIONS_PATH {
        Some(NOTIFICATIONS_NAME)
    } else {
        None
    }
}

/// Whether a signal arriving on a client's host connection is passed to the
/// client: only portal and notification signals, on their own objects. A
/// host process can send a signal straight to any connection, so this is
/// what keeps other services (including `dbus-talk` ones, whose signals
/// are not forwarded) out.
pub fn signal_forwarded(path: &str, iface: &str) -> bool {
    let portal_object = path == PORTAL_PATH || path.starts_with(&format!("{PORTAL_PATH}/"));
    let notifications_object = path == NOTIFICATIONS_PATH;
    match iface {
        "org.freedesktop.DBus.Properties" => portal_object || notifications_object,
        NOTIFICATIONS_NAME => notifications_object,
        i => portal_object && i.starts_with("org.freedesktop.portal."),
    }
}

/// The kind of file-chooser request, for mapping its results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRequest {
    Open,
    Save,
    /// A screenshot: the response's single `uri` is the new image.
    Screenshot,
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    /// Forward, with these replacement arguments when present.
    Forward {
        args: Option<Vec<Value>>,
        file: Option<FileRequest>,
    },
    Deny(String),
}

fn forward() -> Verdict {
    Verdict::Forward { args: None, file: None }
}

fn deny(why: impl Into<String>) -> Verdict {
    Verdict::Deny(why.into())
}

fn introspection(iface: &str) -> bool {
    iface == "org.freedesktop.DBus.Introspectable" || iface == "org.freedesktop.DBus.Peer"
}

/// Decides what to do with a method call addressed to a gateway name.
pub fn check(policy: &Policy, msg: &Message) -> Verdict {
    let dest = msg.destination.as_deref().unwrap_or_default();
    let path = msg.path.as_deref().unwrap_or_default();
    let iface = msg.interface.as_deref().unwrap_or_default();
    let member = msg.member.as_deref().unwrap_or_default();
    match dest {
        PORTAL_NAME if path == PORTAL_PATH => check_portal(policy, msg, iface, member),
        PORTAL_NAME if path.starts_with(REQUEST_PREFIX) => match iface {
            "org.freedesktop.portal.Request" => forward(),
            _ => deny("only org.freedesktop.portal.Request on request objects"),
        },
        PORTAL_NAME if path.starts_with(SESSION_PREFIX) => match iface {
            "org.freedesktop.portal.Session" => forward(),
            "org.freedesktop.DBus.Properties" if member == "Get" || member == "GetAll" => forward(),
            _ => deny("only org.freedesktop.portal.Session on session objects"),
        },
        NOTIFICATIONS_NAME if path == NOTIFICATIONS_PATH => match iface {
            "org.freedesktop.Notifications" if member == "Notify" => strip_notify_paths(msg),
            "org.freedesktop.Notifications" => forward(),
            "org.freedesktop.DBus.Properties" if member == "Get" || member == "GetAll" => forward(),
            i if introspection(i) => forward(),
            _ => deny("interface not allowed"),
        },
        _ => deny("object not allowed"),
    }
}

fn allowed_portal_iface(policy: &Policy, iface: &str) -> bool {
    PLAIN.contains(&iface)
        || iface == "org.freedesktop.portal.Screenshot"
        || iface == "org.freedesktop.portal.ScreenCast"
        || (policy.camera && iface == "org.freedesktop.portal.Camera")
        || (policy.realtime && iface == "org.freedesktop.portal.Realtime")
}

fn check_portal(policy: &Policy, msg: &Message, iface: &str, member: &str) -> Verdict {
    if introspection(iface) {
        return forward();
    }
    if iface == "org.freedesktop.DBus.Properties" {
        let target = msg
            .args()
            .ok()
            .and_then(|a| a.first().and_then(|v| v.as_str().map(str::to_string)));
        return match (member, target) {
            ("Get" | "GetAll", Some(t)) if allowed_portal_iface(policy, &t) => forward(),
            _ => deny("property access not allowed"),
        };
    }
    if !allowed_portal_iface(policy, iface) {
        return deny(format!("{iface} is not forwarded"));
    }
    let Ok(mut args) = msg.args() else {
        return deny("malformed arguments");
    };
    match (iface, member) {
        ("org.freedesktop.portal.OpenURI", "OpenURI") => {
            let uri = args.get(1).and_then(Value::as_str).unwrap_or_default();
            let scheme = uri
                .split_once(':')
                .map(|(s, _)| s.to_ascii_lowercase())
                .unwrap_or_default();
            if policy.uri_schemes.contains(&scheme) {
                forward()
            } else {
                deny(format!("opening {scheme:?} URIs on the host is not allowed"))
            }
        }
        // Without this the host may take screenshots for a "host app"
        // without asking.
        // Screenshot(parent_window, options): the image is made visible in
        // the sandbox like a file-chooser result.
        ("org.freedesktop.portal.Screenshot", "Screenshot") => {
            match args.get_mut(1) {
                Some(opts @ Value::Dict(..)) if msg.signature == "sa{sv}" => {
                    opts.dict_set("interactive", Value::Bool(true));
                }
                _ => return deny("malformed arguments"),
            }
            Verdict::Forward {
                args: Some(args),
                file: Some(FileRequest::Screenshot),
            }
        }
        // OpenFile(parent_window, fd, options): the host always asks which
        // app opens the file, so a sandbox cannot launch a host handler on
        // a file it wrote without the user choosing to.
        ("org.freedesktop.portal.OpenURI", "OpenFile") => {
            match args.get_mut(2) {
                Some(opts @ Value::Dict(..)) if msg.signature == "sha{sv}" => {
                    opts.dict_set("ask", Value::Bool(true));
                }
                _ => return deny("malformed arguments"),
            }
            Verdict::Forward {
                args: Some(args),
                file: None,
            }
        }
        // No remembered selections: every screen cast asks.
        ("org.freedesktop.portal.ScreenCast", "SelectSources") => {
            if let Some(opts) = args.get_mut(1) {
                opts.dict_remove("persist_mode");
                opts.dict_remove("restore_token");
            }
            Verdict::Forward {
                args: Some(args),
                file: None,
            }
        }
        // MakeThreadRealtimeWithPID(process, thread, priority) and
        // MakeThreadHighPriorityWithPID(process, thread, nice): the ids are
        // the sandbox's, and the host portal finds them in the PID namespace
        // of the process that opened the host connection, which is the
        // sandbox's.
        ("org.freedesktop.portal.Realtime", "MakeThreadRealtimeWithPID") if msg.signature == "ttu" => forward(),
        ("org.freedesktop.portal.Realtime", "MakeThreadHighPriorityWithPID") if msg.signature == "tti" => forward(),
        ("org.freedesktop.portal.Realtime", _) => deny("only the WithPID calls of the Realtime portal"),
        ("org.freedesktop.portal.FileChooser", "OpenFile" | "SaveFile" | "SaveFiles") => {
            // Sandbox paths mean nothing to the host dialog.
            if let Some(opts) = args.get_mut(2) {
                opts.dict_remove("current_folder");
                opts.dict_remove("current_file");
                opts.dict_remove("files");
            }
            let file = if member == "OpenFile" {
                FileRequest::Open
            } else {
                FileRequest::Save
            };
            Verdict::Forward {
                args: Some(args),
                file: Some(file),
            }
        }
        _ => forward(),
    }
}

/// Image and sound hints name files by path, which only make sense inside
/// the sandbox; drop them rather than have the host read its own files.
fn strip_notify_paths(msg: &Message) -> Verdict {
    let Ok(mut args) = msg.args() else {
        return deny("malformed arguments");
    };
    if let Some(hints) = args.get_mut(6) {
        hints.dict_remove("image-path");
        hints.dict_remove("image_path");
        hints.dict_remove("sound-file");
    }
    Verdict::Forward {
        args: Some(args),
        file: None,
    }
}

/// The path segment portals derive from a unique name (`:1.42` -> `1_42`).
pub fn escape_unique(name: &str) -> String {
    name.trim_start_matches(':').replace('.', "_")
}

/// Swaps the sender segment of a portal request or session path.
pub fn swap_sender(path: &str, from: &str, to: &str) -> Option<String> {
    for prefix in [REQUEST_PREFIX, SESSION_PREFIX] {
        if let Some(rest) = path.strip_prefix(prefix) {
            let (seg, tail) = rest.split_once('/').unwrap_or((rest, ""));
            if seg == from {
                return Some(if tail.is_empty() {
                    format!("{prefix}{to}")
                } else {
                    format!("{prefix}{to}/{tail}")
                });
            }
        }
    }
    None
}

/// Rewrites request and session paths in the header and body of `msg`.
pub fn rewrite_paths(msg: &mut Message, from: &str, to: &str) {
    if let Some(p) = msg.path.as_deref().and_then(|p| swap_sender(p, from, to)) {
        msg.path = Some(p);
    }
    if !msg.signature.contains(['o', 'v']) {
        return;
    }
    let Ok(mut args) = msg.args() else { return };
    let mut changed = false;
    for a in &mut args {
        a.map_paths(&mut |p| {
            let r = swap_sender(p, from, to);
            changed |= r.is_some();
            r
        });
    }
    if changed {
        msg.set_body(&args);
    }
}

/// Decodes a `file://` URI into a path.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    let b = path.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            out.push(u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).ok()?, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

/// Encodes a path as a `file://` URI.
pub fn path_to_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("file://");
    for &c in path.as_os_str().as_bytes() {
        if c.is_ascii_alphanumeric() || b"/-_.~".contains(&c) {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(dest: &str, path: &str, iface: &str, member: &str, args: &[Value]) -> Message {
        let mut m = Message::method_call(dest, path, iface, member, args);
        m.sender = Some(":1.5".into());
        m
    }

    fn opts() -> Value {
        Value::Dict("s".into(), "v".into(), vec![])
    }

    fn portal(iface: &str, member: &str, args: &[Value]) -> Verdict {
        check(&Policy::default(), &call(PORTAL_NAME, PORTAL_PATH, iface, member, args))
    }

    #[test]
    fn plain_interfaces_forward() {
        let args = [
            Value::Str("org.freedesktop.appearance".into()),
            Value::Str("color-scheme".into()),
        ];
        assert_eq!(portal("org.freedesktop.portal.Settings", "ReadOne", &args), forward());
    }

    #[test]
    fn dangerous_interfaces_are_denied() {
        for iface in [
            "org.freedesktop.portal.Background",
            "org.freedesktop.portal.DynamicLauncher",
            "org.freedesktop.portal.RemoteDesktop",
            "org.freedesktop.portal.Realtime",
            "org.freedesktop.portal.Location",
            "org.freedesktop.portal.Camera",
            "org.freedesktop.portal.Wallpaper",
            "org.freedesktop.portal.Usb",
        ] {
            assert!(matches!(portal(iface, "Anything", &[]), Verdict::Deny(_)), "{iface}");
        }
        let with_camera = Policy {
            camera: true,
            ..Policy::default()
        };
        let m = call(
            PORTAL_NAME,
            PORTAL_PATH,
            "org.freedesktop.portal.Camera",
            "AccessCamera",
            &[opts()],
        );
        assert_eq!(check(&with_camera, &m), forward());
    }

    #[test]
    fn realtime_needs_the_policy_and_translates_ids() {
        let with_realtime = Policy {
            realtime: true,
            ..Policy::default()
        };
        let rt = |member: &str, args: &[Value]| {
            check(
                &with_realtime,
                &call(
                    PORTAL_NAME,
                    PORTAL_PATH,
                    "org.freedesktop.portal.Realtime",
                    member,
                    args,
                ),
            )
        };
        let args = [Value::U64(12), Value::U64(13), Value::U32(5)];
        assert_eq!(rt("MakeThreadRealtimeWithPID", &args), forward());
        let nice = [Value::U64(12), Value::U64(13), Value::I32(-5)];
        assert_eq!(rt("MakeThreadHighPriorityWithPID", &nice), forward());
        // Wrong argument types, other members, and no policy are refused.
        assert!(matches!(rt("MakeThreadRealtimeWithPID", &nice), Verdict::Deny(_)));
        assert!(matches!(rt("MakeThreadRealtime", &[Value::U64(13)]), Verdict::Deny(_)));
        let without = call(
            PORTAL_NAME,
            PORTAL_PATH,
            "org.freedesktop.portal.Realtime",
            "MakeThreadRealtimeWithPID",
            &args,
        );
        assert!(matches!(check(&Policy::default(), &without), Verdict::Deny(_)));
        // Its properties are readable when it is forwarded.
        let prop = call(
            PORTAL_NAME,
            PORTAL_PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
            &[
                Value::Str("org.freedesktop.portal.Realtime".into()),
                Value::Str("MaxRealtimePriority".into()),
            ],
        );
        assert_eq!(check(&with_realtime, &prop), forward());
        assert!(matches!(check(&Policy::default(), &prop), Verdict::Deny(_)));
    }

    #[test]
    fn open_uri_schemes() {
        let open = |uri: &str| {
            portal(
                "org.freedesktop.portal.OpenURI",
                "OpenURI",
                &[Value::Str("".into()), Value::Str(uri.into()), opts()],
            )
        };
        assert_eq!(open("https://example.com"), forward());
        assert_eq!(open("MAILTO:a@b"), forward());
        assert!(matches!(open("file:///home/u/.ssh/id_ed25519"), Verdict::Deny(_)));
        assert!(matches!(open("steam://run/1"), Verdict::Deny(_)));
    }

    #[test]
    fn screenshots_are_forced_interactive() {
        let Verdict::Forward { args: Some(args), .. } = portal(
            "org.freedesktop.portal.Screenshot",
            "Screenshot",
            &[Value::Str("".into()), {
                let mut o = opts();
                o.dict_set("interactive", Value::Bool(false));
                o
            }],
        ) else {
            panic!("expected forward with new args")
        };
        assert_eq!(args[1].dict_get("interactive"), Some(&Value::Bool(true)));
        // The old three-argument shape is not a valid call.
        let bad = portal(
            "org.freedesktop.portal.Screenshot",
            "Screenshot",
            &[Value::Str("".into()), opts(), opts()],
        );
        assert!(matches!(bad, Verdict::Deny(_)));
    }

    #[test]
    fn open_file_always_asks() {
        let Verdict::Forward { args: Some(args), .. } = portal(
            "org.freedesktop.portal.OpenURI",
            "OpenFile",
            &[Value::Str("".into()), Value::Fd(0), {
                let mut o = opts();
                o.dict_set("ask", Value::Bool(false));
                o
            }],
        ) else {
            panic!("expected forward with new args")
        };
        assert_eq!(args[2].dict_get("ask"), Some(&Value::Bool(true)));
    }

    #[test]
    fn screencast_restore_tokens_are_dropped() {
        let mut o = opts();
        o.dict_set("persist_mode", Value::U32(2));
        o.dict_set("restore_token", Value::Str("abc".into()));
        o.dict_set("types", Value::U32(1));
        let session = Value::Path(format!("{SESSION_PREFIX}1_5/s1"));
        let Verdict::Forward { args: Some(args), .. } =
            portal("org.freedesktop.portal.ScreenCast", "SelectSources", &[session, o])
        else {
            panic!("expected forward")
        };
        assert_eq!(args[1].dict_get("persist_mode"), None);
        assert_eq!(args[1].dict_get("restore_token"), None);
        assert_eq!(args[1].dict_get("types"), Some(&Value::U32(1)));
    }

    #[test]
    fn file_chooser_drops_sandbox_paths_and_tags_requests() {
        let mut o = opts();
        o.dict_set(
            "current_folder",
            Value::Array("y".into(), b"/home/u\0".iter().map(|b| Value::Byte(*b)).collect()),
        );
        o.dict_set("current_name", Value::Str("notes.txt".into()));
        let v = portal(
            "org.freedesktop.portal.FileChooser",
            "SaveFile",
            &[Value::Str("".into()), Value::Str("Save".into()), o],
        );
        let Verdict::Forward { args: Some(args), file } = v else {
            panic!("expected forward")
        };
        assert_eq!(file, Some(FileRequest::Save));
        assert_eq!(args[2].dict_get("current_folder"), None);
        assert_eq!(
            args[2].dict_get("current_name").and_then(Value::as_str),
            Some("notes.txt")
        );
    }

    #[test]
    fn properties_only_for_allowed_interfaces() {
        let get = |iface: &str| {
            portal(
                "org.freedesktop.DBus.Properties",
                "Get",
                &[Value::Str(iface.into()), Value::Str("version".into())],
            )
        };
        assert_eq!(get("org.freedesktop.portal.Settings"), forward());
        assert!(matches!(get("org.freedesktop.portal.Background"), Verdict::Deny(_)));
        let set = portal(
            "org.freedesktop.DBus.Properties",
            "Set",
            &[Value::Str("org.freedesktop.portal.Settings".into())],
        );
        assert!(matches!(set, Verdict::Deny(_)));
    }

    #[test]
    fn requests_sessions_and_other_objects() {
        let p = Policy::default();
        let req = call(
            PORTAL_NAME,
            &format!("{REQUEST_PREFIX}1_5/t"),
            "org.freedesktop.portal.Request",
            "Close",
            &[],
        );
        assert_eq!(check(&p, &req), forward());
        let bad = call(
            PORTAL_NAME,
            "/org/freedesktop/portal/documents",
            "org.freedesktop.portal.Documents",
            "Add",
            &[],
        );
        assert!(matches!(check(&p, &bad), Verdict::Deny(_)));
        let other = call(
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "x",
            "StartTransientUnit",
            &[],
        );
        assert!(matches!(check(&p, &other), Verdict::Deny(_)));
    }

    #[test]
    fn notify_drops_path_hints() {
        let mut hints = opts();
        hints.dict_set("image-path", Value::Str("/home/u/.ssh/id".into()));
        hints.dict_set("urgency", Value::Byte(1));
        let args = [
            Value::Str("app".into()),
            Value::U32(0),
            Value::Str("".into()),
            Value::Str("Hi".into()),
            Value::Str("Body".into()),
            Value::Array("s".into(), vec![]),
            hints,
            Value::I32(-1),
        ];
        let m = call(
            NOTIFICATIONS_NAME,
            NOTIFICATIONS_PATH,
            "org.freedesktop.Notifications",
            "Notify",
            &args,
        );
        let Verdict::Forward { args: Some(a), .. } = check(&Policy::default(), &m) else {
            panic!("expected forward")
        };
        assert_eq!(a[6].dict_get("image-path"), None);
        assert_eq!(a[6].dict_get("urgency"), Some(&Value::Byte(1)));
    }

    #[test]
    fn swaps_request_paths_both_ways() {
        let p = format!("{REQUEST_PREFIX}1_5/token");
        let host = swap_sender(&p, "1_5", "1_987").unwrap();
        assert_eq!(host, format!("{REQUEST_PREFIX}1_987/token"));
        assert_eq!(swap_sender(&host, "1_987", "1_5").unwrap(), p);
        assert_eq!(swap_sender(&format!("{REQUEST_PREFIX}1_6/t"), "1_5", "1_9"), None);
        assert_eq!(swap_sender("/org/freedesktop/portal/desktop", "1_5", "1_9"), None);
        assert_eq!(escape_unique(":1.42"), "1_42");
    }

    #[test]
    fn rewrites_header_and_body_paths() {
        let mut m = call(
            PORTAL_NAME,
            &format!("{SESSION_PREFIX}1_5/s"),
            "org.freedesktop.portal.ScreenCast",
            "Start",
            &[
                Value::Path(format!("{SESSION_PREFIX}1_5/s")),
                Value::Str("".into()),
                opts(),
            ],
        );
        rewrite_paths(&mut m, "1_5", "1_9");
        assert_eq!(m.path.as_deref(), Some(format!("{SESSION_PREFIX}1_9/s").as_str()));
        assert_eq!(m.args().unwrap()[0], Value::Path(format!("{SESSION_PREFIX}1_9/s")));
    }

    #[test]
    fn uris_round_trip() {
        let p = Path::new("/home/u/My Docs/ünï.txt");
        let uri = path_to_uri(p);
        assert_eq!(uri, "file:///home/u/My%20Docs/%C3%BCn%C3%AF.txt");
        assert_eq!(uri_to_path(&uri).unwrap(), p);
        assert_eq!(uri_to_path("file://localhost/tmp/x").unwrap(), Path::new("/tmp/x"));
        assert_eq!(uri_to_path("https://x/y"), None);
    }

    #[test]
    fn calls_to_the_unique_name_are_attributed_by_path() {
        assert_eq!(service_for(PORTAL_NAME, ":1.0", "/anything"), Some(PORTAL_NAME));
        assert_eq!(service_for(":1.0", ":1.0", PORTAL_PATH), Some(PORTAL_NAME));
        assert_eq!(
            service_for(":1.0", ":1.0", &format!("{REQUEST_PREFIX}1_5/t")),
            Some(PORTAL_NAME)
        );
        assert_eq!(
            service_for(":1.0", ":1.0", NOTIFICATIONS_PATH),
            Some(NOTIFICATIONS_NAME)
        );
        assert_eq!(service_for(":1.0", ":1.0", "/org/freedesktop/systemd1"), None);
        assert_eq!(service_for(":1.7", ":1.0", PORTAL_PATH), None);
    }

    #[test]
    fn only_portal_and_notification_signals_reach_clients() {
        let request = format!("{REQUEST_PREFIX}1_5/t");
        assert!(signal_forwarded(&request, "org.freedesktop.portal.Request"));
        assert!(signal_forwarded(PORTAL_PATH, "org.freedesktop.portal.Settings"));
        assert!(signal_forwarded(PORTAL_PATH, "org.freedesktop.DBus.Properties"));
        assert!(signal_forwarded(NOTIFICATIONS_PATH, "org.freedesktop.Notifications"));
        assert!(!signal_forwarded(
            "/org/freedesktop/secrets",
            "org.freedesktop.Secret.Prompt"
        ));
        assert!(!signal_forwarded(
            "/org/freedesktop/secrets",
            "org.freedesktop.DBus.Properties"
        ));
        assert!(!signal_forwarded(PORTAL_PATH, "org.freedesktop.Secret.Prompt"));
        assert!(!signal_forwarded(
            "/org/freedesktop/portal/desktopX",
            "org.freedesktop.portal.Settings"
        ));
        assert!(!signal_forwarded(PORTAL_PATH, "org.freedesktop.Notifications"));
    }

    #[test]
    fn activation_blocks_portals() {
        assert!(!activation_allowed("org.freedesktop.portal.Desktop"));
        assert!(!activation_allowed("org.freedesktop.impl.portal.desktop.gtk"));
        assert!(!activation_allowed("org.freedesktop.Notifications"));
        assert!(!activation_allowed("org.freedesktop.systemd1"));
        assert!(activation_allowed("ca.desrt.dconf"));
        assert!(activation_allowed("org.gtk.vfs.Daemon"));
    }
}
