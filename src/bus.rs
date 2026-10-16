//! Files for a sandbox's private session bus.
//!
//! The supervisor writes them into the sandbox runtime directory through a
//! descriptor for the sandbox root, never following a symlink inside it,
//! then starts `dbus-daemon` inside the sandbox.

use std::fs;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::portal::activation_allowed;
use crate::sys::mount::{PointKind, make_point, symlink_at, write_at};

const TEMPLATE: &str = include_str!("assets/session-bus.conf");

/// Host directories whose services may be activated in the sandbox.
const SERVICE_DIRS: [&str; 2] = ["/usr/share/dbus-1/services", "/usr/local/share/dbus-1/services"];

/// Paths of the bus as seen inside the sandbox.
#[derive(Debug, Clone)]
pub struct BusPaths {
    pub config: PathBuf,
    pub socket: PathBuf,
}

impl BusPaths {
    pub fn new(runtime_dir: &Path) -> Self {
        Self {
            config: runtime_dir.join("psbox/dbus/session.conf"),
            socket: runtime_dir.join("bus"),
        }
    }

    /// The D-Bus address clients in the sandbox use.
    pub fn address(&self) -> String {
        format!("unix:path={}", self.socket.display())
    }
}

/// A sandbox path relative to the sandbox root.
pub fn in_root(sandbox_path: &Path) -> &Path {
    sandbox_path.strip_prefix("/").unwrap_or(sandbox_path)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The `Name=` of a D-Bus `.service` file.
fn service_name(text: &str) -> Option<&str> {
    text.lines().find_map(|l| l.trim().strip_prefix("Name=")).map(str::trim)
}

/// Writes the configuration and the filtered service directory, through
/// `root`, a descriptor for the sandbox root.
pub fn prepare(root: BorrowedFd<'_>, paths: &BusPaths, home: &Path) -> Result<()> {
    let config_dir = paths.config.parent().expect("config has a parent");
    let services = config_dir.join("services");
    let services_dir = make_point(root, in_root(&services), PointKind::Dir)
        .with_context(|| format!("create {}", services.display()))?;
    for dir in SERVICE_DIRS {
        let Ok(entries) = fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("service") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else { continue };
            if service_name(&text).is_some_and(activation_allowed) {
                symlink_at(&path, services_dir.as_fd(), &entry.file_name())
                    .with_context(|| format!("link {}", services.join(entry.file_name()).display()))?;
            }
        }
    }
    let config = TEMPLATE
        .replace("@SOCKET@", &xml_escape(&paths.socket.to_string_lossy()))
        .replace("@SERVICES@", &xml_escape(&services.to_string_lossy()))
        .replace(
            "@HOME_SERVICES@",
            &xml_escape(&home.join(".local/share/dbus-1/services").to_string_lossy()),
        );
    let dir = make_point(root, in_root(config_dir), PointKind::Dir).context("create the bus config directory")?;
    let name = paths
        .config
        .file_name()
        .and_then(|n| n.to_str())
        .expect("config has a file name");
    write_at(dir.as_fd(), name, config.as_bytes()).context("write bus config")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_service_names() {
        assert_eq!(
            service_name("[D-BUS Service]\nName=ca.desrt.dconf\nExec=/usr/lib/dconf-service\n"),
            Some("ca.desrt.dconf")
        );
        assert_eq!(service_name("[D-BUS Service]\nExec=x\n"), None);
    }

    #[test]
    fn prepares_filtered_services_and_config() {
        let view = std::env::temp_dir().join(format!("psbox-bus-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&view);
        fs::create_dir_all(&view).unwrap();
        let paths = BusPaths::new(Path::new("/run/user/1000"));
        let root = crate::sys::mount::open_dir(&view).unwrap();
        prepare(root.as_fd(), &paths, Path::new("/home/u")).unwrap();
        let config = fs::read_to_string(view.join(in_root(&paths.config))).unwrap();
        assert!(config.contains("<listen>unix:path=/run/user/1000/bus</listen>"));
        assert!(config.contains("<servicedir>/run/user/1000/psbox/dbus/services</servicedir>"));
        assert!(!config.contains("standard_session_servicedirs"));
        for entry in fs::read_dir(view.join("run/user/1000/psbox/dbus/services"))
            .unwrap()
            .flatten()
        {
            let text = fs::read_to_string(entry.path()).unwrap();
            assert!(
                activation_allowed(service_name(&text).unwrap()),
                "{:?}",
                entry.file_name()
            );
        }
        assert_eq!(paths.address(), "unix:path=/run/user/1000/bus");

        // A symlink planted where the config goes is refused, not followed.
        let planted = std::env::temp_dir().join(format!("psbox-bus-planted-{}", std::process::id()));
        let _ = fs::remove_dir_all(&planted);
        fs::create_dir_all(planted.join("run/user")).unwrap();
        std::os::unix::fs::symlink(&view, planted.join("run/user/1000")).unwrap();
        let root = crate::sys::mount::open_dir(&planted).unwrap();
        assert!(prepare(root.as_fd(), &paths, Path::new("/home/u")).is_err());
        fs::remove_dir_all(&planted).unwrap();
        fs::remove_dir_all(&view).unwrap();
    }
}
