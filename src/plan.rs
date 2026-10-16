//! Resolves a [`Spec`] against the host into a concrete sandbox plan.
//!
//! Building a plan performs no syscalls beyond reading the environment, so it
//! is unit tested directly.

use std::collections::BTreeMap;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::bus::BusPaths;
use crate::spec::{Cache, Dbus, DeviceClass, Home, MountMode, Network, Spec, Tokens};

/// Host facts the plan depends on.
#[derive(Debug, Clone)]
pub struct HostEnv {
    pub uid: u32,
    pub gid: u32,
    pub user: String,
    pub home: PathBuf,
    pub runtime_dir: PathBuf,
    /// Wayland socket path on the host, if any.
    pub wayland_socket: Option<PathBuf>,
    /// The host session bus socket (see [`crate::gateway::host_bus_address`]),
    /// when that is a `unix:path=` address.
    pub session_bus: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME/psbox`.
    pub config_dir: PathBuf,
    /// The sandbox root (`$PSBOX_ROOT`, default `~/Sandbox`).
    pub root: PathBuf,
    /// Whether `pasta` is on `PATH` (isolated networking needs it).
    pub pasta: bool,
}

/// The first executable called `name` on `PATH`.
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(name))
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// The network a sandbox gets for its `network` setting, and whether that
/// is `host` only because pasta is missing. An unset `network` means
/// `isolated` when pasta is installed and `host` otherwise; `None` when
/// `isolated` was set explicitly and pasta is missing.
pub fn resolve_network(setting: Option<Network>, pasta: bool) -> Option<(Network, bool)> {
    match setting {
        Some(Network::Isolated) if !pasta => None,
        Some(n) => Some((n, false)),
        None if pasta => Some((Network::Isolated, false)),
        None => Some((Network::Host, true)),
    }
}

/// The socket path of the first `unix:path=` entry in a D-Bus address.
fn unix_path(address: &str) -> Option<PathBuf> {
    address
        .split(';')
        .filter_map(|entry| entry.strip_prefix("unix:"))
        .flat_map(|params| params.split(','))
        .find_map(|kv| kv.strip_prefix("path="))
        .map(|v| PathBuf::from(std::ffi::OsString::from_vec(crate::dbus::conn::unescape(v))))
}

impl HostEnv {
    pub fn from_process() -> Result<Self> {
        // SAFETY: getuid/getgid cannot fail.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = var("HOME").context("HOME is not set")?;
        let runtime_dir = var("XDG_RUNTIME_DIR").unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}")));
        let wayland_socket = match std::env::var("WAYLAND_DISPLAY") {
            Ok(v) if v.starts_with('/') => Some(PathBuf::from(v)),
            Ok(v) if !v.is_empty() => Some(runtime_dir.join(v)),
            _ => Some(runtime_dir.join("wayland-0")).filter(|p| p.exists()),
        };
        let config_dir = var("XDG_CONFIG_HOME")
            .unwrap_or_else(|| home.join(".config"))
            .join("psbox");
        let root = var("PSBOX_ROOT").unwrap_or_else(|| home.join("Sandbox"));
        let user = std::env::var("USER").unwrap_or_else(|_| uid.to_string());
        let session_bus = crate::gateway::host_bus_address().ok().and_then(|a| unix_path(&a));
        Ok(Self {
            uid,
            gid,
            user,
            home,
            runtime_dir,
            wayland_socket,
            session_bus,
            config_dir,
            root,
            pasta: find_in_path("pasta").is_some(),
        })
    }

    /// The per-user directory for supervisor sockets.
    pub fn socket_dir(&self) -> PathBuf {
        self.runtime_dir.join("psbox")
    }

    pub fn socket_path(&self, id: &Identity) -> PathBuf {
        self.socket_dir().join(format!("{}.sock", id.key()))
    }

    /// Where `psbox sandbox` leaves an unnamed sandbox's settings for its
    /// supervisor.
    pub fn settings_path(&self, id: &Identity) -> PathBuf {
        self.socket_dir().join(format!("{}.toml", id.key()))
    }
}

/// Which supervisor: a sandbox instance (the default or a named one), a
/// task run, or an unnamed sandbox from `psbox sandbox`. Instances of a
/// sandbox share its home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    Instance {
        sandbox: String,
        instance: Option<String>,
    },
    Task {
        sandbox: String,
        task: String,
    },
    /// Settings come from [`HostEnv::settings_path`]; `id` is 16 hex digits.
    Unnamed {
        id: String,
    },
}

/// The sandbox name unnamed sandboxes use for their key and default command.
pub const UNNAMED: &str = "sandbox";

impl Identity {
    /// How messages name the sandbox: `firefox`, `firefox (instance work)`.
    pub fn describe(&self) -> String {
        match self {
            Identity::Instance {
                sandbox,
                instance: None,
            } => sandbox.clone(),
            Identity::Instance {
                sandbox,
                instance: Some(i),
            } => format!("{sandbox} (instance {i})"),
            Identity::Task { sandbox, task } => format!("{sandbox} (task {task})"),
            Identity::Unnamed { id } => format!("unnamed sandbox {id}"),
        }
    }

    /// The arguments that address this sandbox on the command line, such as
    /// `firefox -i work`.
    pub fn cli_args(&self) -> String {
        match self {
            Identity::Instance {
                sandbox,
                instance: None,
            } => sandbox.clone(),
            Identity::Instance {
                sandbox,
                instance: Some(i),
            } => format!("{sandbox} -i {i}"),
            Identity::Task { sandbox, task } => format!("{sandbox} {task}"),
            Identity::Unnamed { id } => format!("sandbox.{id}"),
        }
    }

    pub fn sandbox(&self) -> &str {
        match self {
            Identity::Instance { sandbox, .. } | Identity::Task { sandbox, .. } => sandbox,
            Identity::Unnamed { .. } => UNNAMED,
        }
    }

    /// The name used for sockets, units, and logs. Sandbox names never
    /// contain `.`, so the forms cannot collide.
    pub fn key(&self) -> String {
        match self {
            Identity::Instance {
                sandbox,
                instance: None,
            } => sandbox.clone(),
            Identity::Instance {
                sandbox,
                instance: Some(i),
            } => format!("{sandbox}.instance-{i}"),
            Identity::Task { sandbox, task } => format!("{sandbox}.task-{task}"),
            Identity::Unnamed { id } => format!("{UNNAMED}.{id}"),
        }
    }

    /// Parses a key made by [`Identity::key`].
    pub fn from_key(key: &str) -> Result<Self> {
        let id = match key.split_once('.') {
            None => Identity::Instance {
                sandbox: key.into(),
                instance: None,
            },
            Some((sandbox, rest)) => {
                if let Some(i) = rest.strip_prefix("instance-") {
                    Identity::Instance {
                        sandbox: sandbox.into(),
                        instance: Some(i.into()),
                    }
                } else if let Some(t) = rest.strip_prefix("task-") {
                    Identity::Task {
                        sandbox: sandbox.into(),
                        task: t.into(),
                    }
                } else if sandbox == UNNAMED {
                    Identity::Unnamed { id: rest.into() }
                } else {
                    bail!("invalid supervisor key {key:?}");
                }
            }
        };
        id.validate()?;
        Ok(id)
    }

    /// Validates the sandbox, instance, and task names.
    pub fn validate(&self) -> Result<()> {
        // Unnamed sandboxes use the name reserved for them.
        if !matches!(self, Identity::Unnamed { .. }) {
            crate::spec::validate_sandbox_name(self.sandbox())?;
        }
        let extra = match self {
            Identity::Instance { instance: Some(i), .. } => Some(("instance", i)),
            Identity::Task { task, .. } => Some(("task", task)),
            Identity::Instance { instance: None, .. } => None,
            Identity::Unnamed { id } => {
                if id.len() != 16 || !id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) {
                    bail!("invalid unnamed sandbox id {id:?}");
                }
                None
            }
        };
        if let Some((kind, v)) = extra
            && (v.is_empty() || !v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')))
        {
            bail!("invalid {kind} name {v:?}: use letters, digits, '_', '-'");
        }
        Ok(())
    }
}

/// One mount in the sandbox root, applied in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mount {
    Bind {
        src: PathBuf,
        dst: PathBuf,
        ro: bool,
        optional: bool,
    },
    Tmpfs {
        dst: PathBuf,
        mode: u32,
    },
    Overlay {
        lower: PathBuf,
        dst: PathBuf,
        optional: bool,
    },
    Symlink {
        target: String,
        dst: PathBuf,
    },
    Dir {
        dst: PathBuf,
    },
    Proc {
        dst: PathBuf,
    },
    Dev {
        dst: PathBuf,
    },
    /// A read-only file with this content (following symlinks within the
    /// sandbox root to find where it goes).
    File {
        dst: PathBuf,
        content: String,
    },
}

impl Mount {
    pub fn dst(&self) -> &Path {
        match self {
            Mount::Bind { dst, .. }
            | Mount::Tmpfs { dst, .. }
            | Mount::Overlay { dst, .. }
            | Mount::Symlink { dst, .. }
            | Mount::Dir { dst }
            | Mount::Proc { dst }
            | Mount::Dev { dst }
            | Mount::File { dst, .. } => dst,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub exec: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub app_id: Option<String>,
}

/// A host path the config shares, kept to report it by its config entry
/// when it does not exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub path: PathBuf,
    /// `rw`, `ro`, or a `[mount.<name>]` entry.
    pub entry: SourceEntry,
    /// Written with a trailing `/`: a directory, which psbox creates when
    /// it is missing (see [`Plan::create_dirs`]).
    pub dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceEntry {
    Rw,
    Ro,
    Mount(String),
}

/// Something that stops a sandbox from starting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// A shared host path that does not exist (`dir` when it is marked as
    /// a directory with a trailing `/`).
    Missing {
        path: PathBuf,
        entry: SourceEntry,
        dir: bool,
    },
    /// A setting this host cannot provide: what it needs, and why not.
    Setting {
        setting: String,
        needs: String,
        detail: String,
    },
    /// A shared host path that is a symlink to a path that does not exist,
    /// or into a loop of symlinks.
    BrokenLink {
        path: PathBuf,
        target: PathBuf,
        entry: SourceEntry,
        looped: bool,
    },
    /// A mount point inside a read-only bind whose host directory lacks it.
    MountPoint {
        dst: PathBuf,
        read_only: PathBuf,
        host: PathBuf,
    },
    /// A config entry psbox cannot plan.
    Config { entry: String, message: String },
    /// A kernel feature psbox needs.
    System { message: String },
}

/// How the config entry behind a share is named to the user: `rw`, `ro`,
/// `[mount.<name>]`, or the `psbox sandbox` flag that made it.
fn entry_label(entry: &SourceEntry, unnamed: bool) -> String {
    match entry {
        SourceEntry::Rw => "rw".into(),
        SourceEntry::Ro => "ro".into(),
        SourceEntry::Mount(name) if unnamed && name.starts_with("ro-bind-") => "--ro-bind".into(),
        SourceEntry::Mount(_) if unnamed => "--bind".into(),
        SourceEntry::Mount(name) => format!("[mount.{name}]"),
    }
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub name: String,
    /// The supervisor key ([`Identity::key`]).
    pub key: String,
    /// Host directory backing the private home, created before start.
    pub host_home_dir: Option<PathBuf>,
    /// `$HOME` inside the sandbox.
    pub home: PathBuf,
    pub mounts: Vec<Mount>,
    /// Classes published into `/dev`.
    pub node_classes: Vec<DeviceClass>,
    /// The `audio` class: the host's audio sockets, and the Realtime portal.
    pub audio: bool,
    pub env: BTreeMap<String, String>,
    pub commands: BTreeMap<String, Command>,
    pub idle_timeout: Duration,
    pub dbus: Dbus,
    /// The private session bus, when the sandbox has one.
    pub bus: Option<BusPaths>,
    /// Host bus names the gateway forwards calls to (`dbus-talk`); empty
    /// without a private bus.
    pub dbus_talk: Vec<String>,
    /// Serve X11 clients with an on-demand Xwayland.
    pub xwayland: bool,
    pub network: Network,
    /// `network` is `host` only because pasta is not installed.
    pub network_fallback: bool,
    /// App id for processes not started by a command with its own.
    pub app_id: Option<String>,
    /// The sandbox's `$XDG_RUNTIME_DIR`.
    pub runtime_dir: PathBuf,
    /// Host paths the config shares and does not mark optional.
    pub sources: Vec<Source>,
    /// The sandbox root (`$PSBOX_ROOT`), where missing directories are
    /// created without asking.
    pub sandbox_root: PathBuf,
    /// Problems found while planning that stop the sandbox from starting
    /// (see [`Plan::preflight`]).
    pub problems: Vec<Problem>,
}

const DEFAULT_PATH: &str = "/usr/local/bin:/usr/bin";

/// The address pasta answers DNS on inside an isolated sandbox.
pub const PASTA_DNS: &str = "169.254.1.1";

/// How a running sandbox gets from one plan to another.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Change {
    /// Reasons the new plan needs a restart. When non-empty nothing is
    /// applied live.
    pub restart: Vec<String>,
    /// Mounts to detach (in this order).
    pub detach: Vec<Mount>,
    /// Mounts to attach (in this order).
    pub attach: Vec<Mount>,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        self.restart.is_empty() && self.detach.is_empty() && self.attach.is_empty()
    }
}

/// Mounts that can be attached and detached in a running sandbox. The X
/// socket directory is not: the X socket listens on that tmpfs for the
/// sandbox's lifetime.
fn live_mount(m: &Mount) -> bool {
    matches!(m, Mount::Bind { .. } | Mount::Tmpfs { .. }) && m.dst() != Path::new(crate::x11::SOCKET_DIR)
}

/// Where `path` points when it is a symlink to a path that does not exist,
/// and whether it is a loop of symlinks instead.
fn broken_link(path: &Path) -> Option<(PathBuf, bool)> {
    let link = std::fs::read_link(path).ok()?;
    let looped = match std::fs::metadata(path) {
        Ok(_) => return None,
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => true,
        Err(_) if missing(path) => false,
        Err(_) => return None,
    };
    // Resolved lexically, for showing where it points.
    let joined = path.parent().map_or(link.clone(), |parent| parent.join(&link));
    let mut target = PathBuf::new();
    for c in joined.components() {
        match c {
            std::path::Component::ParentDir => {
                target.pop();
            }
            std::path::Component::CurDir => {}
            other => target.push(other),
        }
    }
    Some((target, looped))
}

/// Whether `path` does not exist on the host (following symlinks).
fn missing(path: &Path) -> bool {
    std::fs::metadata(path).is_err_and(|e| {
        matches!(
            e.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
        )
    })
}

/// Everything that stops a sandbox from starting, by section, with the
/// issues that would not stop it on their own.
#[derive(Debug)]
pub struct Report {
    who: String,
    unnamed: bool,
    /// The sandbox root, for saying where directories are created.
    root: PathBuf,
    /// Run with `--create-dirs`, which only creates paths ending in `/`.
    create_dirs: bool,
    problems: Vec<Problem>,
    /// Issues that do not stop the sandbox, as (what, why), shown along so
    /// that they are not discovered one start later.
    notes: Vec<(String, String)>,
}

impl Report {
    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }

    /// The report styled for the terminal.
    pub fn render(&self) -> String {
        use crate::ui::{BOLD, CYAN, Cell, DIM, Table, YELLOW, code, fix_line, paint, tilde};
        let total = self.problems.len();
        let all_missing = self.problems.iter().all(|p| matches!(p, Problem::Missing { .. }));
        let mut out = crate::ui::failure(format!("{} can't start", paint(BOLD, &self.who)));
        if total > 1 {
            let summary = match all_missing {
                true => format!("{total} paths are missing"),
                false => format!("{total} problems"),
            };
            out.push_str(&format!(" {}", paint(DIM, format!("· {summary}"))));
        }
        out.push('\n');
        let mut section = |table: Table, fixes: Vec<String>| {
            if table.is_empty() {
                return;
            }
            out.push('\n');
            for line in table.render(None) {
                out.push_str(&format!("{line}\n"));
            }
            for fix in fixes {
                out.push_str(&format!("{}\n", fix_line(fix)));
            }
        };

        // Missing paths, sorted so that paths inside another missing one
        // follow it and are shown relative to it.
        let mut missing: Vec<(&PathBuf, &SourceEntry, bool)> = self
            .problems
            .iter()
            .filter_map(|p| match p {
                Problem::Missing { path, entry, dir } => Some((path, entry, *dir)),
                _ => None,
            })
            .collect();
        missing.sort_by(|a, b| a.0.cmp(b.0));
        let mut table = Table::new(&["MISSING ON THE HOST", "USED BY"]);
        for (i, (path, entry, dir)) in missing.iter().enumerate() {
            let parent = missing[..i]
                .iter()
                .map(|(p, _, _)| p)
                .filter(|p| path.starts_with(p) && path != *p)
                .max_by_key(|p| p.components().count());
            let slash = if *dir { "/" } else { "" };
            let shown = match parent {
                Some(parent) => Cell::new("  ").then(
                    CYAN,
                    format!("{}{slash}", path.strip_prefix(parent).expect("inside").display()),
                ),
                None => Cell::styled(CYAN, format!("{}{slash}", tilde(path))),
            };
            table.row(vec![shown, Cell::new(entry_label(entry, self.unnamed))]);
        }
        let mut fixes = Vec::new();
        for list in ["rw", "ro"] {
            let paths: Vec<String> = missing
                .iter()
                .filter(|(_, e, _)| entry_label(e, self.unnamed) == list)
                .map(|(p, _, _)| code(tilde(p)))
                .collect();
            match paths.len() {
                0 => {}
                1 | 2 => fixes.push(format!("remove {} from {list}", paths.join(" and "))),
                _ => fixes.push(format!("remove the missing paths from {list}")),
            }
        }
        let mounts: Vec<String> = missing
            .iter()
            .filter(|(_, e, _)| matches!(e, SourceEntry::Mount(_)) && !self.unnamed)
            .map(|(_, e, _)| entry_label(e, false))
            .collect();
        let optional = code("optional = true");
        match mounts.len() {
            0 => {}
            1 => fixes.push(format!("add {optional} to {} if it can be missing", mounts[0])),
            2 => fixes.push(format!(
                "add {optional} to {} if they can be missing",
                mounts.join(" and ")
            )),
            _ => fixes.push(format!("add {optional} to the mounts that can be missing")),
        }
        let create_dirs = code("--create-dirs");
        let root = tilde(&self.root);
        match (
            missing.iter().any(|(_, _, dir)| !dir),
            missing.iter().any(|(p, _, dir)| !dir && !p.starts_with(&self.root)),
        ) {
            (false, _) => {}
            (true, _) if self.create_dirs => fixes.push(format!(
                "{create_dirs} only creates directories that end with /: add the / to those above to have them created"
            )),
            (true, false) => fixes.push(format!("end directories with / to have psbox create them in {root}")),
            (true, true) => fixes.push(format!(
                "end directories with / to have psbox create them in {root}, and elsewhere with {create_dirs}"
            )),
        }
        if missing.iter().any(|(_, _, dir)| *dir) {
            fixes.push(format!("run with {create_dirs} to create the ones ending in /"));
        }
        section(table, fixes);

        let mut table = Table::new(&["BROKEN SYMLINK", "POINTS TO", "USED BY"]);
        for p in &self.problems {
            if let Problem::BrokenLink {
                path,
                target,
                entry,
                looped,
            } = p
            {
                let why = if *looped {
                    " · a loop of symlinks"
                } else {
                    " · doesn't exist"
                };
                table.row(vec![
                    Cell::styled(CYAN, tilde(path)),
                    Cell::styled(CYAN, tilde(target)).then(DIM, why),
                    Cell::new(entry_label(entry, self.unnamed)),
                ]);
            }
        }
        section(table, Vec::new());

        let mut table = Table::new(&["SETTING", "NEEDS"]);
        for p in &self.problems {
            if let Problem::Setting { setting, needs, detail } = p {
                let mut cell = Cell::new(needs.clone());
                if !detail.is_empty() {
                    cell = cell.then(DIM, format!(" · {detail}"));
                }
                table.row(vec![Cell::styled(CYAN, setting.clone()), cell]);
            }
        }
        section(table, Vec::new());

        let mut table = Table::new(&["MOUNT POINT", "PROBLEM"]);
        for p in &self.problems {
            if let Problem::MountPoint { dst, read_only, host } = p {
                table.row(vec![
                    Cell::styled(CYAN, tilde(dst)),
                    Cell::styled(DIM, "inside read-only ")
                        .then(Default::default(), tilde(read_only))
                        .then(DIM, ", and ")
                        .then(CYAN, tilde(host))
                        .then(DIM, " isn't on the host"),
                ]);
            }
        }
        section(table, Vec::new());

        let mut table = Table::new(&["ENTRY", "ERROR"]);
        for p in &self.problems {
            if let Problem::Config { entry, message } = p {
                table.row(vec![Cell::new(entry.clone()), Cell::new(message.clone())]);
            }
        }
        section(table, Vec::new());

        let mut table = Table::new(&["SYSTEM"]);
        for p in &self.problems {
            if let Problem::System { message } = p {
                table.row(vec![Cell::new(message.clone())]);
            }
        }
        section(
            table,
            vec![format!("{} shows what this system lacks", code("psbox check"))],
        );

        let mut table = Table::new(&["ALSO"]);
        for (what, why) in &self.notes {
            table.row(vec![
                Cell::styled(YELLOW, "! ")
                    .then(Default::default(), what.clone())
                    .then(DIM, format!(" · {why}")),
            ]);
        }
        section(table, Vec::new());
        out
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = crate::ui::plain(&self.render());
        f.write_str(text.trim_start_matches("✗ ").trim_end())
    }
}

impl std::error::Error for Report {}

/// Compares two plans for the same sandbox. Binds and tmpfs mounts (shares,
/// masks, forwarded sockets) change live; everything else that differs
/// needs a restart. Environment, commands, app ids, device classes, and the
/// idle timeout simply take effect from the new plan.
pub fn diff(old: &Plan, new: &Plan) -> Change {
    let mut c = Change::default();
    if old.host_home_dir != new.host_home_dir || old.home != new.home {
        c.restart.push("home".into());
    }
    if old.dbus != new.dbus {
        c.restart.push("dbus".into());
    }
    if old.dbus_talk != new.dbus_talk {
        c.restart.push("dbus-talk".into());
    }
    if old.xwayland != new.xwayland {
        c.restart.push("xwayland".into());
    }
    if old.network != new.network {
        c.restart.push("network".into());
    }
    let cache_tmpfs = |p: &Plan| {
        p.mounts.contains(&Mount::Tmpfs {
            dst: p.home.join(".cache"),
            mode: 0o700,
        })
    };
    if !c.restart.iter().any(|r| r == "home") && cache_tmpfs(old) != cache_tmpfs(new) {
        c.restart.push("cache".into());
    }
    // Detaching a mount takes everything mounted beneath it along, and
    // attaching one covers what is already there, so unchanged mounts below
    // a changed one are detached and attached again too.
    let changed: Vec<&Path> = old
        .mounts
        .iter()
        .filter(|m| !new.mounts.contains(m))
        .chain(new.mounts.iter().filter(|m| !old.mounts.contains(m)))
        .map(Mount::dst)
        .collect();
    let beneath = |m: &Mount| changed.iter().any(|d| m.dst() != *d && m.dst().starts_with(d));
    for m in old
        .mounts
        .iter()
        .rev()
        .filter(|m| !new.mounts.contains(m) || beneath(m))
    {
        if live_mount(m) {
            c.detach.push(m.clone());
        } else {
            c.restart.push(format!("mount at {}", m.dst().display()));
        }
    }
    for m in new.mounts.iter().filter(|m| !old.mounts.contains(m) || beneath(m)) {
        if live_mount(m) {
            c.attach.push(m.clone());
        } else {
            c.restart.push(format!("mount at {}", m.dst().display()));
        }
    }
    // Home, network, and Xwayland changes show up as mount changes too;
    // report them once.
    if c.restart.iter().any(|r| r == "xwayland") {
        let x11 = format!("mount at {}", crate::x11::SOCKET_DIR);
        c.restart.retain(|r| *r != x11);
    }
    if c.restart.iter().any(|r| r == "network") {
        c.restart
            .retain(|r| r != "mount at /etc/resolv.conf" && r != "mount at /run/systemd/resolve");
    }
    if c.restart.iter().any(|r| r == "home") {
        let home = &new.home;
        c.restart
            .retain(|r| !r.starts_with("mount at ") || !Path::new(&r["mount at ".len()..]).starts_with(home));
    }
    // A mount that is both detached and attached is one reason.
    let mut seen = std::collections::HashSet::new();
    c.restart.retain(|r| seen.insert(r.clone()));
    if !c.restart.is_empty() {
        c.attach.clear();
        c.detach.clear();
    }
    c
}

/// Client variables that are passed through to spawned processes. Everything
/// else from the launching environment is dropped.
pub fn client_env_allowed(key: &str) -> bool {
    matches!(
        key,
        "TERM"
            | "COLORTERM"
            | "LANG"
            | "LANGUAGE"
            | "TZ"
            | "NO_COLOR"
            | "XDG_CURRENT_DESKTOP"
            | "XDG_SESSION_DESKTOP"
            | "DESKTOP_SESSION"
            | "XDG_ACTIVATION_TOKEN"
            | "DESKTOP_STARTUP_ID"
    ) || key.starts_with("LC_")
}

impl Plan {
    /// Builds the plan for `id`. Tasks get [`Spec::for_task`].
    pub fn build(id: &Identity, spec: &Spec, host: &HostEnv) -> Result<Self> {
        let name = id.sandbox();
        let task_spec = match id {
            Identity::Task { task, .. } => Some(spec.for_task(task)?),
            Identity::Instance { .. } | Identity::Unnamed { .. } => None,
        };
        let spec = task_spec.as_ref().unwrap_or(spec);
        let unnamed = matches!(id, Identity::Unnamed { .. });
        let mut problems = Vec::new();
        let (network, network_fallback) = match resolve_network(spec.network, host.pasta) {
            Some(r) => r,
            None => {
                problems.push(Problem::Setting {
                    setting: match unnamed {
                        true => "--network isolated".into(),
                        false => "network = \"isolated\"".into(),
                    },
                    needs: "pasta".into(),
                    detail: "not installed (package passt)".into(),
                });
                (Network::Isolated, false)
            }
        };
        let display = spec.display;
        let tokens = Tokens {
            home: &host.home,
            root: &host.root,
            name,
        };
        let home = host.home.clone();
        let rt = host.runtime_dir.clone();
        let classes = spec.device_classes();
        let mut mounts = Vec::new();

        let ro = |p: &str, optional: bool| Mount::Bind {
            src: p.into(),
            dst: p.into(),
            ro: true,
            optional,
        };
        mounts.push(ro("/usr", false));
        mounts.push(ro("/etc", false));
        mounts.push(ro("/opt", true));
        mounts.push(ro("/sys", false));
        for (link, target) in [
            ("/bin", "usr/bin"),
            ("/sbin", "usr/bin"),
            ("/lib", "usr/lib"),
            ("/lib64", "usr/lib"),
        ] {
            mounts.push(Mount::Symlink {
                target: target.into(),
                dst: link.into(),
            });
        }
        mounts.push(Mount::Proc { dst: "/proc".into() });
        mounts.push(Mount::Dev { dst: "/dev".into() });
        mounts.push(Mount::Tmpfs {
            dst: "/tmp".into(),
            mode: 0o1777,
        });
        mounts.push(Mount::Dir { dst: "/var".into() });
        mounts.push(Mount::Tmpfs {
            dst: "/var/tmp".into(),
            mode: 0o1777,
        });
        mounts.push(ro("/var/cache/fontconfig", true));
        match network {
            Network::Host => mounts.push(ro("/run/systemd/resolve", true)),
            // Name lookups go to pasta's DNS forwarder, which relays them to
            // the host's resolver.
            Network::Isolated => mounts.push(Mount::File {
                dst: "/etc/resolv.conf".into(),
                content: format!("nameserver {PASTA_DNS}\n"),
            }),
            Network::None => {}
        }
        mounts.push(ro("/run/udev", true));
        mounts.push(Mount::Tmpfs {
            dst: rt.clone(),
            mode: 0o700,
        });

        let mut env = BTreeMap::new();
        let wayland_socket = host.wayland_socket.as_ref().filter(|_| display);
        let xwayland = spec.xwayland && wayland_socket.is_some();
        if let Some(sock) = wayland_socket {
            let file = sock.file_name().context("Wayland socket has no file name")?;
            mounts.push(Mount::Bind {
                src: sock.clone(),
                dst: rt.join(file),
                ro: true,
                optional: true,
            });
            env.insert("WAYLAND_DISPLAY".into(), file.to_string_lossy().into_owned());
            env.insert("XDG_SESSION_TYPE".into(), "wayland".into());
        }
        if classes.contains(&DeviceClass::Audio) {
            for rel in ["pipewire-0", "pulse/native"] {
                mounts.push(Mount::Bind {
                    src: host.runtime_dir.join(rel),
                    dst: rt.join(rel),
                    ro: true,
                    optional: true,
                });
            }
        }

        let host_home_dir = match &spec.home {
            Home::Private => Some(host.root.join(name)),
            Home::Path(p) => Some(tokens.expand(p)?),
            Home::Tmpfs | Home::None => None,
        };
        match &spec.home {
            Home::Private | Home::Path(_) => {
                let src = host_home_dir.clone().expect("set above");
                mounts.push(Mount::Bind {
                    src,
                    dst: home.clone(),
                    ro: false,
                    optional: false,
                });
                if spec.cache == Cache::Tmpfs {
                    mounts.push(Mount::Tmpfs {
                        dst: home.join(".cache"),
                        mode: 0o700,
                    });
                }
            }
            Home::Tmpfs => {
                mounts.push(Mount::Tmpfs {
                    dst: home.clone(),
                    mode: 0o700,
                });
            }
            Home::None => {}
        }

        // Shares, labeled by the config entry they come from. Paths are
        // compared and mounted without their trailing `/`, which only marks
        // a directory.
        let expand = |p: &str| tokens.expand(p).map(|p| p.components().collect::<PathBuf>());
        let mut labeled = Vec::new();
        let mut sources = Vec::new();
        for (list, entry, ro) in [(&spec.rw, SourceEntry::Rw, false), (&spec.ro, SourceEntry::Ro, true)] {
            let label = entry_label(&entry, unnamed);
            for p in list {
                let path = match expand(p) {
                    Ok(path) => path,
                    Err(e) => {
                        problems.push(Problem::Config {
                            entry: label.clone(),
                            message: format!("{e:#}"),
                        });
                        continue;
                    }
                };
                sources.push(Source {
                    path: path.clone(),
                    entry: entry.clone(),
                    dir: p.ends_with('/'),
                });
                let share = Mount::Bind {
                    src: path.clone(),
                    dst: path,
                    ro,
                    optional: false,
                };
                labeled.push((label.clone(), share));
            }
        }
        for (mname, m) in &spec.mount {
            let entry = SourceEntry::Mount(mname.clone());
            let label = entry_label(&entry, unnamed);
            let src = m.host.as_deref().map(expand).transpose();
            let at = m.at.as_deref().map(expand).transpose();
            let (src, dst) = match (src, at) {
                (Ok(src), Ok(Some(dst))) => (src, dst),
                (Ok(Some(src)), Ok(None)) => (Some(src.clone()), src),
                (Ok(None), Ok(None)) => unreachable!("validated: mounts without host have at"),
                (Err(e), _) | (_, Err(e)) => {
                    problems.push(Problem::Config {
                        entry: label.clone(),
                        message: format!("{e:#}"),
                    });
                    continue;
                }
            };
            if let Some(path) = src.as_ref().filter(|_| !m.optional) {
                sources.push(Source {
                    path: path.clone(),
                    entry: entry.clone(),
                    dir: m.host.as_deref().is_some_and(|h| h.ends_with('/')),
                });
            }
            let share = match m.mode {
                MountMode::Ro | MountMode::Rw => Mount::Bind {
                    src: src.expect("validated"),
                    dst,
                    ro: m.mode == MountMode::Ro,
                    optional: m.optional,
                },
                MountMode::Overlay => Mount::Overlay {
                    lower: src.expect("validated"),
                    dst,
                    optional: m.optional,
                },
                MountMode::Empty => Mount::Tmpfs { dst, mode: 0o755 },
            };
            labeled.push((label, share));
        }
        let mut shares = Vec::new();
        for (label, m) in labeled {
            let dst = m.dst();
            if dst == Path::new("/") || dst.starts_with("/proc") || dst.starts_with("/dev") {
                problems.push(Problem::Config {
                    entry: label,
                    message: format!("cannot mount at {} (psbox sets up /, /proc, and /dev)", dst.display()),
                });
                continue;
            }
            shares.push(m);
        }
        if spec.dbus == Dbus::Host {
            let session = BusPaths::new(&rt).socket;
            match host.session_bus.clone() {
                Some(src) => {
                    mounts.push(Mount::Bind {
                        src,
                        dst: session.clone(),
                        ro: true,
                        optional: false,
                    });
                    mounts.push(ro("/run/dbus", true));
                    mounts.push(ro("/var/lib/dbus", true));
                    env.insert(
                        "DBUS_SESSION_BUS_ADDRESS".into(),
                        format!("unix:path={}", session.display()),
                    );
                }
                None => problems.push(Problem::Setting {
                    setting: match unnamed {
                        true => "--dbus host".into(),
                        false => "dbus = \"host\"".into(),
                    },
                    needs: "a unix:path= session bus address".into(),
                    detail: "DBUS_SESSION_BUS_ADDRESS has none".into(),
                }),
            }
        }
        // Parents before children, after everything psbox mounts itself.
        shares.sort_by_key(|m| m.dst().components().count());
        mounts.extend(shares);
        if xwayland {
            // The X socket directory is the sandbox's own even when `/tmp`
            // is a share, so the socket never lands in the host's.
            mounts.push(Mount::Tmpfs {
                dst: crate::x11::SOCKET_DIR.into(),
                mode: 0o1777,
            });
        }

        env.insert("HOME".into(), home.to_string_lossy().into_owned());
        env.insert("USER".into(), host.user.clone());
        env.insert("LOGNAME".into(), host.user.clone());
        env.insert("PATH".into(), DEFAULT_PATH.into());
        env.insert("XDG_RUNTIME_DIR".into(), rt.to_string_lossy().into_owned());
        let bus = (spec.dbus == Dbus::Portal).then(|| BusPaths::new(&rt));
        if let Some(bus) = &bus {
            env.insert("DBUS_SESSION_BUS_ADDRESS".into(), bus.address());
            // Use portals for files, notifications, and settings: GLib,
            // GTK 3, GTK 4, and GNotification each have their own switch.
            env.insert("GIO_USE_PORTALS".into(), "1".into());
            env.insert("GTK_USE_PORTAL".into(), "1".into());
            env.insert("GDK_DEBUG".into(), "portals".into());
            env.insert("GNOTIFICATION_BACKEND".into(), "portal".into());
        }
        env.extend(spec.env.clone());

        let mut commands: BTreeMap<String, Command> = spec
            .command
            .iter()
            .map(|(k, c)| {
                (
                    k.clone(),
                    Command {
                        exec: c.exec.clone(),
                        env: c.env.clone(),
                        app_id: c.app_id.clone().or_else(|| {
                            c.desktop
                                .as_ref()
                                .map(|d| crate::install::desktop_id(k, None, d.id.as_deref()))
                        }),
                    },
                )
            })
            .collect();
        if commands.is_empty() {
            commands.insert(
                name.into(),
                Command {
                    exec: vec![name.into()],
                    env: BTreeMap::new(),
                    app_id: None,
                },
            );
        }

        Ok(Self {
            name: name.into(),
            key: id.key(),
            host_home_dir,
            home,
            mounts,
            audio: classes.contains(&DeviceClass::Audio),
            node_classes: classes.into_iter().filter(|c| c.is_node()).collect(),
            env,
            commands,
            idle_timeout: Duration::from_secs(spec.idle_timeout),
            dbus: spec.dbus,
            // Each name is owned once on the private bus, so duplicates go.
            dbus_talk: match &bus {
                Some(_) => spec.dbus_talk.iter().fold(Vec::new(), |mut names, n| {
                    if !names.contains(n) {
                        names.push(n.clone());
                    }
                    names
                }),
                None => Vec::new(),
            },
            bus,
            xwayland,
            network,
            network_fallback,
            app_id: spec.app_id.clone(),
            runtime_dir: rt,
            sources,
            sandbox_root: host.root.clone(),
            problems,
        })
    }

    /// The directory beneath which the host path `path` is opened without
    /// following symlinks (see [`crate::sys::mount::open_source`]): its
    /// entry in the sandbox root, or the outermost read-write share of this
    /// sandbox containing it. Apps can write in both, so a symlink there may
    /// have been planted to point a mount somewhere else.
    pub fn source_guard(&self, path: &Path) -> Option<PathBuf> {
        let root_entry = path
            .strip_prefix(&self.sandbox_root)
            .ok()
            .and_then(|rest| rest.components().next())
            .map(|first| self.sandbox_root.join(first));
        self.mounts
            .iter()
            .filter_map(|m| match m {
                Mount::Bind { src, ro: false, .. } => Some(src.clone()),
                _ => None,
            })
            .chain(root_entry)
            .filter(|g| path.starts_with(g) && path != g)
            .min_by_key(|g| g.components().count())
    }

    /// The first symlink in `path` beneath its guard (see
    /// [`Plan::source_guard`]), with that guard.
    fn planted_link(&self, path: &Path) -> Option<(PathBuf, PathBuf)> {
        let guard = self.source_guard(path)?;
        let mut at = guard.clone();
        for part in path.strip_prefix(&guard).ok()?.components() {
            at.push(part);
            match std::fs::symlink_metadata(&at) {
                Ok(meta) if meta.file_type().is_symlink() => return Some((at, guard)),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
        None
    }

    /// Creates the missing directories marked with a trailing `/`: those in
    /// the sandbox root, and with `anywhere` the others too. Optional mounts
    /// are never created. Returns the directories created.
    pub fn create_dirs(&self, anywhere: bool) -> Result<Vec<PathBuf>> {
        let mut created = Vec::new();
        for s in &self.sources {
            if s.dir
                && missing(&s.path)
                && broken_link(&s.path).is_none()
                && (anywhere || s.path.starts_with(&self.sandbox_root))
            {
                crate::sys::mount::create_dir_guarded(&s.path, self.source_guard(&s.path).as_deref())
                    .with_context(|| format!("create {}", s.path.display()))?;
                created.push(s.path.clone());
            }
        }
        Ok(created)
    }

    /// Fails with everything known to stop the sandbox from starting:
    /// problems found while planning, shared host paths that do not exist,
    /// mount points that cannot be created, and missing host features. All
    /// of them are reported together, so they can be fixed in one go.
    /// `create_dirs` says whether the caller already created the missing
    /// directories outside the sandbox root (`--create-dirs`).
    pub fn preflight(&self, id: &Identity, create_dirs: bool) -> Result<()> {
        let mut problems = self.problems.clone();
        problems.extend(self.host_problems(id));
        if problems.is_empty() {
            return Ok(());
        }
        let mut notes = Vec::new();
        if self.network_fallback {
            notes.push(("uses the host network".into(), "pasta isn't installed".into()));
        }
        let unnamed = matches!(id, Identity::Unnamed { .. });
        Err(Report {
            who: match unnamed {
                true => "the sandbox".into(),
                false => id.describe(),
            },
            unnamed,
            root: self.sandbox_root.clone(),
            create_dirs,
            problems,
            notes,
        }
        .into())
    }

    fn host_problems(&self, id: &Identity) -> Vec<Problem> {
        use crate::sys::landlock::{REQUIRED_ABI, abi_version};
        use crate::ui::tilde;
        let unnamed = matches!(id, Identity::Unnamed { .. });
        let mut problems = Vec::new();

        match abi_version() {
            Ok(abi) if abi >= REQUIRED_ABI => {}
            Ok(abi) => problems.push(Problem::System {
                message: format!("Landlock ABI {abi} (psbox needs {REQUIRED_ABI})"),
            }),
            Err(e) => problems.push(Problem::System {
                message: format!("Landlock is unavailable ({e})"),
            }),
        }
        if self.bus.is_some() && find_in_path("dbus-daemon").is_none() {
            problems.push(Problem::Setting {
                setting: match unnamed {
                    true => "--dbus portal".into(),
                    false => "dbus = \"portal\"".into(),
                },
                needs: "dbus-daemon".into(),
                detail: "not installed".into(),
            });
        }

        for s in &self.sources {
            if let Some((link, guard)) = self.planted_link(&s.path) {
                problems.push(Problem::Config {
                    entry: entry_label(&s.entry, unnamed),
                    message: format!(
                        "{} is a symlink inside {}, which the sandbox can write to; psbox doesn't follow it",
                        tilde(&link),
                        tilde(&guard)
                    ),
                });
                continue;
            }
            match std::fs::metadata(&s.path) {
                _ if let Some((target, looped)) = broken_link(&s.path) => problems.push(Problem::BrokenLink {
                    path: s.path.clone(),
                    target,
                    entry: s.entry.clone(),
                    looped,
                }),
                Ok(meta) if s.dir && !meta.is_dir() => problems.push(Problem::Config {
                    entry: entry_label(&s.entry, unnamed),
                    message: format!("{} isn't a directory", tilde(&s.path)),
                }),
                // Created by the supervisor as it starts.
                _ if s.dir && s.path.starts_with(&self.sandbox_root) => {}
                _ if missing(&s.path) => problems.push(Problem::Missing {
                    path: s.path.clone(),
                    entry: s.entry.clone(),
                    dir: s.dir,
                }),
                _ => {}
            }
        }

        // A mount point that does not exist is created inside whatever is
        // mounted above it, which fails when that is a read-only bind.
        let mounted = |m: &Mount| match m {
            Mount::Bind { src, optional, .. } => !(*optional && missing(src)),
            Mount::Overlay { lower, optional, .. } => !(*optional && missing(lower)),
            Mount::Tmpfs { .. } | Mount::Proc { .. } | Mount::Dev { .. } => true,
            Mount::Symlink { .. } | Mount::Dir { .. } | Mount::File { .. } => false,
        };
        for (i, m) in self.mounts.iter().enumerate() {
            let dst = m.dst();
            let earlier = &self.mounts[..i];
            if !mounted(m)
                || matches!(m, Mount::Proc { .. } | Mount::Dev { .. })
                || earlier.iter().any(|e| e.dst() == dst)
            {
                continue;
            }
            let above = earlier
                .iter()
                .filter(|e| mounted(e) && dst.starts_with(e.dst()))
                .max_by_key(|e| e.dst().components().count());
            if let Some(Mount::Bind {
                src, dst: at, ro: true, ..
            }) = above
            {
                let host = src.join(dst.strip_prefix(at).expect("beneath"));
                if missing(&host) {
                    problems.push(Problem::MountPoint {
                        dst: dst.to_path_buf(),
                        read_only: at.clone(),
                        host,
                    });
                }
            }
        }
        problems
    }

    /// The command used when a launch does not name one: the command named
    /// like the sandbox, else the only command.
    pub fn default_command(&self) -> Option<&str> {
        if self.commands.contains_key(&self.name) {
            return Some(&self.name);
        }
        if self.commands.len() == 1 {
            return self.commands.keys().next().map(String::as_str);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn main(name: &str) -> Identity {
        Identity::Instance {
            sandbox: name.into(),
            instance: None,
        }
    }

    fn host() -> HostEnv {
        HostEnv {
            uid: 1000,
            gid: 1000,
            user: "u".into(),
            home: "/home/u".into(),
            runtime_dir: "/run/user/1000".into(),
            wayland_socket: Some("/run/user/1000/wayland-0".into()),
            session_bus: Some("/run/user/1000/bus".into()),
            config_dir: "/home/u/.config/psbox".into(),
            root: "/home/u/Sandbox".into(),
            pasta: true,
        }
    }

    #[test]
    fn dbus_talk_reaches_the_plan_only_with_a_private_bus() {
        let talk = |text: &str| {
            Plan::build(&main("app"), &Spec::parse(text).unwrap(), &host())
                .unwrap()
                .dbus_talk
        };
        assert!(talk("").is_empty());
        assert_eq!(talk("dbus-talk = [\"org.a.b\"]"), ["org.a.b"]);
        assert_eq!(
            talk("dbus-talk = [\"org.a.b\", \"org.c.d\", \"org.a.b\"]"),
            ["org.a.b", "org.c.d"]
        );
        // Tasks have no bus unless they set one.
        let spec = Spec::parse("dbus-talk = [\"org.a.b\"]\n[task.t]\nexec = [\"x\"]\n").unwrap();
        let task = Identity::Task {
            sandbox: "app".into(),
            task: "t".into(),
        };
        assert!(Plan::build(&task, &spec, &host()).unwrap().dbus_talk.is_empty());
        let old = Plan::build(&main("app"), &Spec::parse("").unwrap(), &host()).unwrap();
        let new = Plan::build(&main("app"), &spec, &host()).unwrap();
        assert!(diff(&old, &new).restart.contains(&"dbus-talk".to_string()));
    }

    #[test]
    fn default_plan_binds_private_home_and_wayland() {
        let plan = Plan::build(&main("app"), &Spec::parse("").unwrap(), &host()).unwrap();
        assert_eq!(plan.host_home_dir.as_deref(), Some(Path::new("/home/u/Sandbox/app")));
        assert!(plan.mounts.contains(&Mount::Bind {
            src: "/home/u/Sandbox/app".into(),
            dst: "/home/u".into(),
            ro: false,
            optional: false
        }));
        assert!(plan.mounts.contains(&Mount::Tmpfs {
            dst: "/home/u/.cache".into(),
            mode: 0o700
        }));
        assert!(plan.mounts.contains(&Mount::Bind {
            src: "/run/user/1000/pipewire-0".into(),
            dst: "/run/user/1000/pipewire-0".into(),
            ro: true,
            optional: true
        }));
        assert_eq!(plan.env["WAYLAND_DISPLAY"], "wayland-0");
        assert_eq!(plan.env["DBUS_SESSION_BUS_ADDRESS"], "unix:path=/run/user/1000/bus");
        assert_eq!(plan.env["GIO_USE_PORTALS"], "1");
        let off = Plan::build(&main("app"), &Spec::parse("dbus = \"off\"").unwrap(), &host()).unwrap();
        assert!(off.bus.is_none());
        assert!(!off.env.contains_key("DBUS_SESSION_BUS_ADDRESS"));
        assert!(!plan.xwayland);
        let x = Spec::parse("xwayland = true").unwrap();
        assert!(Plan::build(&main("app"), &x, &host()).unwrap().xwayland);
        let headless = HostEnv {
            wayland_socket: None,
            ..host()
        };
        assert!(!Plan::build(&main("app"), &x, &headless).unwrap().xwayland);
        assert_eq!(plan.node_classes, vec![DeviceClass::Gpu]);
        assert_eq!(plan.default_command(), Some("app"));
    }

    #[test]
    fn shares_are_ordered_parents_first() {
        let spec = Spec::parse(
            r#"
            rw = ["~/Games/saves"]
            ro = ["~/Games"]
            [mount.bin]
            host = "@self.bin"
            at = "/opt/app"
            mode = "overlay"
            "#,
        )
        .unwrap();
        let plan = Plan::build(&main("retro"), &spec, &host()).unwrap();
        let games = plan
            .mounts
            .iter()
            .position(|m| m.dst() == Path::new("/home/u/Games"))
            .unwrap();
        let saves = plan
            .mounts
            .iter()
            .position(|m| m.dst() == Path::new("/home/u/Games/saves"))
            .unwrap();
        assert!(games < saves);
        assert!(plan.mounts.contains(&Mount::Overlay {
            lower: "/home/u/Sandbox/retro.bin".into(),
            dst: "/opt/app".into(),
            optional: false
        }));
    }

    #[test]
    fn no_audio_without_classes() {
        let plan = Plan::build(&main("x"), &Spec::parse("devices = [\"camera\"]").unwrap(), &host()).unwrap();
        assert!(!plan.mounts.iter().any(|m| m.dst().ends_with("pipewire-0")));
        assert_eq!(plan.node_classes, vec![DeviceClass::Camera]);
    }

    #[test]
    fn rejects_dev_and_proc_targets() {
        let p = Plan::build(
            &main("x"),
            &Spec::parse("rw = [\"/dev/bus\", \"/proc/sys\"]").unwrap(),
            &host(),
        )
        .unwrap();
        let config = |dst: &str| Problem::Config {
            entry: "rw".into(),
            message: format!("cannot mount at {dst} (psbox sets up /, /proc, and /dev)"),
        };
        assert_eq!(p.problems, [config("/dev/bus"), config("/proc/sys")]);
        assert!(!p.mounts.iter().any(|m| m.dst().starts_with("/proc/sys")));
    }

    #[test]
    fn examples_parse_and_plan() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_stem().unwrap().to_str().unwrap().to_string();
            let spec = Spec::parse(&std::fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|e| panic!("{}: {e:#}", path.display()));
            let plan =
                Plan::build(&main(&name), &spec, &host()).unwrap_or_else(|e| panic!("{}: {e:#}", path.display()));
            assert!(plan.default_command().is_some() || plan.commands.len() > 1, "{name}");
            assert!(plan.problems.is_empty(), "{name}: {:?}", plan.problems);
            count += 1;
        }
        assert!(count > 0);
    }

    #[test]
    fn identities() {
        let i = |instance: Option<&str>| Identity::Instance {
            sandbox: "firefox".into(),
            instance: instance.map(String::from),
        };
        assert_eq!(i(None).key(), "firefox");
        assert_eq!(i(Some("work")).key(), "firefox.instance-work");
        let t = Identity::Task {
            sandbox: "retroarch".into(),
            task: "update".into(),
        };
        assert_eq!(t.key(), "retroarch.task-update");
        let u = Identity::Unnamed {
            id: "0123456789abcdef".into(),
        };
        assert_eq!(u.key(), "sandbox.0123456789abcdef");
        for id in [i(None), i(Some("work")), t.clone(), u] {
            assert_eq!(Identity::from_key(&id.key()).unwrap(), id);
        }
        assert!(Identity::from_key("sandbox.0123").is_err());
        assert!(Identity::from_key("sandbox.0123456789ABCDEF").is_err());
        // `sandbox` is reserved for unnamed sandboxes.
        assert!(Identity::from_key("sandbox.instance-x").is_err());
        assert!(Identity::from_key("a.other-x").is_err());
        assert!(Identity::from_key("a.task-x.y").is_err());
        assert!(i(Some("a.b")).validate().is_err());
        assert!(
            Identity::Task {
                sandbox: "a.b".into(),
                task: "x".into()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn host_bus_and_no_display() {
        let plan = Plan::build(
            &main("app"),
            &Spec::parse("dbus = \"host\"\ndisplay = false").unwrap(),
            &host(),
        )
        .unwrap();
        assert!(plan.bus.is_none());
        assert_eq!(plan.env["DBUS_SESSION_BUS_ADDRESS"], "unix:path=/run/user/1000/bus");
        assert!(!plan.env.contains_key("GTK_USE_PORTAL"));
        assert!(plan.mounts.contains(&Mount::Bind {
            src: "/run/user/1000/bus".into(),
            dst: "/run/user/1000/bus".into(),
            ro: true,
            optional: false
        }));
        assert!(plan.mounts.iter().any(|m| m.dst() == Path::new("/run/dbus")));
        assert!(!plan.xwayland && !plan.env.contains_key("WAYLAND_DISPLAY"));

        let elsewhere = HostEnv {
            session_bus: unix_path("unix:abstract=/x;unix:guid=1,path=/tmp/test%20bus"),
            ..host()
        };
        let p = Plan::build(&main("app"), &Spec::parse("dbus = \"host\"").unwrap(), &elsewhere).unwrap();
        assert!(p.mounts.contains(&Mount::Bind {
            src: "/tmp/test bus".into(),
            dst: "/run/user/1000/bus".into(),
            ro: true,
            optional: false
        }));
        let no_bus = HostEnv {
            session_bus: None,
            ..host()
        };
        let p = Plan::build(&main("app"), &Spec::parse("dbus = \"host\"").unwrap(), &no_bus).unwrap();
        assert!(matches!(&p.problems[..], [Problem::Setting { setting, .. }] if setting == "dbus = \"host\""));

        let portal = Plan::build(&main("app"), &Spec::parse("xwayland = true").unwrap(), &host()).unwrap();
        assert_eq!(
            diff(&portal, &plan).restart,
            vec!["dbus".to_string(), "xwayland".to_string()]
        );
    }

    #[test]
    fn task_plans_have_no_display_audio_or_bus() {
        let spec = Spec::parse(
            "[mount.bin]\nhost = \"@self.bin\"\nat = \"~/.bin\"\nmode = \"ro\"\n[task.update]\nhome = \"tmpfs\"\nwritable = [\"bin\"]\nscript = \"true\"\n",
        )
        .unwrap();
        let id = Identity::Task {
            sandbox: "retroarch".into(),
            task: "update".into(),
        };
        let plan = Plan::build(&id, &spec, &host()).unwrap();
        assert_eq!(plan.key, "retroarch.task-update");
        assert!(plan.bus.is_none() && !plan.xwayland);
        assert!(!plan.env.contains_key("WAYLAND_DISPLAY") && !plan.env.contains_key("DISPLAY"));
        assert!(
            !plan
                .mounts
                .iter()
                .any(|m| m.dst().ends_with("wayland-0") || m.dst().ends_with("pipewire-0"))
        );
        assert!(plan.mounts.contains(&Mount::Tmpfs {
            dst: "/home/u".into(),
            mode: 0o700
        }));
        assert!(
            plan.mounts
                .iter()
                .any(|m| matches!(m, Mount::Bind { dst, ro: false, .. } if dst == Path::new("/home/u/.bin")))
        );
        assert!(plan.node_classes.is_empty());
        assert!(plan.idle_timeout.is_zero());
        assert_eq!(plan.default_command(), Some("update"));
    }

    #[test]
    fn network_modes() {
        let build = |text: &str| Plan::build(&main("app"), &Spec::parse(text).unwrap(), &host()).unwrap();
        let isolated = build("network = \"isolated\"");
        assert!(isolated.mounts.contains(&Mount::File {
            dst: "/etc/resolv.conf".into(),
            content: format!("nameserver {PASTA_DNS}\n")
        }));
        assert!(
            !isolated
                .mounts
                .iter()
                .any(|m| m.dst() == Path::new("/run/systemd/resolve"))
        );
        assert_eq!(diff(&build("network = \"host\""), &isolated).restart, vec!["network"]);
        let none = build("network = \"none\"");
        assert!(!none.mounts.iter().any(|m| matches!(m, Mount::File { .. })));
    }

    #[test]
    fn network_defaults_to_isolated_when_pasta_is_installed() {
        let no_pasta = HostEnv { pasta: false, ..host() };
        let build = |text: &str, host: &HostEnv| Plan::build(&main("app"), &Spec::parse(text).unwrap(), host);

        let p = build("", &host()).unwrap();
        assert_eq!((p.network, p.network_fallback), (Network::Isolated, false));
        let p = build("", &no_pasta).unwrap();
        assert_eq!((p.network, p.network_fallback), (Network::Host, true));
        assert!(p.mounts.iter().any(|m| m.dst() == Path::new("/run/systemd/resolve")));
        let p = build("network = \"host\"", &no_pasta).unwrap();
        assert_eq!((p.network, p.network_fallback), (Network::Host, false));
        let p = build("network = \"none\"", &no_pasta).unwrap();
        assert_eq!((p.network, p.network_fallback), (Network::None, false));

        let pasta = |setting: &str| Problem::Setting {
            setting: setting.into(),
            needs: "pasta".into(),
            detail: "not installed (package passt)".into(),
        };
        let p = build("network = \"isolated\"", &no_pasta).unwrap();
        assert_eq!(p.problems, [pasta("network = \"isolated\"")]);
        let unnamed = Identity::Unnamed { id: "0".repeat(16) };
        let p = Plan::build(&unnamed, &Spec::parse("network = \"isolated\"").unwrap(), &no_pasta).unwrap();
        assert_eq!(p.problems, [pasta("--network isolated")]);

        // Tasks inherit the sandbox's setting, explicit or not.
        let task = |text: &str| {
            let id = Identity::Task {
                sandbox: "app".into(),
                task: "t".into(),
            };
            Plan::build(
                &id,
                &Spec::parse(&format!("{text}\n[task.t]\nexec = [\"x\"]")).unwrap(),
                &no_pasta,
            )
        };
        assert!(task("").unwrap().network_fallback);
        assert_eq!(task("network = \"isolated\"").unwrap().problems.len(), 1);
        assert!(!task("network = \"host\"").unwrap().network_fallback);
    }

    #[test]
    fn diffs_live_and_restart_changes() {
        let build = |text: &str| Plan::build(&main("app"), &Spec::parse(text).unwrap(), &host()).unwrap();
        let base = build("rw = [\"~/Downloads\"]");

        let c = diff(
            &base,
            &build("rw = [\"~/Downloads\", \"~/Music\"]\n[mount.ssh]\nat = \"/etc/ssh\"\nmode = \"empty\""),
        );
        assert!(c.restart.is_empty() && c.detach.is_empty());
        assert_eq!(c.attach.len(), 2);

        let c = diff(&base, &build(""));
        assert_eq!(c.detach.len(), 1);
        assert!(c.attach.is_empty());

        let c = diff(&base, &build("rw = [\"~/Downloads\"]\ndevices = [\"gpu\"]"));
        assert_eq!(c.detach.len(), 2, "audio sockets go away live");

        assert!(diff(&base, &build("rw = [\"~/Downloads\"]\nenv = { A = \"1\" }")).is_empty());

        let c = diff(&base, &build("rw = [\"~/Downloads\"]\nhome = \"tmpfs\""));
        assert_eq!(c.restart, vec!["home"]);
        assert!(c.attach.is_empty() && c.detach.is_empty());

        let c = diff(&base, &build("rw = [\"~/Downloads\"]\ndbus = \"off\"\nxwayland = true"));
        assert_eq!(c.restart, vec!["dbus", "xwayland"]);

        let c = diff(
            &base,
            &build("rw = [\"~/Downloads\"]\n[mount.o]\nhost = \"/srv/x\"\nat = \"/opt/x\"\nmode = \"overlay\""),
        );
        assert_eq!(c.restart, vec!["mount at /opt/x"]);

        assert_eq!(
            diff(&base, &build("rw = [\"~/Downloads\"]\ncache = \"home\"")).restart,
            vec!["cache"]
        );
    }

    #[test]
    fn diff_reattaches_mounts_beneath_changed_ones() {
        let build = |text: &str| Plan::build(&main("app"), &Spec::parse(text).unwrap(), &host()).unwrap();
        let private = "[mount.private]\nat = \"/data/private\"\nmode = \"empty\"\n";
        let base = build(&format!("rw = [\"/data\"]\n{private}"));
        let moved = "[mount.data]\nhost = \"/srv/data\"\nat = \"/data\"\nmode = \"rw\"\n";

        let c = diff(&base, &build(&format!("{moved}{private}")));
        let dsts = |ms: &[Mount]| ms.iter().map(|m| m.dst().to_path_buf()).collect::<Vec<_>>();
        assert!(c.restart.is_empty());
        assert_eq!(dsts(&c.detach), [PathBuf::from("/data/private"), "/data".into()]);
        assert_eq!(dsts(&c.attach), [PathBuf::from("/data"), "/data/private".into()]);

        let tmp = |src: &str| {
            build(&format!(
                "xwayland = true\n[mount.tmp]\nhost = \"{src}\"\nat = \"/tmp\"\nmode = \"rw\"\n"
            ))
        };
        assert_eq!(
            diff(&tmp("/tmp"), &tmp("/srv/tmp")).restart,
            vec!["mount at /tmp/.X11-unix"]
        );

        let c = diff(&build(private), &base);
        assert_eq!(dsts(&c.detach), [PathBuf::from("/data/private")]);
        assert_eq!(dsts(&c.attach), [PathBuf::from("/data"), "/data/private".into()]);
    }

    #[test]
    fn preflight_reports_every_problem() {
        let dir = std::env::temp_dir().join(format!("psbox-preflight-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ro")).unwrap();
        let d = dir.display();
        let text = format!(
            "rw = [\"{d}/missing-share\"]\n\
             [mount.data]\nhost = \"{d}/missing-data\"\nmode = \"rw\"\n\
             [mount.extra]\nhost = \"{d}/missing-extra\"\nmode = \"ro\"\noptional = true\n\
             [mount.base]\nhost = \"{d}/ro\"\nat = \"/srv/base\"\nmode = \"ro\"\n\
             [mount.inner]\nat = \"/srv/base/inner\"\nmode = \"empty\"\n"
        );
        let no_pasta = HostEnv { pasta: false, ..host() };
        let mut plan = Plan::build(
            &main("app"),
            &Spec::parse(&format!("network = \"isolated\"\n{text}")).unwrap(),
            &no_pasta,
        )
        .unwrap();
        plan.bus = None;
        let mut problems = plan.problems.clone();
        problems.extend(
            plan.host_problems(&main("app"))
                .into_iter()
                .filter(|p| !matches!(p, Problem::System { .. })),
        );
        let report = Report {
            who: "app".into(),
            unnamed: false,
            root: "/home/u/Sandbox".into(),
            create_dirs: false,
            problems,
            notes: Vec::new(),
        };
        // Columns are as wide as the temporary paths, so compare words.
        let lines: Vec<String> = report
            .to_string()
            .replace(&d.to_string(), "D")
            .lines()
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        assert_eq!(
            lines,
            [
                "app can't start · 4 problems",
                "",
                "MISSING ON THE HOST USED BY",
                "D/missing-data [mount.data]",
                "D/missing-share rw",
                "→ remove D/missing-share from rw",
                "→ add optional = true to [mount.data] if it can be missing",
                "→ end directories with / to have psbox create them in /home/u/Sandbox, and elsewhere with --create-dirs",
                "",
                "SETTING NEEDS",
                "network = \"isolated\" pasta · not installed (package passt)",
                "",
                "MOUNT POINT PROBLEM",
                "/srv/base/inner inside read-only /srv/base, and D/ro/inner isn't on the host",
            ]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn report_nests_missing_paths_and_adds_notes() {
        let missing = |path: &str, mount: &str| Problem::Missing {
            path: path.into(),
            entry: SourceEntry::Mount(mount.into()),
            dir: mount == "saves",
        };
        let report = Report {
            who: "emulator".into(),
            unnamed: false,
            root: "/data".into(),
            create_dirs: false,
            problems: vec![
                missing("/data/bin", "bin"),
                missing("/data/games/saves", "saves"),
                missing("/data/games", "games"),
            ],
            notes: vec![("uses the host network".into(), "pasta isn't installed".into())],
        };
        let text = report.to_string();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "emulator can't start · 3 paths are missing");
        assert!(lines[3].starts_with("  /data/bin "));
        assert!(lines[4].starts_with("  /data/games "));
        assert!(lines[5].starts_with("    saves/ "));
        assert_eq!(
            &lines[6..9],
            [
                "  → add optional = true to the mounts that can be missing",
                "  → end directories with / to have psbox create them in /data",
                "  → run with --create-dirs to create the ones ending in /",
            ]
        );
        assert_eq!(
            &lines[10..],
            ["  ALSO", "  ! uses the host network · pasta isn't installed"]
        );

        let with_flag = Report {
            who: "app".into(),
            unnamed: false,
            root: "/data".into(),
            create_dirs: true,
            problems: vec![missing("/srv/plain", "plain")],
            notes: Vec::new(),
        };
        assert!(with_flag.to_string().ends_with(
            "  → --create-dirs only creates directories that end with /: add the / to those above to have them created"
        ));

        let unnamed = Report {
            who: "the sandbox".into(),
            unnamed: true,
            root: "/home/u/Sandbox".into(),
            create_dirs: false,
            problems: vec![missing("/nope", "bind-000")],
            notes: Vec::new(),
        };
        assert_eq!(
            unnamed.to_string(),
            "the sandbox can't start\n\n  MISSING ON THE HOST   USED BY\n  /nope                 --bind\n  \
             → end directories with / to have psbox create them in /home/u/Sandbox, and elsewhere with --create-dirs"
        );
    }

    #[test]
    fn broken_symlinks_are_reported_and_never_created_through() {
        let dir = std::env::temp_dir().join(format!("psbox-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::os::unix::fs::symlink("real", dir.join("good")).unwrap();
        // Through `..` and back, to check that the target is shown resolved.
        let name = dir.file_name().unwrap().to_str().unwrap();
        std::os::unix::fs::symlink(format!("../{name}/missing/target"), dir.join("broken")).unwrap();
        let d = dir.display();
        let spec = Spec::parse(&format!("rw = [\"{d}/good/\", \"{d}/broken/\"]")).unwrap();
        let mut plan = Plan::build(&main("app"), &spec, &host()).unwrap();
        plan.bus = None;

        let problems: Vec<Problem> = plan
            .host_problems(&main("app"))
            .into_iter()
            .filter(|p| !matches!(p, Problem::System { .. }))
            .collect();
        assert_eq!(
            problems,
            [Problem::BrokenLink {
                path: dir.join("broken"),
                target: dir.join("missing/target"),
                entry: SourceEntry::Rw,
                looped: false,
            }]
        );
        assert!(plan.create_dirs(true).unwrap().is_empty());
        assert!(!dir.join("missing").exists());

        std::os::unix::fs::symlink("loop", dir.join("loop")).unwrap();
        assert_eq!(broken_link(&dir.join("loop")), Some((dir.join("loop"), true)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sources_in_sandbox_writable_places_are_guarded() {
        let spec = Spec::parse(
            "rw = [\"~/Projects/\"]\n[mount.data]\nhost = \"@self/data/\"\nat = \"/data\"\nmode = \"rw\"\n",
        )
        .unwrap();
        let plan = Plan::build(&main("app"), &spec, &host()).unwrap();
        let guard = |p: &str| plan.source_guard(Path::new(p));
        assert_eq!(guard("/home/u/Sandbox/app/data"), Some("/home/u/Sandbox/app".into()));
        assert_eq!(guard("/home/u/Projects/x/y"), Some("/home/u/Projects".into()));
        assert_eq!(
            guard("/home/u/Sandbox/app.bin/x"),
            Some("/home/u/Sandbox/app.bin".into())
        );
        // The guarded directory itself, and paths outside, resolve normally.
        assert_eq!(guard("/home/u/Sandbox/app.bin"), None);
        assert_eq!(guard("/home/u/Projects"), None);
        assert_eq!(guard("/home/u/Documents/Library"), None);
    }

    #[test]
    fn planted_symlinks_are_reported_and_never_followed() {
        let dir = std::env::temp_dir().join(format!("psbox-planted-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("Sandbox");
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::create_dir_all(dir.join("secret")).unwrap();
        std::os::unix::fs::symlink(dir.join("secret"), root.join("app/data")).unwrap();
        let host = HostEnv {
            root: root.clone(),
            ..host()
        };
        let spec = Spec::parse("[mount.data]\nhost = \"@self/data/new/\"\nat = \"/data\"\nmode = \"rw\"\n").unwrap();
        let mut plan = Plan::build(&main("app"), &spec, &host).unwrap();
        plan.bus = None;

        let problems: Vec<Problem> = plan
            .host_problems(&main("app"))
            .into_iter()
            .filter(|p| !matches!(p, Problem::System { .. }))
            .collect();
        assert!(
            matches!(&problems[..], [Problem::Config { message, .. }] if message.contains("is a symlink inside")),
            "{problems:?}"
        );
        assert!(plan.create_dirs(false).is_err());
        assert!(!dir.join("secret/new").exists(), "nothing created through the link");
        let err = crate::sys::mount::open_source(&root.join("app/data/new"), Some(&root.join("app"))).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ELOOP));
        // Outside a guard, and the guard itself, links are followed.
        assert!(crate::sys::mount::open_source(&root.join("app/data"), None).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn trailing_slash_marks_directories_to_create() {
        let dir = std::env::temp_dir().join(format!("psbox-dirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("Sandbox");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("file"), "").unwrap();
        let host = HostEnv {
            root: root.clone(),
            ..host()
        };
        let d = dir.display();
        let spec = Spec::parse(&format!(
            "rw = [\"@self.bin/\", \"@root/plain\", \"@root/file/\"]\n\
             [mount.outside]\nhost = \"{d}/outside/\"\nmode = \"rw\"\n\
             [mount.opt]\nhost = \"@self.opt/\"\nmode = \"rw\"\noptional = true\n"
        ))
        .unwrap();
        let mut plan = Plan::build(&main("app"), &spec, &host).unwrap();
        plan.bus = None;
        let bin = root.join("app.bin");
        assert!(plan.mounts.contains(&Mount::Bind {
            src: bin.clone(),
            dst: bin.clone(),
            ro: false,
            optional: false
        }));

        let problems: Vec<Problem> = plan
            .host_problems(&main("app"))
            .into_iter()
            .filter(|p| !matches!(p, Problem::System { .. }))
            .collect();
        assert_eq!(
            problems,
            [
                Problem::Missing {
                    path: root.join("plain"),
                    entry: SourceEntry::Rw,
                    dir: false
                },
                Problem::Config {
                    entry: "rw".into(),
                    message: format!("{} isn't a directory", root.join("file").display()),
                },
                Problem::Missing {
                    path: dir.join("outside"),
                    entry: SourceEntry::Mount("outside".into()),
                    dir: true
                },
            ]
        );

        assert_eq!(plan.create_dirs(false).unwrap(), std::slice::from_ref(&bin));
        assert!(bin.is_dir());
        assert_eq!(plan.create_dirs(true).unwrap(), [dir.join("outside")]);
        assert!(!root.join("app.opt").exists() && !root.join("plain").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn client_env_allowlist() {
        assert!(client_env_allowed("LC_TIME"));
        assert!(client_env_allowed("TERM"));
        assert!(!client_env_allowed("DBUS_SESSION_BUS_ADDRESS"));
        assert!(!client_env_allowed("SSH_AUTH_SOCK"));
    }
}
