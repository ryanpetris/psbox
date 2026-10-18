//! Client side: connect to (or start) a sandbox and run requests.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use crate::plan::{HostEnv, Identity, Plan};
use crate::protocol::{self, NoIdentity, Request, Response, Unidentified};
use crate::spec::Spec;
use crate::sys::cvt_int;
use crate::sys::fd::{Seqpacket, poll_readable};
use crate::systemd::{self, Systemd};
use crate::ui;
use crate::unnamed::Settings;

/// Connects to a running sandbox without starting it. The supervisor's
/// state comes from systemd: connecting to the socket alone would start it.
/// A running sandbox stays reachable when its config has since broken.
fn connect(host: &HostEnv, id: &Identity) -> Result<Seqpacket> {
    id.validate()?;
    if !Systemd::connect()?.is_active(&systemd::service_unit(&id.key()))? {
        check_config(host, id, false)?;
        bail!("{} isn't running", id.describe());
    }
    Ok(Seqpacket::connect(&host.socket_path(id))?)
}

/// Connects to the sandbox, starting its socket unit when needed; the
/// connection then activates the supervisor.
fn connect_or_start(host: &HostEnv, id: &Identity, create_dirs: bool) -> Result<Seqpacket> {
    id.validate()?;
    check_config(host, id, create_dirs)?;
    let path = host.socket_path(id);
    if let Ok(s) = Seqpacket::connect(&path) {
        return Ok(s);
    }
    let unit = systemd::socket_unit(&id.key());
    Systemd::connect()?.start(&unit).map_err(|e| {
        ui::with_fix(
            e.context(format!("couldn't start {unit}")),
            format!("{} shows what is missing", ui::code("psbox check")),
        )
    })?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match Seqpacket::connect(&path) {
            Ok(s) => return Ok(s),
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => bail!("{unit} is not listening ({e})"),
        }
    }
}

/// Named sandboxes need a valid config (and tasks a defined task) that
/// this host can run. The supervisor would fail without one too, but this
/// reports every problem directly, before anything starts.
fn check_config(host: &HostEnv, id: &Identity, create_dirs: bool) -> Result<()> {
    match id {
        Identity::Instance { sandbox, .. } | Identity::Task { sandbox, .. } => {
            let plan = Plan::build(id, &Spec::load(sandbox, host)?, host)?;
            preflight(&plan, id, create_dirs)
        }
        Identity::Unnamed { .. } => Ok(()),
    }
}

/// With `create_dirs`, creates the missing sources of mounts with `create`
/// outside the sandbox root too (the supervisor creates those inside it),
/// then checks that the sandbox can start.
fn preflight(plan: &Plan, id: &Identity, create_dirs: bool) -> Result<()> {
    if create_dirs {
        for dir in plan.create_dirs(true)? {
            ui::done(format!("created {}", ui::code(ui::tilde(dir))));
        }
    }
    plan.preflight(id, create_dirs)
}

/// Runs `argv` in a new unnamed sandbox described by `settings` (see
/// `unnamed.rs`) and returns the exit code to use. The sandbox and its socket
/// unit are gone when this returns.
pub fn sandbox(settings: &Settings, argv: Vec<String>, create_dirs: bool) -> Result<i32> {
    let host = HostEnv::from_process()?;
    let id = Identity::Unnamed { id: random_id()? };
    preflight(&Plan::build(&id, &settings.spec()?, &host)?, &id, create_dirs)?;
    if let Some(app_id) = &settings.app_id {
        use crate::identity::Probe;
        let dirs = crate::install::Dirs::for_host(&host);
        if let Some(by) = crate::install::claimant(app_id, crate::plan::UNNAMED, &dirs) {
            bail!("--app-id {app_id}: {app_id}.desktop belongs to the sandbox {by}");
        }
        match crate::identity::probe(app_id, crate::identity::STARTING)? {
            Probe::Accepted => {}
            Probe::Unknown => bail!(
                "the host portal doesn't accept {app_id}: there's no {app_id}.desktop in its applications directories"
            ),
            Probe::NoPortal => bail!("--app-id {app_id}: no portal runs on the host bus"),
        }
    }
    let body = serde_json::to_string(settings)?;
    let dir = host.socket_dir();
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .with_context(|| format!("create {}", dir.display()))?;
    let path = host.settings_path(&id);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("create {}", path.display()))?;
    if let Err(e) = file.write_all(body.as_bytes()) {
        let _ = fs::remove_file(&path);
        return Err(e).with_context(|| format!("write {}", path.display()));
    }
    drop(file);

    let r = run(&id, None, argv, true, false);
    let _ = fs::remove_file(&path);
    let _ = Systemd::connect().and_then(|s| s.stop(&systemd::socket_unit(&id.key())));
    r
}

