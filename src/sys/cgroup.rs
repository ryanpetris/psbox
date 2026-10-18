//! The supervisor's delegated cgroup subtree.
//!
//! Layout beneath the unit's cgroup:
//!
//! - `supervisor/`: the supervisor process itself (and pasta)
//! - `infra/init/`: the sandbox init
//! - `infra/bus/`: the private session bus and the services it starts
//! - `infra/display/`: xwayland-satellite and the title bar proxy
//! - `infra/connector/`: the host bus connector
//! - `workload/launch-<n>/`: everything one launch started
//!
//! Everything started from a sandbox's code (launches, the bus and the
//! services it starts, the display helpers) runs in a cgroup namespace
//! rooted at its own cgroup. With the cgroup2 `nsdelegate` mount option,
//! that keeps it from moving itself or a child into any other cgroup,
//! which is what makes a launch's cgroup tell which launch a process
//! belongs to. The sandbox is idle when `workload/` has no processes.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone)]
pub struct Cgroups {
    pub supervisor: PathBuf,
    pub infra: PathBuf,
    pub init: PathBuf,
    pub bus: PathBuf,
    pub display: PathBuf,
    pub connector: PathBuf,
    pub workload: PathBuf,
}

/// Whether the cgroup2 hierarchy at `/sys/fs/cgroup` is mounted with
/// `nsdelegate`, which makes a cgroup namespace a delegation boundary.
pub fn nsdelegate() -> Result<bool> {
    let mounts = fs::read_to_string("/proc/self/mounts").context("read /proc/self/mounts")?;
    nsdelegate_in(&mounts)
}

fn nsdelegate_in(mounts: &str) -> Result<bool> {
    let entry = mounts
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .find(|f| f.len() >= 4 && f[1] == "/sys/fs/cgroup" && f[2] == "cgroup2");
    match entry {
        Some(f) => Ok(f[3].split(',').any(|o| o == "nsdelegate")),
        None => bail!("cgroup v2 is not mounted at /sys/fs/cgroup"),
    }
}

impl Cgroups {
    /// Creates the subtree under the caller's cgroup and moves the caller
    /// into `supervisor/`. The caller's cgroup must be delegated
    /// (`Delegate=yes`).
    pub fn setup() -> Result<Self> {
        let own = fs::read_to_string("/proc/self/cgroup").context("read /proc/self/cgroup")?;
        let rel = own
            .lines()
            .find_map(|l| l.strip_prefix("0::"))
            .context("no cgroup v2 entry in /proc/self/cgroup")?;
        let base = Path::new("/sys/fs/cgroup").join(rel.trim_start_matches('/'));
        if base.file_name().and_then(|n| n.to_str()) == Some("supervisor") {
            bail!("already inside a psbox supervisor cgroup: {}", base.display());
        }
        let infra = base.join("infra");
        let cg = Self {
            supervisor: base.join("supervisor"),
            init: infra.join("init"),
            bus: infra.join("bus"),
            display: infra.join("display"),
            connector: infra.join("connector"),
            infra,
            workload: base.join("workload"),
        };
        for dir in [
            &cg.supervisor,
            &cg.infra,
            &cg.init,
            &cg.bus,
            &cg.display,
            &cg.connector,
            &cg.workload,
        ] {
            match fs::create_dir(dir) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!(
                            "create {} (run the supervisor in a delegated unit, e.g. systemd-run --user -p Delegate=yes)",
                            dir.display()
                        )
                    });
                }
            }
        }
        cg.add(&cg.supervisor, std::process::id() as i32)?;
        Ok(cg)
    }

    /// Moves `pid` into `dir`.
    pub fn add(&self, dir: &Path, pid: i32) -> Result<()> {
        fs::write(dir.join("cgroup.procs"), pid.to_string())
            .with_context(|| format!("move {pid} into {}", dir.display()))
    }

    /// The cgroup of launch `n`.
    pub fn launch(&self, n: u64) -> PathBuf {
        self.workload.join(format!("launch-{n}"))
    }

    /// The launch whose cgroup `cgroup` is, or is beneath.
    pub fn launch_of(&self, cgroup: &Path) -> Option<u64> {
        let first = cgroup.strip_prefix(&self.workload).ok()?.components().next()?;
        first.as_os_str().to_str()?.strip_prefix("launch-")?.parse().ok()
    }

    /// Creates a launch's cgroup.
    pub fn create_launch(&self, n: u64) -> Result<PathBuf> {
        let dir = self.launch(n);
        fs::create_dir(&dir).with_context(|| format!("create {}", dir.display()))?;
        Ok(dir)
    }

    /// Removes `dir` and the cgroups beneath it, deepest first. Fails while
    /// any of them has a process.
    pub fn remove(dir: &Path) -> io::Result<()> {
        for entry in fs::read_dir(dir)?.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                Self::remove(&entry.path())?;
            }
        }
        fs::remove_dir(dir)
    }

    /// Whether `dir` (or a descendant) has any process.
    pub fn populated(dir: &Path) -> bool {
        fs::read_to_string(dir.join("cgroup.events"))
            .map(|s| s.lines().any(|l| l == "populated 1"))
            .unwrap_or(false)
    }

    /// Process ids in `dir` and every cgroup beneath it.
    pub fn procs(dir: &Path) -> Vec<i32> {
        let mut out: Vec<i32> = fs::read_to_string(dir.join("cgroup.procs"))
            .map(|s| s.lines().filter_map(|l| l.parse().ok()).collect())
            .unwrap_or_default();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    out.extend(Self::procs(&entry.path()));
                }
            }
        }
        out
    }

    /// Kills every process in `dir` and its descendants.
    pub fn kill(dir: &Path) -> io::Result<()> {
        fs::write(dir.join("cgroup.kill"), "1")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_nsdelegate() {
        let with =
            "cgroup2 /sys/fs/cgroup cgroup2 rw,nosuid,nodev,noexec,relatime,nsdelegate,memory_recursiveprot 0 0\n";
        assert!(nsdelegate_in(with).unwrap());
        let without = "proc /proc proc rw 0 0\ncgroup2 /sys/fs/cgroup cgroup2 rw,nosuid 0 0\n";
        assert!(!nsdelegate_in(without).unwrap());
        assert!(nsdelegate_in("proc /proc proc rw 0 0\n").is_err());
    }

    #[test]
    fn launches_by_cgroup() {
        let cg = Cgroups {
            supervisor: "/cg/supervisor".into(),
            infra: "/cg/infra".into(),
            init: "/cg/infra/init".into(),
            bus: "/cg/infra/bus".into(),
            display: "/cg/infra/display".into(),
            connector: "/cg/infra/connector".into(),
            workload: "/cg/workload".into(),
        };
        assert_eq!(cg.launch_of(Path::new("/cg/workload/launch-7")), Some(7));
        assert_eq!(
            cg.launch_of(Path::new("/cg/workload/launch-7/made-by-the-app")),
            Some(7)
        );
        assert_eq!(cg.launch_of(Path::new("/cg/workload")), None);
        assert_eq!(cg.launch_of(Path::new("/cg/infra/bus")), None);
        assert_eq!(cg.launch_of(Path::new("/cg/workload/other")), None);
    }
}
