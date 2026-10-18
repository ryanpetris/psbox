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
use crate::spec::{
    AppSpec, Cache, DbusMode, DeviceClass, Home, MountMode, Network, NetworkSetting, Spec, XwaylandMode, XwaylandScale,
};

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
    /// The psbox executable, bound into sandboxes as the link handler.
    pub exe: PathBuf,
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
/// is `host` only because pasta is missing. `automatic` means `isolated`
/// when pasta is installed and `host` otherwise; `None` when `isolated` was
/// chosen and pasta is missing.
pub fn resolve_network(setting: NetworkSetting, pasta: bool) -> Option<(Network, bool)> {
    match setting {
        NetworkSetting::Isolated if !pasta => None,
        NetworkSetting::Isolated => Some((Network::Isolated, false)),
        NetworkSetting::Host => Some((Network::Host, false)),
        NetworkSetting::Loopback => Some((Network::Loopback, false)),
        NetworkSetting::Automatic if pasta => Some((Network::Isolated, false)),
        NetworkSetting::Automatic => Some((Network::Host, true)),
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
            exe: std::env::current_exe().context("find the psbox executable")?,
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
        self.socket_dir().join(format!("{}.json", id.key()))
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

/// The sandbox name unnamed sandboxes use for their key.
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

/// Something a launch can run: an application, or a task's command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub run: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// The app id that gives its processes a portal identity, when the
    /// host portal accepts it (decided at launch, see
    /// [`crate::supervisor`]).
    pub id: Option<String>,
}

/// A host path a mount shares, kept to report it by its mount when it does
/// not exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub path: PathBuf,
    /// The mount's name.
    pub entry: String,
    /// `create`: a directory psbox creates when it is missing (see
    /// [`Plan::create_dirs`]).
    pub dir: bool,
}

/// Something that stops a sandbox from starting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// A shared host path that does not exist (`dir` when it is marked as
    /// a directory with a trailing `/`).
    Missing { path: PathBuf, entry: String, dir: bool },
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
        entry: String,
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

/// How the mount behind a share is named to the user: `mount.<name>`, or
/// the `psbox sandbox` flag that made it.
fn entry_label(entry: &str, unnamed: bool) -> String {
    match entry {
        name if unnamed && name.starts_with(crate::unnamed::RO_BIND_PREFIX) => "--ro-bind".into(),
        name if unnamed && name.starts_with(crate::unnamed::BIND_PREFIX) => "--bind".into(),
        name => format!("mount.{name}"),
    }
}