fn random_id() -> Result<String> {
    let mut b = [0u8; 8];
    // SAFETY: getrandom fills the local buffer.
    let n = unsafe { libc::getrandom(b.as_mut_ptr().cast(), b.len(), 0) };
    if n != b.len() as isize {
        bail!("getrandom: {}", std::io::Error::last_os_error());
    }
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Receives a response from the supervisor. One that does not parse comes
/// from a supervisor still running an older psbox.
fn recv(conn: &Seqpacket, id: &Identity) -> Result<Option<(Response, Vec<OwnedFd>)>> {
    match protocol::recv::<Response>(conn) {
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => Err(ui::with_fix(
            anyhow!("{} is running an older version of psbox", id.describe()),
            format!("restart it with {}", ui::code(format!("psbox stop {}", id.cli_args()))),
        )),
        r => Ok(r?),
    }
}

/// Runs an app in the sandbox and returns the exit code to use.
pub fn run(id: &Identity, app: Option<String>, args: Vec<String>, exec: bool, create_dirs: bool) -> Result<i32> {
    let host = HostEnv::from_process()?;
    let conn = connect_or_start(&host, id, create_dirs)?;
    let env: BTreeMap<String, String> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect();
    // The request is JSON, so a directory that is not UTF-8 is not sent.
    let cwd = std::env::current_dir()
        .ok()
        .filter(|d| d.to_str().is_some())
        .unwrap_or_else(|| "/".into());
    let (stdin, stdout, stderr) = (std::io::stdin(), std::io::stdout(), std::io::stderr());
    let stdio = [stdin.as_fd(), stdout.as_fd(), stderr.as_fd()];
    protocol::send(
        &conn,
        &Request::Spawn(protocol::Spawn {
            app,
            args,
            env,
            cwd,
            exec,
        }),
        &stdio,
    )?;

    let sfd = forwarded_signals()?;
    loop {
        let ready = poll_readable(&[conn.as_fd(), sfd.as_fd()], -1)?;
        if ready[1] {
            let mut info = [0u8; 128];
            // SAFETY: reading one signalfd_siginfo into a local buffer.
            let n = unsafe { libc::read(sfd.as_raw_fd(), info.as_mut_ptr().cast(), info.len()) };
            if n == info.len() as isize {
                let signal = u32::from_ne_bytes(info[..4].try_into().expect("4 bytes")) as i32;
                protocol::send(&conn, &Request::Signal { signal }, &[])?;
            }
        }
        if ready[0] {
            match recv(&conn, id)? {
                Some((
                    Response::Spawned {
                        restart_pending,
                        network_fallback,
                        unidentified,
                        ..
                    },
                    _,
                )) => {
                    let who = ui::paint(ui::BOLD, id.describe());
                    if network_fallback {
                        warn_network_fallback(id);
                    }
                    if let Some(Unidentified { id: app_id, reason }) = unidentified {
                        ui::warning(format!(
                            "{who} runs without a portal identity {}",
                            ui::paint(
                                ui::DIM,
                                format!(
                                    "· {}, so file dialogs show only the sandbox and screen sharing, the camera portal, and printing are unavailable",
                                    reason.describe(&app_id)
                                )
                            )
                        ));
                        match reason {
                            NoIdentity::Unknown => ui::fix(format!(
                                "{} writes its desktop entry; then restart it with {}",
                                ui::code("psbox install"),
                                ui::code(format!("psbox stop {}", id.cli_args()))
                            )),
                            NoIdentity::Claimed { .. } => {
                                ui::fix(format!("give the app an {} of its own", ui::code("id")))
                            }
                            NoIdentity::NoPortal => {}
                        }
                    }
                    if !restart_pending.is_empty() {
                        ui::warning(format!(
                            "{who} has changes waiting for a restart {}",
                            ui::paint(ui::DIM, format!("· {}", restart_pending.join(", ")))
                        ));
                        ui::fix(format!(
                            "it restarts by itself when idle if they are still pending; {} restarts it now",
                            ui::code(format!("psbox stop {}", id.cli_args()))
                        ));
                    }
                }
                Some((Response::Exited { code, signal }, _)) => {
                    return Ok(code.unwrap_or_else(|| 128 + signal.unwrap_or(0)));
                }
                Some((Response::Error { message }, _)) => bail!("{message}"),
                Some((other, _)) => bail!("unexpected response {other:?}"),
                None => bail!("{} stopped while the command was running", id.describe()),
            }
        }
    }
}

/// Warns that a sandbox with the automatic network uses the host network
/// because pasta is not installed.
pub fn warn_network_fallback(id: &Identity) {
    ui::warning(format!(
        "{} is using the host network {}",
        ui::paint(ui::BOLD, id.describe()),
        ui::paint(ui::DIM, "· pasta isn't installed")
    ));
    ui::fix(format!(
        "install pasta for isolated networking, or {} to silence this",
        match id {
            Identity::Unnamed { .. } => format!("pass {}", ui::code("-c network.mode=host")),
            _ => format!("set {}", ui::code("network: { mode: 'host' }")),
        }
    ));
}

/// Blocks the signals forwarded to the sandboxed process and returns a
/// signalfd that reports them.
fn forwarded_signals() -> Result<OwnedFd> {
    // SAFETY: sigset manipulation on a local set; signalfd returns a new fd.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
            libc::sigaddset(&mut set, sig);
        }
        libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        let fd = cvt_int(libc::signalfd(-1, &set, libc::SFD_CLOEXEC))?;
        Ok(OwnedFd::from_raw_fd(fd))
    }
}

/// An error the supervisor answered a request with.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// Sends one request to a running sandbox and returns its response.
pub fn request(id: &Identity, req: Request) -> Result<Response> {
    let host = HostEnv::from_process()?;
    let conn = connect(&host, id)?;
    protocol::send(&conn, &req, &[])?;
    let (resp, _) = recv(&conn, id)?.ok_or_else(|| anyhow!("no response"))?;
    if let Response::Error { message } = resp {
        return Err(Refused(message).into());
    }
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreadable_response_means_an_older_supervisor() {
        let (a, b) = Seqpacket::pair().unwrap();
        // A reply from a version whose `Spawned` lacks a field this one has.
        a.send(
            br#"{"Spawned":{"pid":1,"restart_pending":[],"network_fallback":false}}"#,
            &[],
        )
        .unwrap();
        let id = Identity::Instance {
            sandbox: "firefox".into(),
            instance: None,
        };
        let e = recv(&b, &id).unwrap_err();
        assert_eq!(e.to_string(), "firefox is running an older version of psbox");
        let fixed = e.downcast_ref::<ui::Fixed>().unwrap();
        assert!(fixed.fix.contains("psbox stop firefox"));
    }
}
