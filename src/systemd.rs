//! The systemd user manager, over the session bus, and the host systemd
//! version.

use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::dbus::message::Message;
use crate::dbus::{Conn, Value};

const DEST: &str = "org.freedesktop.systemd1";
const PATH: &str = "/org/freedesktop/systemd1";
const MANAGER: &str = "org.freedesktop.systemd1.Manager";

pub struct Systemd(Conn);

/// The unit name prefix for psbox supervisors (`psbox@<key>`).
pub const UNIT_PREFIX: &str = "psbox@";

pub fn socket_unit(key: &str) -> String {
    format!("{UNIT_PREFIX}{key}.socket")
}

pub fn service_unit(key: &str) -> String {
    format!("{UNIT_PREFIX}{key}.service")
}

impl Systemd {
    /// Connects to the session bus the user manager is on.
    pub fn connect() -> Result<Self> {
        let address = match std::env::var("DBUS_SESSION_BUS_ADDRESS") {
            Ok(a) if !a.is_empty() => a,
            _ => {
                let rt = std::env::var("XDG_RUNTIME_DIR").context("no session bus: XDG_RUNTIME_DIR is not set")?;
                format!("unix:path={rt}/bus")
            }
        };
        Ok(Self(Conn::connect(&address).context("connect to the session bus")?))
    }

    fn call(&self, member: &str, args: &[Value]) -> Result<Message> {
        self.0
            .call(&Message::method_call(DEST, PATH, MANAGER, member, args))
            .with_context(|| format!("systemd {member}"))
    }

    pub fn start(&self, unit: &str) -> Result<()> {
        self.call("StartUnit", &[Value::Str(unit.into()), Value::Str("replace".into())])?;
        Ok(())
    }

    pub fn stop(&self, unit: &str) -> Result<()> {
        self.call("StopUnit", &[Value::Str(unit.into()), Value::Str("replace".into())])?;
        Ok(())
    }

    /// Names of active units matching `pattern`.
    pub fn active_units(&self, pattern: &str) -> Result<Vec<String>> {
        self.units(&["active"], pattern)
    }

    /// Names of units matching `pattern` in one of `states`.
    fn units(&self, states: &[&str], pattern: &str) -> Result<Vec<String>> {
        let reply = self.call(
            "ListUnitsByPatterns",
            &[
                Value::Array("s".into(), states.iter().map(|s| Value::Str(s.to_string())).collect()),
                Value::Array("s".into(), vec![Value::Str(pattern.into())]),
            ],
        )?;
        let args = reply.args()?;
        let Some(Value::Array(_, units)) = args.into_iter().next() else {
            return Ok(Vec::new());
        };
        Ok(units
            .into_iter()
            .filter_map(|u| match u {
                Value::Struct(fields) => fields.first().and_then(Value::as_str).map(str::to_string),
                _ => None,
            })
            .collect())
    }

