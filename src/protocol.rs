//! Client/supervisor messages: one JSON object per `SOCK_SEQPACKET` packet.

use std::collections::BTreeMap;
use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::path::PathBuf;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::spec::{MountMode, Network};
use crate::sys::fd::Seqpacket;

pub const MAX_MESSAGE: usize = 64 * 1024;

/// A command to start. With `exec`, `args` is the whole command line; the
/// command only supplies environment and app id.
#[derive(Debug, Serialize, Deserialize)]
pub struct Spawn {
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// The client's working directory, used when the sandbox has it.
    pub cwd: PathBuf,
    pub exec: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Request {
    /// Start a command. Carries stdin, stdout, and stderr as descriptors.
    Spawn(Spawn),
    /// Deliver a signal to the process started on this connection.
    Signal {
        signal: i32,
    },
    /// Attach a host path into the running sandbox.
    Mount {
        host: String,
        at: String,
        mode: MountMode,
    },
    /// Detach a mount added with `Mount`.
    Umount {
        at: String,
    },
    /// Replace the overlays over these host directories with fresh ones,
    /// after a task wrote to them: an overlay does not reliably show changes
    /// made to its lower directory while it is mounted.
    Refresh {
        hosts: Vec<String>,
    },
    Status,
    Stop,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Response {
    /// `restart_pending` lists config changes waiting for a restart;
    /// `network_fallback` is set when the sandbox uses the host network
    /// only because pasta is not installed.
    Spawned {
        pid: i32,
        restart_pending: Vec<String>,
        network_fallback: bool,
    },
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    Done,
    Status(Status),
    Error {
        message: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Status {
    pub name: String,
    /// When the supervisor started, in seconds since the Unix epoch.
    pub started: u64,
    pub restart_pending: Vec<String>,
    pub init_pid: i32,
    pub network: Network,
    /// `network` is `host` only because pasta is not installed.
    pub network_fallback: bool,
    /// The X display number, when the sandbox has Xwayland.
    pub display: Option<u32>,
    /// Processes started for clients that are still running.
    pub processes: Vec<Process>,
    /// Every process in the sandbox's workload, including children.
    pub process_count: usize,
    pub clients: usize,
    pub mounts: Vec<LiveMount>,
    pub devices: Vec<PublishedDevice>,
}

/// A process started for a client (host pid).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Process {
    pub pid: i32,
    pub command: String,
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveMount {
    pub host: String,
    pub at: String,
    pub mode: MountMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedDevice {
    pub node: String,
    pub links: Vec<String>,
    pub classes: Vec<String>,
}

pub fn send<T: Serialize>(sock: &Seqpacket, msg: &T, fds: &[BorrowedFd<'_>]) -> io::Result<()> {
    let data = serde_json::to_vec(msg).map_err(io::Error::other)?;
    if data.len() > MAX_MESSAGE {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "message too large"));
    }
    sock.send(&data, fds)
}

/// Receives one message. `Ok(None)` means the peer closed the connection.
pub fn recv<T: DeserializeOwned>(sock: &Seqpacket) -> io::Result<Option<(T, Vec<OwnedFd>)>> {
    let mut buf = vec![0u8; MAX_MESSAGE];
    let Some((n, fds)) = sock.recv(&mut buf)? else {
        return Ok(None);
    };
    let msg = serde_json::from_slice(&buf[..n]).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some((msg, fds)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_fds() {
        let (a, b) = Seqpacket::pair().unwrap();
        let req = Request::Mount {
            host: "/h".into(),
            at: "/a".into(),
            mode: MountMode::Ro,
        };
        use std::os::fd::AsFd;
        send(&a, &req, &[a.as_fd()]).unwrap();
        let (got, fds): (Request, _) = recv(&b).unwrap().unwrap();
        assert!(matches!(
            got,
            Request::Mount {
                mode: MountMode::Ro,
                ..
            }
        ));
        assert_eq!(fds.len(), 1);
        // The received descriptor is another reference to `a`.
        drop(fds);
        drop(a);
        assert!(recv::<Request>(&b).unwrap().is_none());
    }
}
