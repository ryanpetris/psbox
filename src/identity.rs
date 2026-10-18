//! Portal identity: the app id the D-Bus gateway registers a sandboxed
//! process under with the host portal.
//!
//! The host portal accepts an id only when it finds an application entry
//! with that id (`org.freedesktop.host.portal.Registry.Register`), so psbox
//! asks it directly when a launch starts, on a connection of its own, and
//! the answer decides both the launch's identity and its environment. Each
//! launch runs in its own cgroup (see `sys/cgroup.rs`), so the gateway finds
//! which launch a connection belongs to from the connecting process's
//! cgroup, however it forked.

use std::os::fd::{AsRawFd, BorrowedFd};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

use crate::dbus::message::Message;
use crate::dbus::{Conn, Value};
use crate::portal::{PORTAL_NAME, PORTAL_PATH, REGISTRY_IFACE};

/// What the host portal says about an app id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// It registered the id.
    Accepted,
    /// It has no application entry with this id.
    Unknown,
    /// No portal runs on the host bus, so nothing can be registered.
    NoPortal,
}

/// How long psbox waits for a portal that is starting (D-Bus activation's
/// own timeout) before a launch fails.
pub const STARTING: Duration = Duration::from_secs(25);

/// Classifies a failed `Register`: `None` for an error that may pass (the
/// portal still starting), retried while the probe may wait.
fn classify(error: &str) -> Option<Probe> {
    if error.contains("App info not found") {
        Some(Probe::Unknown)
    } else if error.contains("org.freedesktop.DBus.Error.ServiceUnknown")
        || error.contains("org.freedesktop.DBus.Error.NameHasNoOwner")
        || error.contains("org.freedesktop.DBus.Error.UnknownInterface")
        || error.contains("org.freedesktop.DBus.Error.UnknownMethod")
    {
        Some(Probe::NoPortal)
    } else {
        None
    }
}

/// Asks the host portal whether it accepts `app_id`, on a connection opened
/// for the question and closed after it, retrying errors that may pass for
/// up to `wait`. No answer within `wait` is an error.
pub fn probe(app_id: &str, wait: Duration) -> Result<Probe> {
    let deadline = Instant::now() + wait;
    let (tx, rx) = std::sync::mpsc::channel();
    let id = app_id.to_string();
    // The attempts run on a thread of their own, so the deadline holds
    // however the connection stalls. Connecting and each read and write on
    // the connection time out, and calls stop at the deadline, so the thread
    // ends soon after it, unless the host bus keeps sending a message a
    // little at a time.
    std::thread::Builder::new()
        .name("portal-probe".into())
        .spawn(move || {
            let _ = tx.send(attempts(&id, deadline));
        })
        .context("start the portal probe")?;
    match rx.recv_timeout(wait) {
        Ok(answer) => answer,
        Err(_) => Err(anyhow!(
            "the host portal didn't answer for {app_id} within {:.1} s",
            wait.as_secs_f64()
        )),
    }
}

fn attempts(app_id: &str, deadline: Instant) -> Result<Probe> {
    let register = Message::method_call(
        PORTAL_NAME,
        PORTAL_PATH,
        REGISTRY_IFACE,
        "Register",
        &[Value::Str(app_id.into()), Value::Dict("s".into(), "v".into(), vec![])],
    );
    let mut error = String::from("no time to ask");
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(anyhow!("the host portal didn't answer for {app_id}: {error}"));
        }
        let address = crate::gateway::host_bus_address()?;
        let bus = Conn::connect_timeout(&address, left).map_err(|e| anyhow!("connect to the host bus: {e}"))?;
        error = match bus.call(&register) {
            Ok(_) => return Ok(Probe::Accepted),
            Err(e) => e.to_string(),
        };
        if let Some(answer) = classify(&error) {
            return Ok(answer);
        }
        std::thread::sleep(Duration::from_millis(500).min(deadline.saturating_duration_since(Instant::now())));
    }
}

/// The host pid of the process behind `pidfd`, while it runs.
fn pidfd_pid(pidfd: BorrowedFd<'_>) -> Option<i32> {
    let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", pidfd.as_raw_fd())).ok()?;
    let pid: i32 = info.lines().find_map(|l| l.strip_prefix("Pid:"))?.trim().parse().ok()?;
    (pid > 0).then_some(pid)
}

/// The cgroup (as a path beneath `/sys/fs/cgroup`) of the process behind
/// `pidfd`. Read through the process's pid, then checked again, so a pid
/// reused meanwhile is never taken for it.
pub fn process_cgroup(pidfd: BorrowedFd<'_>) -> Option<PathBuf> {
    let pid = pidfd_pid(pidfd)?;
    let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let rel = text.lines().find_map(|l| l.strip_prefix("0::"))?;
    // The process still runs, so `pid` was it when its cgroup was read.
    (pidfd_pid(pidfd) == Some(pid)).then(|| PathBuf::from("/sys/fs/cgroup").join(rel.trim_start_matches('/')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_errors() {
        assert_eq!(
            classify("org.freedesktop.portal.Error.Failed: Could not register app ID: App info not found for 'x'"),
            Some(Probe::Unknown)
        );
        assert_eq!(
            classify("org.freedesktop.DBus.Error.ServiceUnknown: The name is not activatable"),
            Some(Probe::NoPortal)
        );
        assert_eq!(classify("org.freedesktop.DBus.Error.NoReply: timeout"), None);
    }

    #[test]
    fn reads_its_own_cgroup_through_a_pidfd() {
        use std::os::fd::{AsFd, FromRawFd, OwnedFd};
        // SAFETY: plain syscall; a successful one returns a new descriptor
        // nothing else owns.
        let pidfd = unsafe {
            let fd = libc::syscall(libc::SYS_pidfd_open, std::process::id() as i32, 0);
            assert!(fd >= 0, "pidfd_open");
            OwnedFd::from_raw_fd(fd as i32)
        };
        let cg = process_cgroup(pidfd.as_fd()).unwrap();
        assert!(cg.starts_with("/sys/fs/cgroup"));
        let own = std::fs::read_to_string("/proc/self/cgroup").unwrap();
        let rel = own.lines().find_map(|l| l.strip_prefix("0::")).unwrap();
        assert_eq!(cg, PathBuf::from("/sys/fs/cgroup").join(rel.trim_start_matches('/')));
    }
}