#[derive(Debug, Clone)]
pub struct Plan {
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
    /// What a launch can run, by name.
    pub apps: BTreeMap<String, App>,
    /// What a launch runs when it names nothing: the default app, or a
    /// task's command.
    pub default_app: Option<String>,
    /// The portal identity to try for a plain `--exec` launch and for the
    /// private bus's services: the default app's id, a task's app's, or an
    /// unnamed sandbox's `--app-id` (set by the supervisor).
    pub exec_identity: Option<String>,
    pub idle_timeout: Duration,
    /// `None`: no session bus.
    pub dbus: Option<DbusMode>,
    /// The private session bus, when the sandbox has one.
    pub bus: Option<BusPaths>,
    /// Host bus names the gateway forwards calls to (`dbus.talk`); empty
    /// without a private bus.
    pub dbus_talk: Vec<String>,
    /// How X11 clients are served; `None` without a Wayland socket.
    pub xwayland: Option<XwaylandMode>,
    /// How X11 apps meet a scaled display, in the builtin mode.
    pub xwayland_scale: XwaylandScale,
    /// The schemes whose links open in the host's default app: those
    /// `links` sends to the host, with a session bus.
    pub open_urls: Vec<String>,
    pub network: Network,
    /// `network` is `host` only because pasta is not installed.
    pub network_fallback: bool,
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

/// Holds what psbox puts in the sandbox for itself: `bin/psbox` (psbox,
/// for `psbox open-uri` and `psbox decorations`), and the link handler, in
/// sandboxes with a session bus: `config` first in `XDG_CONFIG_DIRS`, and
/// `share` first in `XDG_DATA_DIRS`.
pub const PSBOX_DIR: &str = "/run/psbox";

/// The link handler's `mimeapps.list` and desktop entry, claiming `schemes`.
fn link_handler(schemes: &[String]) -> (String, String) {
    let defaults: String = schemes
        .iter()
        .map(|s| format!("x-scheme-handler/{s}=psbox-open-uri.desktop\n"))
        .collect();
    let types: String = schemes.iter().map(|s| format!("x-scheme-handler/{s};")).collect();
    (
        format!("[Default Applications]\n{defaults}"),
        include_str!("assets/psbox-open-uri.desktop").replace("@MIME_TYPES@", &types),
    )
}

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
        let mut missing: Vec<(&PathBuf, &String, bool)> = self
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
        let mounts: Vec<String> = missing
            .iter()
            .filter(|_| !self.unnamed)
            .map(|(_, e, _)| entry_label(e, false))
            .collect();
        let optional = code("optional: true");
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
        let create = code("create: true");
        let root = tilde(&self.root);
        match (
            missing.iter().any(|(_, _, dir)| !dir),
            missing.iter().any(|(p, _, dir)| !dir && !p.starts_with(&self.root)),
        ) {
            (false, _) => {}
            (true, _) if self.create_dirs => fixes.push(format!(
                "{create_dirs} only creates the sources of mounts with {create}: add it to have them created"
            )),
            (true, false) => fixes.push(format!(
                "add {create} to have psbox create missing directories in {root}"
            )),
            (true, true) => fixes.push(format!(
                "add {create} to have psbox create missing directories in {root}, and elsewhere with {create_dirs}"
            )),
        }
        if missing.iter().any(|(_, _, dir)| *dir) {
            fixes.push(format!(
                "run with {create_dirs} to create the ones shown with a trailing /"
            ));
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
/// needs a restart. Environment, apps, device classes, and the idle timeout
/// simply take effect from the new plan.
pub fn diff(old: &Plan, new: &Plan) -> Change {
    let mut c = Change::default();
    if old.host_home_dir != new.host_home_dir || old.home != new.home {
        c.restart.push("home".into());
    }
    if old.dbus != new.dbus {
        c.restart.push("dbus".into());
    }
    if old.dbus_talk != new.dbus_talk {
        c.restart.push("dbus.talk".into());
    }
    if old.xwayland != new.xwayland {
        c.restart.push("xwayland".into());
    }
    if old.xwayland_scale != new.xwayland_scale && old.xwayland == new.xwayland {
        c.restart.push("xwayland.scale".into());
    }
    if old.network != new.network {
        c.restart.push("network".into());
    }
    if old.open_urls != new.open_urls && old.dbus == new.dbus {
        c.restart.push("links".into());
    }
    let cache_tmpfs = |p: &Plan| {
        p.mounts.contains(&Mount::Tmpfs {
            dst: p.home.join(".cache"),
            mode: 0o700,
        })
    };
    if !c.restart.iter().any(|r| r == "home") && cache_tmpfs(old) != cache_tmpfs(new) {
        c.restart.push("home.cache".into());
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
    // Home, network, Xwayland, links, and dbus changes show up as mount
    // changes too; report them once.
    if c.restart.iter().any(|r| r == "xwayland") {
        let x11 = format!("mount at {}", crate::x11::SOCKET_DIR);
        let bin = format!("mount at {PSBOX_DIR}/bin/");
        c.restart.retain(|r| *r != x11 && !r.starts_with(&bin));
    }
    if c.restart.iter().any(|r| r == "links" || r == "dbus") {
        let handler = format!("mount at {PSBOX_DIR}/");
        c.restart.retain(|r| !r.starts_with(&handler));
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
    /// Builds the plan for `id`. A task gets its own config, which is the
    /// sandbox's with the task's overrides.
    pub fn build(id: &Identity, spec: &Spec, host: &HostEnv) -> Result<Self> {
        let name = id.sandbox();
        let unnamed = matches!(id, Identity::Unnamed { .. });
        let (spec, apps, default_app) = match id {
            Identity::Task { task, .. } => {
                let t = spec.task.get(task).with_context(|| format!("no task named {task:?}"))?;
                let portal_id = t.app.as_ref().and_then(|a| match spec.app.get(a) {
                    Some(AppSpec::Application(app)) => Some(app.id.clone()),
                    _ => None,
                });
                let apps = BTreeMap::from([(
                    task.clone(),
                    App {
                        run: t.command.clone(),
                        env: BTreeMap::new(),
                        id: portal_id,
                    },
                )]);
                (t.spec.as_ref(), apps, Some(task.clone()))
            }
            Identity::Instance { .. } | Identity::Unnamed { .. } => {
                let apps: BTreeMap<String, App> = spec
                    .app
                    .iter()
                    .filter_map(|(k, a)| match a {
                        AppSpec::Application(a) => Some((
                            k.clone(),
                            App {
                                run: a.run.clone(),
                                env: a.environment.clone(),
                                id: Some(a.id.clone()),
                            },
                        )),
                        _ => None,
                    })
                    .collect();
                let default_app = apps.contains_key(name).then(|| name.to_string());
                (spec, apps, default_app)
            }
        };
        let mut problems = Vec::new();
        let (network, network_fallback) = match resolve_network(spec.network, host.pasta) {
            Some(r) => r,
            None => {
                problems.push(Problem::Setting {
                    setting: match unnamed {
                        true => "-c network.mode=isolated".into(),
                        false => "network.mode: 'isolated'".into(),
                    },
                    needs: "pasta".into(),
                    detail: "not installed (package passt)".into(),
                });
                (Network::Isolated, false)
            }
        };
        let display = spec.display;
        let home = host.home.clone();
        let rt = host.runtime_dir.clone();
        let classes = spec.devices.clone();
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
            Network::Loopback => {}
        }
        mounts.push(ro("/run/udev", true));
        mounts.push(Mount::Tmpfs {
            dst: rt.clone(),
            mode: 0o700,
        });

        let mut env = BTreeMap::new();
        let wayland_socket = host.wayland_socket.as_ref().filter(|_| display);
        let xwayland = match wayland_socket {
            Some(_) => spec.xwayland.map(|x| x.mode),
            None => None,
        };
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
            Some(Home::Path { path, .. }) => Some(path.clone()),
            Some(Home::Temporary) | None => None,
        };
        match &spec.home {
            Some(Home::Path { path, cache }) => {
                mounts.push(Mount::Bind {
                    src: path.clone(),
                    dst: home.clone(),
                    ro: false,
                    optional: false,
                });
                if *cache == Cache::Temporary {
                    mounts.push(Mount::Tmpfs {
                        dst: home.join(".cache"),
                        mode: 0o700,
                    });
                }
            }
            Some(Home::Temporary) => {
                mounts.push(Mount::Tmpfs {
                    dst: home.clone(),
                    mode: 0o700,
                });
            }
            None => {}
        }

        // Mounts, labeled by their name.
        let mut sources = Vec::new();
        let mut shares = Vec::new();
        for (mname, m) in &spec.mount {
            let label = entry_label(mname, unnamed);
            if let Some(path) = m.source.as_ref().filter(|_| !m.optional) {
                sources.push(Source {
                    path: path.clone(),
                    entry: mname.clone(),
                    dir: m.create,
                });
            }
            let dst = m.destination.clone();
            if dst == Path::new("/") || dst.starts_with("/proc") || dst.starts_with("/dev") {
                problems.push(Problem::Config {
                    entry: label,
                    message: format!("cannot mount at {} (psbox sets up /, /proc, and /dev)", dst.display()),
                });
                continue;
            }
            shares.push(match (m.mode, &m.source) {
                (MountMode::ReadOnly | MountMode::ReadWrite, Some(src)) => Mount::Bind {
                    src: src.clone(),
                    dst,
                    ro: m.mode == MountMode::ReadOnly,
                    optional: m.optional,
                },
                (MountMode::Overlay, Some(src)) => Mount::Overlay {
                    lower: src.clone(),
                    dst,
                    optional: m.optional,
                },
                (MountMode::Empty, _) | (_, None) => Mount::Tmpfs { dst, mode: 0o755 },
            });
        }
        let dbus = spec.dbus.as_ref().map(|d| d.mode);
        if dbus == Some(DbusMode::Host) {
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
                        true => "-c dbus.mode=host".into(),
                        false => "dbus.mode: 'host'".into(),
                    },
                    needs: "a unix:path= session bus address".into(),
                    detail: "DBUS_SESSION_BUS_ADDRESS has none".into(),
                }),
            }
        }
        // The link handler (see below) is made before the shares, so its
        // files are always created in the sandbox root, never in a share.
        let open_urls: Vec<String> = match dbus {
            None => Vec::new(),
            Some(_) => spec.links.host_schemes().into_iter().map(String::from).collect(),
        };
        let handler = !open_urls.is_empty();
        if handler || xwayland == Some(XwaylandMode::Builtin) {
            mounts.push(Mount::Bind {
                src: host.exe.clone(),
                dst: format!("{PSBOX_DIR}/bin/psbox").into(),
                ro: true,
                optional: false,
            });
        }
        if handler {
            let (mimeapps, entry) = link_handler(&open_urls);
            mounts.push(Mount::File {
                dst: format!("{PSBOX_DIR}/config/mimeapps.list").into(),
                content: mimeapps,
            });
            mounts.push(Mount::File {
                dst: format!("{PSBOX_DIR}/share/applications/psbox-open-uri.desktop").into(),
                content: entry,
            });
        }
        // Parents before children, after everything psbox mounts itself.
        shares.sort_by_key(|m| m.dst().components().count());
        mounts.extend(shares);
        if xwayland.is_some() {
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
        let bus = (dbus == Some(DbusMode::Portal)).then(|| BusPaths::new(&rt));
        if let Some(bus) = &bus {
            env.insert("DBUS_SESSION_BUS_ADDRESS".into(), bus.address());
        }
        // GLib and xdg-open run a link's local handler before trying the
        // portal, and the host's desktop entries (such as a browser's or a
        // mail client's) are visible in /usr. A handler of the sandbox's
        // own, found before them, sends the links `links` sends to the host
        // to the OpenURI portal instead, and so to the host's default app.
        if handler {
            env.insert("XDG_CONFIG_DIRS".into(), format!("{PSBOX_DIR}/config:/etc/xdg"));
            env.insert(
                "XDG_DATA_DIRS".into(),
                format!("{PSBOX_DIR}/share:/usr/local/share:/usr/share"),
            );
        }
        env.extend(spec.environment.clone());

        Ok(Self {
            key: id.key(),
            host_home_dir,
            home,
            mounts,
            audio: classes.contains(&DeviceClass::Audio),
            node_classes: classes.into_iter().filter(|c| c.is_node()).collect(),
            env,
            exec_identity: default_app
                .as_ref()
                .and_then(|a| apps.get(a))
                .and_then(|a| a.id.clone()),
            apps,
            default_app,
            idle_timeout: Duration::from_secs(spec.idle_timeout),
            dbus,
            dbus_talk: match &bus {
                Some(_) => spec.dbus.as_ref().map(|d| d.talk.clone()).unwrap_or_default(),
                None => Vec::new(),
            },
            bus,
            xwayland,
            xwayland_scale: match xwayland {
                Some(XwaylandMode::Builtin) => spec.xwayland.map(|x| x.scale).unwrap_or_default(),
                _ => XwaylandScale::Native,
            },
            open_urls,
            network,
            network_fallback,
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
        if let Some(mode) = self.xwayland {
            for program in ["xwayland-satellite", "Xwayland"]
                .iter()
                .filter(|p| find_in_path(p).is_none())
            {
                problems.push(Problem::Setting {
                    setting: match unnamed {
                        true => format!("-c xwayland.mode={mode}"),
                        false => format!("xwayland.mode: '{mode}'"),
                    },
                    needs: (*program).into(),
                    detail: "not installed".into(),
                });
            }
        }
        if self.bus.is_some() && find_in_path("dbus-daemon").is_none() {
            problems.push(Problem::Setting {
                setting: match unnamed {
                    true => "-c dbus.mode=portal".into(),
                    false => "dbus.mode: 'portal'".into(),
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
}

/// What makes GLib, GTK 3, GTK 4, and GNotification use portals: each has
/// its own switch. A launch with a portal identity gets these; one without
/// gets none of them, so toolkits fall back to their own dialogs.
pub const PORTAL_ENV: [(&str, &str); 4] = [
    ("GIO_USE_PORTALS", "1"),
    ("GTK_USE_PORTAL", "1"),
    ("GDK_DEBUG", "portals"),
    ("GNOTIFICATION_BACKEND", "portal"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn main(name: &str) -> Identity {
        Identity::Instance {
            sandbox: name.into(),
            instance: None,
        }
    }

    fn task(name: &str) -> Identity {
        Identity::Task {
            sandbox: "app".into(),
            task: name.into(),
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
            exe: "/usr/bin/psbox".into(),
        }
    }

    /// The config of the sandbox `app`, with `psbox` in scope.
    fn spec(text: &str) -> Spec {
        crate::spec::parse(&format!("local psbox = import 'psbox';\n{text}")).unwrap_or_else(|e| panic!("{e:#}"))
    }

    fn build(text: &str) -> Plan {
        Plan::build(&main("app"), &spec(text), &host()).unwrap()
    }

    #[test]
    fn dbus_talk_reaches_the_plan_only_with_a_private_bus() {
        let talk = |text: &str| build(text).dbus_talk;
        assert!(talk("{}").is_empty());
        assert_eq!(talk("{ dbus+: { talk: ['org.a.b'] } }"), ["org.a.b"]);
        assert_eq!(
            talk("{ dbus+: { talk: ['org.a.b', 'org.c.d', 'org.a.b'] } }"),
            ["org.a.b", "org.c.d"]
        );
        // Tasks inherit the bus, unless their overrides remove it.
        let s = spec(
            "{ dbus+: { talk: ['org.a.b'] },
               task+: { t: { run: ['x'] }, quiet: { run: ['x'], overrides: { dbus: null } } } }",
        );
        assert_eq!(Plan::build(&task("t"), &s, &host()).unwrap().dbus_talk, ["org.a.b"]);
        assert!(Plan::build(&task("quiet"), &s, &host()).unwrap().dbus_talk.is_empty());
        let old = build("{}");
        let new = Plan::build(&main("app"), &s, &host()).unwrap();
        assert!(diff(&old, &new).restart.contains(&"dbus.talk".to_string()));
    }

    #[test]
    fn default_plan_binds_private_home_and_wayland() {
        let plan = build("{}");
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
        // Portal switches come with a launch's portal identity.
        assert!(!plan.env.contains_key("GTK_USE_PORTAL"));
        let off = build("{ dbus: null }");
        assert!(off.bus.is_none() && off.dbus.is_none());
        assert!(!off.env.contains_key("DBUS_SESSION_BUS_ADDRESS"));
        assert_eq!(plan.xwayland, None);
        let x = spec("{ xwayland: {} }");
        assert_eq!(
            Plan::build(&main("app"), &x, &host()).unwrap().xwayland,
            Some(XwaylandMode::Builtin)
        );
        let headless = HostEnv {
            wayland_socket: None,
            ..host()
        };
        assert_eq!(Plan::build(&main("app"), &x, &headless).unwrap().xwayland, None);
        assert_eq!(plan.node_classes, vec![DeviceClass::Gpu]);
        assert_eq!(plan.default_app.as_deref(), Some("app"));
        assert_eq!(plan.apps["app"].run, ["app"]);
        assert_eq!(plan.exec_identity.as_deref(), Some("app"));
    }

    #[test]
    fn apps_carry_their_command_environment_and_id() {
        let plan = build(
            "{ app+: { code: { run: ['code'], environment: { A: '1' }, id: 'code-oss' },
                       'code-autostart': { kind: 'autostart', app: 'code' } } }",
        );
        assert_eq!(plan.apps["code"].run, ["code"]);
        assert_eq!(plan.apps["code"].env["A"], "1");
        assert_eq!(plan.apps["code"].id.as_deref(), Some("code-oss"));
        assert!(
            !plan.apps.contains_key("code-autostart"),
            "autostart entries are not runnable"
        );
        let removed = build("{ app+: { app: null, code: { run: ['code'] } } }");
        assert_eq!(removed.default_app, None);
        assert_eq!(removed.exec_identity, None);
    }

    #[test]
    fn shares_are_ordered_parents_first() {
        let s = crate::spec::parse_named(
            "retro",
            "local psbox = import 'psbox';
             { mount+: {
                 saves: psbox.mount.readWrite(psbox.path([psbox.home, 'Games', 'saves'])),
                 games: psbox.mount.readOnly(psbox.path([psbox.home, 'Games'])),
                 bin: psbox.mount.overlay(psbox.storageFor('bin'), destination='/opt/app'),
             } }",
            &[],
        )
        .unwrap();
        let plan = Plan::build(&main("retro"), &s, &host()).unwrap();
        let at = |dst: &str| plan.mounts.iter().position(|m| m.dst() == Path::new(dst)).unwrap();
        assert!(at("/home/u/Games") < at("/home/u/Games/saves"));
        assert!(plan.mounts.contains(&Mount::Overlay {
            lower: "/home/u/Sandbox/retro.bin".into(),
            dst: "/opt/app".into(),
            optional: false
        }));
    }

    #[test]
    fn no_audio_without_classes() {
        let plan = build("{ devices: psbox.devices.none + { camera: true } }");
        assert!(!plan.mounts.iter().any(|m| m.dst().ends_with("pipewire-0")));
        assert_eq!(plan.node_classes, vec![DeviceClass::Camera]);
    }

    #[test]
    fn rejects_dev_and_proc_targets() {
        let p = build("{ mount+: { bus: { source: '/dev/bus' }, sys: { source: '/proc/sys' } } }");
        let config = |entry: &str, dst: &str| Problem::Config {
            entry: entry.into(),
            message: format!("cannot mount at {dst} (psbox sets up /, /proc, and /dev)"),
        };
        assert_eq!(
            p.problems,
            [config("mount.bus", "/dev/bus"), config("mount.sys", "/proc/sys")]
        );
        assert!(!p.mounts.iter().any(|m| m.dst().starts_with("/proc/sys")));
    }

    #[test]
    fn examples_parse_and_plan() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "jsonnet") {
                continue;
            }
            let name = path.file_stem().unwrap().to_str().unwrap().to_string();
            let text = std::fs::read_to_string(&path).unwrap();
            let spec =
                crate::spec::parse_named(&name, &text, &[]).unwrap_or_else(|e| panic!("{}: {e:#}", path.display()));
            let plan =
                Plan::build(&main(&name), &spec, &host()).unwrap_or_else(|e| panic!("{}: {e:#}", path.display()));
            assert!(plan.default_app.is_some() || !plan.apps.is_empty(), "{name}");
            assert!(plan.problems.is_empty(), "{name}: {:?}", plan.problems);
            for t in spec.task.keys() {
                let id = Identity::Task {
                    sandbox: name.clone(),
                    task: t.clone(),
                };
                let p = Plan::build(&id, &spec, &host()).unwrap_or_else(|e| panic!("{name} task {t}: {e:#}"));
                assert!(p.problems.is_empty(), "{name} task {t}: {:?}", p.problems);
            }
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
        let plan = build("{ dbus+: { mode: 'host' }, display: null }");
        assert!(plan.bus.is_none());
        assert_eq!(plan.dbus, Some(DbusMode::Host));
        assert_eq!(plan.env["DBUS_SESSION_BUS_ADDRESS"], "unix:path=/run/user/1000/bus");
        assert!(plan.mounts.contains(&Mount::Bind {
            src: "/run/user/1000/bus".into(),
            dst: "/run/user/1000/bus".into(),
            ro: true,
            optional: false
        }));
        assert!(plan.mounts.iter().any(|m| m.dst() == Path::new("/run/dbus")));
        assert!(plan.xwayland.is_none() && !plan.env.contains_key("WAYLAND_DISPLAY"));

        let elsewhere = HostEnv {
            session_bus: unix_path("unix:abstract=/x;unix:guid=1,path=/tmp/test%20bus"),
            ..host()
        };
        let host_bus = spec("{ dbus+: { mode: 'host' } }");
        let p = Plan::build(&main("app"), &host_bus, &elsewhere).unwrap();
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
        let p = Plan::build(&main("app"), &host_bus, &no_bus).unwrap();
        assert!(matches!(&p.problems[..], [Problem::Setting { setting, .. }] if setting == "dbus.mode: 'host'"));

        let portal = build("{ xwayland: { mode: 'xwayland-satellite' } }");
        assert_eq!(
            diff(&portal, &plan).restart,
            vec!["dbus".to_string(), "xwayland".to_string()]
        );
    }

    #[test]
    fn tasks_inherit_the_sandbox_unless_their_overrides_change_it() {
        let s = crate::spec::parse_named(
            "retroarch",
            "local psbox = import 'psbox';
             { mount+: { bin: psbox.mount.readOnly(psbox.storageFor('bin'), destination=psbox.path([psbox.home, '.bin'])) },
               task+: {
                 update: { script: 'true', app: 'retroarch',
                           overrides: { home: { mode: 'temporary' }, mount+: { bin+: { mode: 'read-write' } } } },
                 quiet: { run: ['x'], overrides: { dbus: null, display: null, devices: psbox.devices.none } },
               } }",
            &[],
        )
        .unwrap();
        let id = |t: &str| Identity::Task {
            sandbox: "retroarch".into(),
            task: t.into(),
        };
        let plan = Plan::build(&id("update"), &s, &host()).unwrap();
        assert_eq!(plan.key, "retroarch.task-update");
        // The sandbox's bus, display, and devices come along.
        assert!(plan.bus.is_some() && plan.env.contains_key("WAYLAND_DISPLAY"));
        assert_eq!(plan.node_classes, vec![DeviceClass::Gpu]);
        assert!(plan.mounts.contains(&Mount::Tmpfs {
            dst: "/home/u".into(),
            mode: 0o700
        }));
        assert!(
            plan.mounts
                .iter()
                .any(|m| matches!(m, Mount::Bind { dst, ro: false, .. } if dst == Path::new("/home/u/.bin")))
        );
        assert!(plan.idle_timeout.is_zero());
        assert_eq!(plan.default_app.as_deref(), Some("update"));
        assert_eq!(plan.apps["update"].run, ["bash", "-c", "true"]);
        // `app` lends the task an app's portal identity.
        assert_eq!(plan.exec_identity.as_deref(), Some("retroarch"));

        let quiet = Plan::build(&id("quiet"), &s, &host()).unwrap();
        assert!(quiet.bus.is_none() && quiet.xwayland.is_none());
        assert!(!quiet.env.contains_key("WAYLAND_DISPLAY"));
        assert!(
            !quiet
                .mounts
                .iter()
                .any(|m| m.dst().ends_with("wayland-0") || m.dst().ends_with("pipewire-0"))
        );
        assert!(quiet.node_classes.is_empty());
        assert_eq!(quiet.exec_identity, None);
        assert!(Plan::build(&id("nope"), &s, &host()).is_err());
    }

    #[test]
    fn network_modes() {
        let isolated = build("{ network: { mode: 'isolated' } }");
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
        assert_eq!(
            diff(&build("{ network: { mode: 'host' } }"), &isolated).restart,
            vec!["network"]
        );
        let loopback = build("{ network: null }");
        assert_eq!(loopback.network, Network::Loopback);
        assert!(!loopback.mounts.iter().any(|m| m.dst() == Path::new("/etc/resolv.conf")));
    }

    #[test]
    fn network_defaults_to_isolated_when_pasta_is_installed() {
        let no_pasta = HostEnv { pasta: false, ..host() };
        let with = |text: &str, host: &HostEnv| Plan::build(&main("app"), &spec(text), host);

        let p = with("{}", &host()).unwrap();
        assert_eq!((p.network, p.network_fallback), (Network::Isolated, false));
        let p = with("{}", &no_pasta).unwrap();
        assert_eq!((p.network, p.network_fallback), (Network::Host, true));
        assert!(p.mounts.iter().any(|m| m.dst() == Path::new("/run/systemd/resolve")));
        let p = with("{ network: { mode: 'automatic' } }", &no_pasta).unwrap();
        assert_eq!((p.network, p.network_fallback), (Network::Host, true));
        let p = with("{ network: { mode: 'host' } }", &no_pasta).unwrap();
        assert_eq!((p.network, p.network_fallback), (Network::Host, false));
        let p = with("{ network: null }", &no_pasta).unwrap();
        assert_eq!((p.network, p.network_fallback), (Network::Loopback, false));

        let pasta = |setting: &str| Problem::Setting {
            setting: setting.into(),
            needs: "pasta".into(),
            detail: "not installed (package passt)".into(),
        };
        let p = with("{ network: { mode: 'isolated' } }", &no_pasta).unwrap();
        assert_eq!(p.problems, [pasta("network.mode: 'isolated'")]);
        let unnamed = Identity::Unnamed { id: "0".repeat(16) };
        let p = Plan::build(&unnamed, &spec("{ network: { mode: 'isolated' } }"), &no_pasta).unwrap();
        assert_eq!(p.problems, [pasta("-c network.mode=isolated")]);

        // Tasks inherit the sandbox's setting, explicit or not.
        let in_task = |text: &str| {
            Plan::build(
                &task("t"),
                &spec(&format!("{text} + {{ task+: {{ t: {{ run: ['x'] }} }} }}")),
                &no_pasta,
            )
        };
        assert!(in_task("{}").unwrap().network_fallback);
        assert_eq!(in_task("{ network: { mode: 'isolated' } }").unwrap().problems.len(), 1);
        assert!(!in_task("{ network: { mode: 'host' } }").unwrap().network_fallback);
    }

    #[test]
    fn xwayland_modes() {
        let bin = |p: &Plan| p.mounts.iter().any(|m| m.dst() == Path::new("/run/psbox/bin/psbox"));
        let x11 = |p: &Plan| p.mounts.iter().any(|m| m.dst() == Path::new(crate::x11::SOCKET_DIR));

        // The builtin mode runs psbox's own decorations inside the sandbox,
        // so it needs psbox there even without a link handler.
        let builtin = build("{ dbus: null, xwayland: {} }");
        assert!(bin(&builtin) && x11(&builtin));
        let satellite = build("{ dbus: null, xwayland: { mode: 'xwayland-satellite' } }");
        assert!(!bin(&satellite) && x11(&satellite) && satellite.problems.is_empty());
        let off = build("{ dbus: null }");
        assert!(!bin(&off) && !x11(&off));
        assert_eq!(diff(&builtin, &satellite).restart, ["xwayland"]);
        let round_up = build("{ dbus: null, xwayland: { scale: 'round-up' } }");
        assert_eq!(round_up.xwayland_scale, XwaylandScale::RoundUp);
        assert_eq!(diff(&builtin, &round_up).restart, ["xwayland.scale"]);
        // A display set to null takes Xwayland with it.
        assert!(build("{ display: null, xwayland: {} }").xwayland.is_none());
    }

    #[test]
    fn links_open_on_the_host_through_the_sandboxs_own_handler() {
        let file = |p: &Plan, name: &str| {
            p.mounts.iter().find_map(|m| match m {
                Mount::File { dst, content } if dst.ends_with(name) => Some(content.clone()),
                _ => None,
            })
        };
        let claims = |p: &Plan| {
            let list = file(p, "mimeapps.list").unwrap_or_default();
            ["http", "https", "mailto"]
                .into_iter()
                .filter(|s| list.contains(&format!("x-scheme-handler/{s}=psbox-open-uri.desktop\n")))
                .collect::<Vec<_>>()
        };

        let plan = build("{}");
        assert_eq!(plan.open_urls, ["http", "https", "mailto"]);
        assert_eq!(claims(&plan), ["http", "https", "mailto"]);
        assert!(plan.mounts.contains(&Mount::Bind {
            src: "/usr/bin/psbox".into(),
            dst: "/run/psbox/bin/psbox".into(),
            ro: true,
            optional: false,
        }));
        let entry = file(&plan, "psbox-open-uri.desktop").unwrap();
        assert!(entry.contains(&format!("Exec={PSBOX_DIR}/bin/psbox open-uri -- %u\n")));
        assert!(entry.contains("MimeType=x-scheme-handler/http;x-scheme-handler/https;x-scheme-handler/mailto;\n"));
        assert_eq!(plan.env["XDG_CONFIG_DIRS"], "/run/psbox/config:/etc/xdg");
        assert_eq!(
            plan.env["XDG_DATA_DIRS"],
            "/run/psbox/share:/usr/local/share:/usr/share"
        );
        assert_eq!(build("{ dbus+: { mode: 'host' } }").open_urls.len(), 3);

        // A share above the handler's directory comes after its files, so
        // they are never created in the share.
        let shared = build("{ mount+: { run: psbox.mount.readOnly('/run') } }");
        let at = |dst: &str| shared.mounts.iter().position(|m| m.dst() == Path::new(dst)).unwrap();
        assert!(at("/run/psbox/share/applications/psbox-open-uri.desktop") < at("/run"));

        // Browsers handle their own web links, and mail clients their own
        // mail links; the rest still go to the host. Profiles merge the
        // section key by key.
        for (text, host) in [
            ("psbox.profiles.browser", vec!["mailto"]),
            ("{ links+: { mailto: 'sandbox' } }", vec!["http", "https"]),
            ("psbox.profiles.browser + { links+: { mailto: 'sandbox' } }", vec![]),
            (
                "psbox.profiles.browser + { links+: { https: 'host' } }",
                vec!["https", "mailto"],
            ),
        ] {
            let p = build(text);
            assert_eq!(p.open_urls, host, "{text}");
            assert_eq!(claims(&p), host, "{text}");
            if !host.is_empty() {
                let types: String = host.iter().map(|s| format!("x-scheme-handler/{s};")).collect();
                let entry = file(&p, "psbox-open-uri.desktop").unwrap();
                assert!(entry.contains(&format!("MimeType={types}\n")), "{text}");
            }
        }
        assert!(crate::spec::parse("{ links+: { ftp: 'host' } }").is_err());
        assert!(crate::spec::parse("{ links+: { http: 'elsewhere' } }").is_err());

        // Without a bus there is no portal to send links to, and with every
        // link kept inside there is nothing to send.
        for text in [
            "{ dbus: null }",
            "psbox.profiles.browser + { links+: { mailto: 'sandbox' } }",
        ] {
            let p = build(text);
            assert!(p.open_urls.is_empty(), "{text}");
            assert!(!p.mounts.iter().any(|m| m.dst().starts_with(PSBOX_DIR)), "{text}");
            assert!(!p.env.contains_key("XDG_DATA_DIRS"), "{text}");
        }

        assert_eq!(
            build("{ environment+: { XDG_DATA_DIRS: '/x' } }").env["XDG_DATA_DIRS"],
            "/x",
            "the config's environment wins"
        );
        let c = diff(&plan, &build("{ links+: { http: 'sandbox' } }"));
        assert_eq!(c.restart, vec!["links"]);
    }

    #[test]
    fn diffs_live_and_restart_changes() {
        let dl = "{ mount+: { dl: psbox.mount.readWrite(psbox.path([psbox.home, 'Downloads'])) } }";
        let with = |extra: &str| build(&format!("{dl} + {extra}"));
        let base = build(dl);

        let c = diff(
            &base,
            &with(
                "{ mount+: { music: psbox.mount.readWrite(psbox.path([psbox.home, 'Music'])),
                             ssh: psbox.mount.empty('/etc/ssh') } }",
            ),
        );
        assert!(c.restart.is_empty() && c.detach.is_empty());
        assert_eq!(c.attach.len(), 2);

        let c = diff(&base, &build("{}"));
        assert_eq!(c.detach.len(), 1);
        assert!(c.attach.is_empty());

        let c = diff(&base, &with("{ devices+: { audio: false } }"));
        assert_eq!(c.detach.len(), 2, "audio sockets go away live");

        assert!(diff(&base, &with("{ environment+: { A: '1' } }")).is_empty());

        let c = diff(&base, &with("{ home: { mode: 'temporary' } }"));
        assert_eq!(c.restart, vec!["home"]);
        assert!(c.attach.is_empty() && c.detach.is_empty());

        let c = diff(&base, &with("{ dbus: null, xwayland: {} }"));
        assert_eq!(c.restart, vec!["dbus", "xwayland"]);

        let c = diff(
            &base,
            &with("{ mount+: { o: psbox.mount.overlay('/srv/x', destination='/opt/x') } }"),
        );
        assert_eq!(c.restart, vec!["mount at /opt/x"]);

        assert_eq!(
            diff(&base, &with("{ home+: { cache: 'persistent' } }")).restart,
            vec!["home.cache"]
        );
    }

    #[test]
    fn diff_reattaches_mounts_beneath_changed_ones() {
        let private = "{ mount+: { private: psbox.mount.empty('/data/private') } }";
        let base = build(&format!("{private} + {{ mount+: {{ data: {{ source: '/data' }} }} }}"));
        let moved = "{ mount+: { data: { source: '/srv/data', destination: '/data' } } }";

        let c = diff(&base, &build(&format!("{private} + {moved}")));
        let dsts = |ms: &[Mount]| ms.iter().map(|m| m.dst().to_path_buf()).collect::<Vec<_>>();
        assert!(c.restart.is_empty());
        assert_eq!(dsts(&c.detach), [PathBuf::from("/data/private"), "/data".into()]);
        assert_eq!(dsts(&c.attach), [PathBuf::from("/data"), "/data/private".into()]);

        let tmp = |src: &str| {
            build(&format!(
                "{{ xwayland: {{}}, mount+: {{ tmp: {{ source: '{src}', destination: '/tmp' }} }} }}"
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
            "{{ network: {{ mode: 'isolated' }}, mount+: {{
                 share: {{ source: '{d}/missing-share' }},
                 data: {{ source: '{d}/missing-data' }},
                 extra: {{ source: '{d}/missing-extra', mode: 'read-only', optional: true }},
                 base: {{ source: '{d}/ro', destination: '/srv/base', mode: 'read-only' }},
                 inner: psbox.mount.empty('/srv/base/inner'),
               }} }}"
        );
        let no_pasta = HostEnv { pasta: false, ..host() };
        let mut plan = Plan::build(&main("app"), &spec(&text), &no_pasta).unwrap();
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
                "D/missing-data mount.data",
                "D/missing-share mount.share",
                "→ add optional: true to mount.data and mount.share if they can be missing",
                "→ add create: true to have psbox create missing directories in /home/u/Sandbox, and elsewhere with --create-dirs",
                "",
                "SETTING NEEDS",
                "network.mode: 'isolated' pasta · not installed (package passt)",
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
            entry: mount.into(),
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
                "  → add optional: true to the mounts that can be missing",
                "  → add create: true to have psbox create missing directories in /data",
                "  → run with --create-dirs to create the ones shown with a trailing /",
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
            "  → --create-dirs only creates the sources of mounts with create: true: add it to have them created"
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
             → add create: true to have psbox create missing directories in /home/u/Sandbox, and elsewhere with --create-dirs"
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
        let mut plan = build(&format!(
            "{{ mount+: {{ good: {{ source: '{d}/good', create: true }}, broken: {{ source: '{d}/broken', create: true }} }} }}"
        ));
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
                entry: "broken".into(),
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
        let plan = build(
            "{ mount+: {
                 projects: psbox.mount.readWrite(psbox.path([psbox.home, 'Projects']), create=true),
                 data: { source: psbox.path([psbox.storage, 'data']), destination: '/data', create: true },
               } }",
        );
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
        let r = root.display();
        let s = spec(&format!(
            "{{ mount+: {{ data: {{ source: '{r}/app/data/new', destination: '/data', create: true }} }} }}"
        ));
        let mut plan = Plan::build(&main("app"), &s, &host).unwrap();
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
    fn create_marks_directories_to_create() {
        let dir = std::env::temp_dir().join(format!("psbox-dirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("Sandbox");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("file"), "").unwrap();
        let host = HostEnv {
            root: root.clone(),
            ..host()
        };
        let (d, r) = (dir.display(), root.display());
        let s = spec(&format!(
            "{{ mount+: {{
                 bin: {{ source: '{r}/app.bin', create: true }},
                 plain: {{ source: '{r}/plain' }},
                 file: {{ source: '{r}/file', create: true }},
                 outside: {{ source: '{d}/outside', create: true }},
                 opt: {{ source: '{r}/app.opt', optional: true }},
               }} }}"
        ));
        let mut plan = Plan::build(&main("app"), &s, &host).unwrap();
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
                Problem::Config {
                    entry: "mount.file".into(),
                    message: format!("{} isn't a directory", root.join("file").display()),
                },
                Problem::Missing {
                    path: dir.join("outside"),
                    entry: "outside".into(),
                    dir: true
                },
                Problem::Missing {
                    path: root.join("plain"),
                    entry: "plain".into(),
                    dir: false
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