    /// The state of a unit file (`static`, `enabled`, ...), or an error when
    /// systemd has no such unit file.
    pub fn unit_file_state(&self, unit: &str) -> Result<String> {
        let reply = self.call("GetUnitFileState", &[Value::Str(unit.into())])?;
        Ok(reply
            .args()?
            .first()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// Whether `unit` is starting, running, or stopping.
    pub fn is_up(&self, unit: &str) -> Result<bool> {
        let states = ["active", "activating", "deactivating", "reloading", "refreshing"];
        Ok(self.units(&states, unit)?.iter().any(|u| u == unit))
    }

    pub fn is_active(&self, unit: &str) -> Result<bool> {
        Ok(self.active_units(unit)?.iter().any(|u| u == unit))
    }
}

/// How long `systemctl --version` may take, counted from its start.
const VERSION_DEADLINE: Duration = Duration::from_millis(250);
/// Far more than `systemctl --version` prints.
const VERSION_LIMIT: usize = 16 * 1024;

/// The major version reported by `/usr/bin/systemctl --version`, if available.
pub fn version_major() -> Option<u32> {
    let out = bounded_output(
        Command::new("/usr/bin/systemctl").arg("--version"),
        VERSION_LIMIT,
        VERSION_DEADLINE,
    )?;
    parse_major(&out)
}

/// The leading number of the version on the first line of
/// `systemctl --version`, such as 257 in `systemd 257 (257.9-1-arch)`.
fn parse_major(out: &[u8]) -> Option<u32> {
    let line = std::str::from_utf8(out.split(|&b| b == b'\n').next()?).ok()?;
    let mut words = line.split_whitespace();
    if words.next()? != "systemd" {
        return None;
    }
    let major = words.next()?.split(['.', '-', '+', '~']).next()?;
    major.parse().ok()
}

/// Captures up to `limit` bytes and requires a successful exit within `timeout` of spawning.
fn bounded_output(cmd: &mut Command, limit: usize, timeout: Duration) -> Option<Vec<u8>> {
    let mut child = cmd
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    let mut stdout = child.stdout.take();
    let mut out = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }

        let Some(pipe) = stdout.as_mut() else {
            // A child can close stdout before exiting.
            match child.try_wait() {
                Ok(Some(status)) => return status.success().then_some(out),
                Ok(None) => std::thread::sleep(left.min(Duration::from_millis(5))),
                Err(_) => break,
            }
            continue;
        };
        let mut fd = libc::pollfd {
            fd: pipe.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: fd is a valid pollfd for the single open pipe.
        let ready = unsafe { libc::poll(&mut fd, 1, left.as_millis().max(1) as i32) };
        if ready <= 0 {
            if ready == 0 || io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        match pipe.read(&mut chunk) {
            Ok(0) => stdout = None,
            Ok(n) if out.len() + n <= limit => out.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Ok(_) | Err(_) => break,
        }
    }
    if matches!(child.try_wait(), Ok(None)) {
        let _ = child.kill();
        let _ = child.wait();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_major_takes_the_leading_number() {
        for (out, major) in [
            ("systemd 257 (257.9-1-arch)\n+PAM +AUDIT\n", Some(257)),
            ("systemd 252 (252.39-1~deb12u1)\n", Some(252)),
            ("systemd 249 (249.11-0ubuntu3.16)", Some(249)),
            ("systemd 255.4-1ubuntu8\n", Some(255)),
            ("systemd 258~rc2\n", Some(258)),
            ("systemd (unknown)\n", None),
            ("systemd v257\n", None),
            ("systemd 257invalid\n", None),
            ("systemd 99999999999\n", None),
            ("systemctl 257\n", None),
            ("\nsystemd 257\n", None),
            ("", None),
        ] {
            assert_eq!(parse_major(out.as_bytes()), major, "{out:?}");
        }
        assert_eq!(parse_major(b"systemd 251 (251)\n\xff\xfe\n"), Some(251));
    }

    #[test]
    fn bounded_output_reads_what_fits() {
        let out = bounded_output(
            Command::new("/usr/bin/echo").arg("systemd 251"),
            64,
            Duration::from_secs(5),
        );
        assert_eq!(out.as_deref(), Some(&b"systemd 251\n"[..]));
    }

    #[test]
    fn bounded_output_accepts_a_successful_exit_after_stdout_closes() {
        let out = bounded_output(
            Command::new("/bin/sh").args(["-c", "printf 'systemd 251\\n'; exec 1>&-; exec /usr/bin/sleep 0.05"]),
            64,
            Duration::from_secs(5),
        );
        assert_eq!(out.as_deref(), Some(&b"systemd 251\n"[..]));
    }

    #[test]
    fn bounded_output_rejects_output_from_a_failed_child() {
        let out = bounded_output(
            Command::new("/bin/sh").args(["-c", "printf 'systemd 251\\n'; exit 1"]),
            64,
            Duration::from_secs(5),
        );
        assert_eq!(out, None);
    }

    #[test]
    fn bounded_output_stops_a_stalled_child() {
        let start = Instant::now();
        assert_eq!(
            bounded_output(Command::new("/usr/bin/sleep").arg("30"), 64, Duration::from_millis(50)),
            None
        );
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn bounded_output_stops_a_stalled_child_after_stdout_closes() {
        let start = Instant::now();
        let out = bounded_output(
            Command::new("/bin/sh").args(["-c", "printf 'systemd 251\\n'; exec 1>&-; exec /usr/bin/sleep 30"]),
            64,
            Duration::from_millis(250),
        );
        assert_eq!(out, None);
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn bounded_output_stops_an_oversized_child() {
        assert_eq!(
            bounded_output(&mut Command::new("/usr/bin/yes"), 64, Duration::from_secs(5)),
            None
        );
    }
}
