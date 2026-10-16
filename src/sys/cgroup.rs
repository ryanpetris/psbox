//! The supervisor's delegated cgroup subtree.
//!
//! Layout beneath the unit's cgroup:
//!
//! - `supervisor/`: the supervisor process itself
//! - `infra/`: the sandbox init and infrastructure (private bus, X server)
//! - `workload/`: everything started for a client
//!
//! The sandbox is idle when `workload/` has no processes.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone)]
pub struct Cgroups {
    pub supervisor: PathBuf,
    pub infra: PathBuf,
    pub workload: PathBuf,
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
        let cg = Self {
            supervisor: base.join("supervisor"),
            infra: base.join("infra"),
            workload: base.join("workload"),
        };
        for dir in [&cg.supervisor, &cg.infra, &cg.workload] {
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

    /// Whether `dir` (or a descendant) has any process.
    pub fn populated(dir: &Path) -> bool {
        fs::read_to_string(dir.join("cgroup.events"))
            .map(|s| s.lines().any(|l| l == "populated 1"))
            .unwrap_or(false)
    }

    /// Process ids currently in `dir`.
    pub fn procs(dir: &Path) -> Vec<i32> {
        fs::read_to_string(dir.join("cgroup.procs"))
            .map(|s| s.lines().filter_map(|l| l.parse().ok()).collect())
            .unwrap_or_default()
    }

    /// Kills every process in `dir` and its descendants.
    pub fn kill(dir: &Path) -> io::Result<()> {
        fs::write(dir.join("cgroup.kill"), "1")
    }
}
