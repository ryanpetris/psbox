//! Sandbox configs: what an evaluated config ([`crate::jsonnet`]) means.
//!
//! A config is `$XDG_CONFIG_HOME/psbox/sandboxes/<name>.jsonnet`; the file
//! name is the sandbox name. Evaluation yields JSON, which this module
//! checks strictly (unknown keys, wrong types, keys the chosen mode does not
//! use) and completes with the defaults of every present section. Every
//! problem is reported, by the key path it is at.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Map, Value};

use crate::jsonnet::{self, Layer, LoadedFile};
use crate::plan::HostEnv;

/// A sandbox config, complete with defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    /// Seconds with no workload before the sandbox stops.
    pub idle_timeout: u64,
    /// `None`: no home.
    pub home: Option<Home>,
    /// The device classes that are on.
    pub devices: Vec<DeviceClass>,
    /// `None`: no session bus.
    pub dbus: Option<Dbus>,
    pub network: NetworkSetting,
    /// Wayland (and Xwayland when `xwayland` is set).
    pub display: bool,
    /// `None`: no X server.
    pub xwayland: Option<Xwayland>,
    pub links: Links,
    /// Extra environment for every process.
    pub environment: BTreeMap<String, String>,
    pub mount: BTreeMap<String, MountSpec>,
    /// Applications and autostart entries, by entry name.
    pub app: BTreeMap<String, AppSpec>,
    /// Oneshot tasks.
    pub task: BTreeMap<String, TaskSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Home {
    /// A host directory, bound read-write.
    Path { path: PathBuf, cache: Cache },
    /// An empty tmpfs.
    Temporary,
}

/// `home.cache`: whether `~/.cache` is an empty tmpfs or kept in the home.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cache {
    Temporary,
    Persistent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dbus {
    pub mode: DbusMode,
    /// Host session bus names the sandbox may call through the gateway
    /// (portal mode). Each is an opt-in to that host service, unfiltered.
    pub talk: Vec<String>,
}

/// `portal`: a private session bus with portals and notifications
/// forwarded to the host. `host`: the host session and system buses,
/// unfiltered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbusMode {
    Portal,
    Host,
}

/// The network a config asks for. `Automatic` is isolated when pasta is
/// installed and the host network otherwise (see
/// [`crate::plan::resolve_network`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkSetting {
    Automatic,
    Isolated,
    Host,
    Loopback,
}

/// The network a sandbox gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Network {
    Host,
    Isolated,
    /// `network: null`: loopback only.
    Loopback,
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Network::Host => "host",
            Network::Isolated => "isolated",
            Network::Loopback => "loopback",
        })
    }
}

/// How X11 apps run on a private Xwayland that `xwayland-satellite` serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Xwayland {
    pub mode: XwaylandMode,
    /// How X11 apps meet a scaled display (builtin mode).
    pub scale: XwaylandScale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XwaylandMode {
    /// With psbox drawing the title bars of X11 windows when the compositor
    /// does not (`psbox decorations`).
    Builtin,
    /// Satellite on its own, with its own title bars.
    Satellite,
}

impl fmt::Display for XwaylandMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            XwaylandMode::Builtin => "builtin",
            XwaylandMode::Satellite => "xwayland-satellite",
        })
    }
}

/// `xwayland.scale`: how X11 apps meet a scaled display. psbox's
/// decorations proxy changes the scale xwayland-satellite sees, so it needs
/// the builtin mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum XwaylandScale {
    /// Apps draw at the display's real pixels, told its scale, and scale
    /// themselves.
    #[default]
    Native,
    /// Apps are told the next whole scale and draw at it; the compositor
    /// shrinks the result to the display's scale.
    RoundUp,
    /// Apps draw unscaled; the compositor enlarges the result.
    Compositor,
}

impl fmt::Display for XwaylandScale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            XwaylandScale::Native => "native",
            XwaylandScale::RoundUp => "round-up",
            XwaylandScale::Compositor => "compositor",
        })
    }
}

impl FromStr for XwaylandScale {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "native" => Ok(XwaylandScale::Native),
            "round-up" => Ok(XwaylandScale::RoundUp),
            "compositor" => Ok(XwaylandScale::Compositor),
            _ => bail!("expected native, round-up, or compositor"),
        }
    }
}

/// `links`: the sandbox's default handler for each scheme the OpenURI
/// portal accepts. `host` (the default) sends links to the host's default
/// app through the portal; `sandbox` leaves them to whatever handles them
/// inside the sandbox. Apps that call the portal themselves are not
/// affected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Links {
    pub http: OpenIn,
    pub https: OpenIn,
    pub mailto: OpenIn,
}

impl Links {
    /// The schemes whose links go to the host.
    pub fn host_schemes(&self) -> Vec<&'static str> {
        [("http", self.http), ("https", self.https), ("mailto", self.mailto)]
            .into_iter()
            .filter(|(_, at)| *at == OpenIn::Host)
            .map(|(scheme, _)| scheme)
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenIn {
    Host,
    Sandbox,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountSpec {
    /// Host source; `None` for `empty`.
    pub source: Option<PathBuf>,
    pub destination: PathBuf,
    pub mode: MountMode,
    /// Skip silently when the source is missing.
    pub optional: bool,
    /// Create the source directory when it is missing.
    pub create: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MountMode {
    ReadWrite,
    ReadOnly,
    /// A read-only source with a private writable layer that is discarded.
    Overlay,
    /// An empty tmpfs that hides whatever is at the destination.
    Empty,
}

impl fmt::Display for MountMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            MountMode::ReadWrite => "read-write",
            MountMode::ReadOnly => "read-only",
            MountMode::Overlay => "overlay",
            MountMode::Empty => "empty",
        })
    }
}

/// Where a desktop entry goes: the applications or the autostart directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Application,
    Autostart,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Kind::Application => "application",
            Kind::Autostart => "autostart",
        })
    }
}

/// One `app` entry: exactly one desktop file, and for an application, how
/// to run it.
#[derive(Debug, Clone, PartialEq)]
pub enum AppSpec {
    Application(Application),
    Autostart(Autostart),
    /// A `Hidden=true` entry under `id`, hiding the host's.
    Suppress {
        kind: Kind,
        id: String,
    },
}

impl AppSpec {
    pub fn kind(&self) -> Kind {
        match self {
            AppSpec::Application(_) => Kind::Application,
            AppSpec::Autostart(_) => Kind::Autostart,
            AppSpec::Suppress { kind, .. } => *kind,
        }
    }

    /// The desktop file id.
    pub fn id(&self) -> &str {
        match self {
            AppSpec::Application(a) => &a.id,
            AppSpec::Autostart(a) => &a.id,
            AppSpec::Suppress { id, .. } => id,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Application {
    pub run: Vec<String>,
    pub environment: BTreeMap<String, String>,
    /// The desktop file id, also the app id registered with the portal.
    pub id: String,
    /// The host entry copied, when any.
    pub source_id: Option<String>,
    pub remove_arguments: Vec<String>,
    pub keys: BTreeMap<String, KeyValue>,
    /// Actions by name; `None` removes a copied action.
    pub actions: BTreeMap<String, Option<Action>>,
    /// `NoDisplay`: `Some(true)` hides it from menus, `Some(false)` removes
    /// a copied `NoDisplay`.
    pub no_display: Option<bool>,
    /// The implicit default app, which gets a desktop file only when it
    /// inherits a host entry.
    pub implicit: bool,
}

impl Application {
    /// Whether `psbox install` writes a desktop file for it.
    pub fn has_entry(&self) -> bool {
        !self.implicit || self.source_id.is_some()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Autostart {
    /// The application it starts.
    pub app: String,
    pub id: String,
    pub source_id: Option<String>,
    pub remove_arguments: Vec<String>,
    pub keys: BTreeMap<String, KeyValue>,
}

/// A Desktop Entry key's value.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyValue {
    /// Remove the key from a copied entry.
    Remove,
    Bool(bool),
    Str(String),
    /// Written as `a;b;`.
    List(Vec<String>),
    /// `{ default: 'x', de: 'y' }`: `Key=x`, `Key[de]=y`.
    Localized {
        default: String,
        locales: BTreeMap<String, String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    /// Arguments added after the app's `run`.
    pub arguments: Vec<String>,
    pub keys: BTreeMap<String, KeyValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskSpec {
    pub command: Vec<String>,
    /// The application whose portal identity the task uses.
    pub app: Option<String>,
    /// The task's config: the sandbox's with its overrides.
    pub spec: Box<Spec>,
}

/// A class of host devices or media access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceClass {
    /// DRM render nodes (`/dev/dri/renderD*`).
    Gpu,
    /// video4linux devices that can capture.
    Camera,
    /// Input devices udev marks as joysticks.
    Gamepad,
    /// Raw ALSA devices (`/dev/snd/*`), for apps that bypass PipeWire.
    Alsa,
    /// FIDO security keys (passkeys, U2F): the `hidraw` interfaces udev
    /// marks as security tokens from their HID report descriptor, and not
    /// the other interfaces of the same key (such as an OTP keyboard).
    SecurityKey,
    /// One USB device (any of its nodes: tty, usb, hidraw).
    Usb { vendor: u16, product: u16 },
    /// The host PipeWire and PulseAudio sockets. These are not filtered, so
    /// they allow playback, capture, and any PipeWire camera nodes.
    Audio,
}

impl DeviceClass {
    /// Whether this class is backed by `/dev` nodes (as opposed to sockets).
    pub fn is_node(self) -> bool {
        self != DeviceClass::Audio
    }
}

/// Parses a `devices.usb` entry, `<vendor>:<product>` in four hex digits
/// each.
fn usb_device(s: &str) -> std::result::Result<DeviceClass, String> {
    let (v, p) = s
        .split_once(':')
        .ok_or_else(|| format!("expected <vendor>:<product>, got {s:?}"))?;
    let hex = |x: &str| {
        if x.len() != 4 {
            return Err(format!("USB ids are four hex digits, got {x:?}"));
        }
        u16::from_str_radix(x, 16).map_err(|_| format!("invalid USB id {x:?}"))
    };
    Ok(DeviceClass::Usb {
        vendor: hex(v)?,
        product: hex(p)?,
    })
}

impl fmt::Display for DeviceClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceClass::Gpu => f.write_str("gpu"),
            DeviceClass::Camera => f.write_str("camera"),
            DeviceClass::Gamepad => f.write_str("gamepad"),
            DeviceClass::Alsa => f.write_str("alsa"),
            DeviceClass::SecurityKey => f.write_str("security-key"),
            DeviceClass::Usb { vendor, product } => write!(f, "usb:{vendor:04x}:{product:04x}"),
            DeviceClass::Audio => f.write_str("audio"),
        }
    }
}

/// Sandbox names: letters, digits, `_`, `-`. Never a `.`, because
/// `<root>/<name>.<type>` directories belong to the sandbox `<name>`.
pub fn validate_sandbox_name(name: &str) -> Result<()> {
    if !valid_word(name) || name.starts_with('-') {
        bail!("invalid sandbox name {name:?}: use letters, digits, '_', '-'");
    }
    // Unnamed sandboxes are keyed `sandbox.<id>` and use `<root>/sandbox`.
    if name == crate::plan::UNNAMED {
        bail!("{name:?} is reserved for unnamed sandboxes");
    }
    Ok(())
}

/// App, mount, and task names.
pub fn validate_name(kind: &str, name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name != "."
        && name != ".."
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !ok {
        bail!("invalid {kind} name {name:?}: use letters, digits, '.', '_', '-'");
    }
    Ok(())
}

/// A config that does not evaluate or validate.
#[derive(Debug)]
pub struct ConfigError {
    /// The sandbox, when the config is a named one.
    pub name: Option<String>,
    pub file: Option<PathBuf>,
    pub problem: ConfigProblem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigProblem {
    /// Jsonnet syntax or evaluation: `location` is `file:line:column` and
    /// `excerpt` the source line with a caret, when known.
    Parse {
        location: Option<String>,
        message: String,
        excerpt: Vec<String>,
    },
    /// Every validation error, as (key path, message).
    Invalid(Vec<(String, String)>),
}

impl ConfigError {
    /// One line about the error, for lists.
    pub fn summary(&self) -> String {
        match &self.problem {
            ConfigProblem::Parse {
                location: Some(at),
                message,
                ..
            } => format!("{at}: {message}"),
            ConfigProblem::Parse { message, .. } => message.clone(),
            ConfigProblem::Invalid(errors) => match errors.as_slice() {
                [(entry, message)] => format!("{entry}: {message}"),
                [(entry, message), ..] => format!("{} errors · {entry}: {message}, …", errors.len()),
                [] => String::new(),
            },
        }
    }

    /// The error with its sections, styled for the terminal.
    pub fn render(&self) -> String {
        use crate::ui::{BOLD, CYAN, Cell, DIM, RED, Table, paint, tilde};
        let (subject, plural) = match &self.name {
            Some(name) => (format!("{}'s config", paint(BOLD, name)), false),
            None => ("the settings".to_string(), true),
        };
        let what = match (&self.problem, plural) {
            (ConfigProblem::Parse { .. }, false) => "doesn't evaluate".to_string(),
            (ConfigProblem::Parse { .. }, true) => "don't evaluate".to_string(),
            (ConfigProblem::Invalid(errors), _) => {
                let verb = if plural { "have" } else { "has" };
                match errors.len() {
                    1 => format!("{verb} an error"),
                    n => format!("{verb} {n} errors"),
                }
            }
        };
        let mut out = crate::ui::failure(format!("{subject} {what}"));
        if let Some(file) = &self.file {
            out.push_str(&format!(" {} {}", paint(DIM, "·"), paint(CYAN, tilde(file))));
        }
        out.push_str("\n\n");
        match &self.problem {
            ConfigProblem::Parse {
                location,
                message,
                excerpt,
            } => {
                match location {
                    Some(at) => out.push_str(&format!("  {} {message}\n", paint(DIM, format!("{at}:")))),
                    None => out.push_str(&format!("  {message}\n")),
                }
                for line in excerpt {
                    let (gutter, code) = line.split_once('|').unwrap_or(("", line));
                    let code = match code.trim().chars().all(|c| c == '^') {
                        true => paint(RED, code),
                        false => code.to_string(),
                    };
                    out.push_str(&format!("  {}{code}\n", paint(DIM, format!("{gutter}│"))));
                }
            }
            ConfigProblem::Invalid(errors) => {
                let mut t = Table::new(&["KEY", "ERROR"]);
                for (entry, message) in errors {
                    t.row(vec![Cell::new(entry.clone()), Cell::new(message.clone())]);
                }
                for line in t.render(None) {
                    out.push_str(&format!("{line}\n"));
                }
            }
        }
        out
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = crate::ui::plain(&self.render());
        f.write_str(text.trim_start_matches("✗ ").trim_end())
    }
}

impl std::error::Error for ConfigError {}

/// What a config is checked in the light of.
pub struct Context<'a> {
    /// The sandbox name; `None` for an unnamed sandbox.
    pub name: Option<&'a str>,
    /// `<root>/<name>`, the default home of a named sandbox.
    pub storage: Option<PathBuf>,
    /// Whether the host has a desktop entry with this id that psbox did not
    /// write, for the implicit default app to inherit.
    pub host_entry: &'a dyn Fn(&str) -> bool,
}

/// A config loaded from disk, with what evaluating it read.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub spec: Spec,
    /// The evaluated JSON (config and tasks), to tell whether a reload
    /// changed anything.
    pub json: String,
    pub files: Vec<LoadedFile>,
    pub missing: Vec<PathBuf>,
}

/// A config that failed to load, with what evaluation read before it
/// failed, so that a watcher notices when it is fixed.
#[derive(Debug)]
pub struct LoadFailure {
    pub error: anyhow::Error,
    pub files: Vec<LoadedFile>,
    pub missing: Vec<PathBuf>,
}

impl Spec {
    /// The config file of the sandbox `name`.
    pub fn file(name: &str, config_dir: &Path) -> PathBuf {
        config_dir.join("sandboxes").join(format!("{name}.jsonnet"))
    }

    /// Loads the sandbox `name`'s config. No file it reads may be inside
    /// the sandbox root or any of `forbidden`, the places the running
    /// sandbox can write; nor inside a place the loaded config lets it (or
    /// its tasks) write. `before_read` is told of each file before it is
    /// read.
    pub fn load_checked(
        name: &str,
        host: &HostEnv,
        forbidden: &[PathBuf],
        before_read: Option<&(dyn Fn(&Path) + Sync)>,
    ) -> std::result::Result<Loaded, LoadFailure> {
        let fail = |error: anyhow::Error| LoadFailure {
            error,
            files: Vec::new(),
            missing: Vec::new(),
        };
        validate_sandbox_name(name).map_err(fail)?;
        let file = Self::file(name, &host.config_dir);
        if !file.exists() && std::fs::symlink_metadata(&file).is_err() {
            return Err(LoadFailure {
                error: crate::ui::with_fix(
                    anyhow!("no sandbox named {name} · {} doesn't exist", crate::ui::tilde(&file)),
                    format!(
                        "{} shows the configured sandboxes; {} runs without one",
                        crate::ui::code("psbox list"),
                        crate::ui::code("psbox sandbox")
                    ),
                ),
                files: Vec::new(),
                missing: vec![file],
            });
        }
        let error = |problem| {
            anyhow::Error::from(ConfigError {
                name: Some(name.to_string()),
                file: Some(file.clone()),
                problem,
            })
        };
        let mut blocked = vec![host.root.clone()];
        blocked.extend(forbidden.iter().cloned());
        let evaluation = jsonnet::evaluate(&jsonnet::Input {
            layers: vec![Layer::File(file.clone())],
            name: Some(name),
            home: &host.home,
            root: &host.root,
            forbidden: canonical(&blocked),
            before_read,
        })
        .map_err(|f| LoadFailure {
            error: error(f.problem),
            files: f.files,
            missing: f.missing,
        })?;
        let host_entry = |id: &str| crate::install::host_entry_exists(id, host);
        let ctx = Context {
            name: Some(name),
            storage: Some(host.root.join(name)),
            host_entry: &host_entry,
        };
        let json = serde_json::to_string(&(&evaluation.config, &evaluation.tasks)).expect("JSON values serialize");
        let spec = Self::from_json(&evaluation.config, &evaluation.tasks, &ctx);
        let spec = spec.and_then(|spec| {
            jsonnet::check_files(&evaluation.files, &canonical(&spec.writable()))
                .map(|()| spec)
                .map_err(|e| ConfigProblem::Invalid(vec![("import".into(), e)]))
        });
        match spec {
            Ok(spec) => Ok(Loaded {
                spec,
                json,
                files: evaluation.files,
                missing: evaluation.missing,
            }),
            Err(problem) => Err(LoadFailure {
                error: error(problem),
                files: evaluation.files,
                missing: evaluation.missing,
            }),
        }
    }

    /// Loads the sandbox `name`'s config.
    pub fn load(name: &str, host: &HostEnv) -> Result<Self> {
        Self::load_checked(name, host, &[], None)
            .map(|l| l.spec)
            .map_err(|f| f.error)
    }

    /// Loads every configured sandbox, in name order. Each file is
    /// independent: one that fails reports its own error.
    pub fn load_all(host: &HostEnv) -> Result<Vec<(String, Result<Self>)>> {
        Ok(Self::names(&host.config_dir)?
            .into_iter()
            .map(|name| {
                let spec = Self::load(&name, host);
                (name, spec)
            })
            .collect())
    }

    /// The names of the configured sandboxes, sorted, without loading them.
    /// A missing `sandboxes` directory means no sandboxes; one that cannot
    /// be read (or is not a directory) is an error, so that nothing acts on
    /// a list that only looks empty.
    pub fn names(config_dir: &Path) -> Result<Vec<String>> {
        let dir = config_dir.join("sandboxes");
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("can't read {}", crate::ui::tilde(&dir))),
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                if path.extension()? != "jsonnet" {
                    return None;
                }
                Some(path.file_stem()?.to_str()?.to_string())
            })
            .collect();
        names.sort();
        Ok(names)
    }

    /// Checks and completes an evaluated config: `config` is the composed
    /// config and `tasks` each task's.
    pub fn from_json(
        config: &Value,
        tasks: &BTreeMap<String, Value>,
        ctx: &Context<'_>,
    ) -> std::result::Result<Self, ConfigProblem> {
        let mut c = Checker::default();
        let spec = parse_config(&mut c, config, tasks, ctx, false);
        match c.errors.is_empty() {
            true => Ok(spec),
            false => Err(ConfigProblem::Invalid(c.errors)),
        }
    }

    /// The host paths the sandbox and its tasks can write: a path home and
    /// read-write mounts.
    pub fn writable(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut add = |spec: &Spec| {
            if let Some(Home::Path { path, .. }) = &spec.home {
                out.push(path.clone());
            }
            for m in spec.mount.values() {
                if let (MountMode::ReadWrite, Some(src)) = (m.mode, &m.source) {
                    out.push(src.clone());
                }
            }
        };
        add(self);
        for t in self.task.values() {
            add(&t.spec);
        }
        out
    }

    /// The config as JSON, every default filled in, for `psbox config`.
    pub fn to_json(&self) -> Value {
        use serde_json::json;
        let home = match &self.home {
            None => Value::Null,
            Some(Home::Temporary) => json!({ "mode": "temporary" }),
            Some(Home::Path { path, cache }) => json!({
                "mode": "path",
                "path": path,
                "cache": match cache { Cache::Temporary => "temporary", Cache::Persistent => "persistent" },
            }),
        };
        let mut devices = Map::new();
        for (key, class) in [
            ("gpu", DeviceClass::Gpu),
            ("audio", DeviceClass::Audio),
            ("camera", DeviceClass::Camera),
            ("gamepad", DeviceClass::Gamepad),
            ("security-key", DeviceClass::SecurityKey),
            ("alsa", DeviceClass::Alsa),
        ] {
            devices.insert(key.into(), Value::Bool(self.devices.contains(&class)));
        }
        let usb: Vec<String> = self
            .devices
            .iter()
            .filter_map(|d| match d {
                DeviceClass::Usb { vendor, product } => Some(format!("{vendor:04x}:{product:04x}")),
                _ => None,
            })
            .collect();
        devices.insert("usb".into(), json!(usb));
        let open_in = |o: OpenIn| match o {
            OpenIn::Host => "host",
            OpenIn::Sandbox => "sandbox",
        };
        let keys = |keys: &BTreeMap<String, KeyValue>| -> Value {
            keys.iter()
                .map(|(k, v)| {
                    let v = match v {
                        KeyValue::Remove => Value::Null,
                        KeyValue::Bool(b) => json!(b),
                        KeyValue::Str(s) => json!(s),
                        KeyValue::List(l) => json!(l),
                        KeyValue::Localized { default, locales } => {
                            let mut m: Map<String, Value> =
                                locales.iter().map(|(l, v)| (l.clone(), json!(v))).collect();
                            m.insert("default".into(), json!(default));
                            Value::Object(m)
                        }
                    };
                    (k.clone(), v)
                })
                .collect::<Map<_, _>>()
                .into()
        };
        let app: Map<String, Value> = self
            .app
            .iter()
            .map(|(name, a)| {
                let v = match a {
                    AppSpec::Application(a) => {
                        let actions: Map<String, Value> = a
                            .actions
                            .iter()
                            .map(|(n, act)| {
                                let v = match act {
                                    None => Value::Null,
                                    Some(act) => {
                                        let mut m = keys(&act.keys).as_object().cloned().unwrap_or_default();
                                        m.insert("arguments".into(), json!(act.arguments));
                                        Value::Object(m)
                                    }
                                };
                                (n.clone(), v)
                            })
                            .collect();
                        let mut v = json!({
                            "kind": "application",
                            "run": a.run,
                            "environment": a.environment,
                            "id": a.id,
                            "source-id": a.source_id,
                            "remove-arguments": a.remove_arguments,
                            "keys": keys(&a.keys),
                            "actions": actions,
                        });
                        if let Some(no_display) = a.no_display {
                            v["no-display"] = json!(no_display);
                        }
                        v
                    }
                    AppSpec::Autostart(a) => json!({
                        "kind": "autostart",
                        "app": a.app,
                        "id": a.id,
                        "source-id": a.source_id,
                        "remove-arguments": a.remove_arguments,
                        "keys": keys(&a.keys),
                    }),
                    AppSpec::Suppress { kind, id } => json!({ "kind": kind.to_string(), "id": id, "suppress": true }),
                };
                (name.clone(), v)
            })
            .collect();
        let mount: Map<String, Value> = self
            .mount
            .iter()
            .map(|(name, m)| {
                let mut v = json!({
                    "destination": m.destination,
                    "mode": m.mode.to_string(),
                    "optional": m.optional,
                    "create": m.create,
                });
                if let Some(src) = &m.source {
                    v["source"] = json!(src);
                }
                (name.clone(), v)
            })
            .collect();
        let task: Map<String, Value> = self
            .task
            .iter()
            .map(|(name, t)| {
                let mut v = json!({ "run": t.command });
                if let Some(app) = &t.app {
                    v["app"] = json!(app);
                }
                (name.clone(), v)
            })
            .collect();
        json!({
            "sandbox": { "idle-timeout": self.idle_timeout },
            "home": home,
            "devices": devices,
            "dbus": match &self.dbus {
                None => Value::Null,
                Some(d) => json!({
                    "mode": match d.mode { DbusMode::Portal => "portal", DbusMode::Host => "host" },
                    "talk": d.talk,
                }),
            },
            "network": match self.network {
                NetworkSetting::Loopback => Value::Null,
                NetworkSetting::Automatic => json!({ "mode": "automatic" }),
                NetworkSetting::Isolated => json!({ "mode": "isolated" }),
                NetworkSetting::Host => json!({ "mode": "host" }),
            },
            "display": if self.display { json!({}) } else { Value::Null },
            "xwayland": match &self.xwayland {
                None => Value::Null,
                Some(x) => match x.mode {
                    XwaylandMode::Builtin => json!({ "mode": x.mode.to_string(), "scale": x.scale.to_string() }),
                    XwaylandMode::Satellite => json!({ "mode": x.mode.to_string() }),
                },
            },
            "links": { "http": open_in(self.links.http), "https": open_in(self.links.https), "mailto": open_in(self.links.mailto) },
            "environment": self.environment,
            "mount": mount,
            "app": app,
            "task": task,
        })
    }
}

/// Evaluates `text` as the config of the sandbox `name`, whose host has a
/// desktop entry for each of `host_entries`, in a host with home `/home/u`
/// and sandbox root `/home/u/Sandbox`. For tests.
#[cfg(test)]
pub fn parse_named(name: &str, text: &str, host_entries: &[&str]) -> Result<Spec> {
    let evaluation = jsonnet::evaluate(&jsonnet::Input {
        layers: vec![Layer::Snippet {
            name: format!("{name}.jsonnet"),
            text: text.into(),
        }],
        name: Some(name),
        home: Path::new("/home/u"),
        root: Path::new("/home/u/Sandbox"),
        forbidden: Vec::new(),
        before_read: None,
    })
    .map_err(|f| ConfigError {
        name: Some(name.into()),
        file: None,
        problem: f.problem,
    })?;
    let host_entry = |id: &str| host_entries.contains(&id);
    let ctx = Context {
        name: Some(name),
        storage: Some(PathBuf::from("/home/u/Sandbox").join(name)),
        host_entry: &host_entry,
    };
    Spec::from_json(&evaluation.config, &evaluation.tasks, &ctx).map_err(|problem| {
        ConfigError {
            name: Some(name.into()),
            file: None,
            problem,
        }
        .into()
    })
}

/// [`parse_named`] for the sandbox `app`, on a host without desktop entries.
#[cfg(test)]
pub fn parse(text: &str) -> Result<Spec> {
    parse_named("app", text, &[])
}

/// `paths`, each with its symlinks resolved when it exists.
pub fn canonical(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths
        .iter()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
        .collect()
}

/// Collects validation errors by key path.
#[derive(Default)]
struct Checker {
    errors: Vec<(String, String)>,
}

/// `parent.key`, quoting a key that contains a `.`.
fn at(parent: &str, key: &str) -> String {
    let key = match key.contains('.') {
        true => format!("{key:?}"),
        false => key.to_string(),
    };
    match parent.is_empty() {
        true => key,
        false => format!("{parent}.{key}"),
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

impl Checker {
    fn err(&mut self, path: impl Into<String>, message: impl fmt::Display) {
        self.errors.push((path.into(), message.to_string()));
    }

    fn object<'v>(&mut self, path: &str, v: &'v Value) -> Option<&'v Map<String, Value>> {
        match v {
            Value::Object(m) => Some(m),
            other => {
                self.err(path, format!("expected an object, got {}", type_name(other)));
                None
            }
        }
    }

    /// Reports keys of `m` not in `allowed`.
    fn known(&mut self, path: &str, m: &Map<String, Value>, allowed: &[&str]) {
        for key in m.keys().filter(|k| !allowed.contains(&k.as_str())) {
            self.err(at(path, key), "unknown key");
        }
    }

    fn bool(&mut self, path: &str, m: &Map<String, Value>, key: &str, default: bool) -> bool {
        match m.get(key) {
            None => default,
            Some(Value::Bool(b)) => *b,
            Some(other) => {
                self.err(at(path, key), format!("expected a boolean, got {}", type_name(other)));
                default
            }
        }
    }

    fn string<'v>(&mut self, path: &str, m: &'v Map<String, Value>, key: &str) -> Option<&'v str> {
        match m.get(key) {
            None => None,
            Some(Value::String(s)) => Some(s),
            Some(other) => {
                self.err(at(path, key), format!("expected a string, got {}", type_name(other)));
                None
            }
        }
    }

    fn choice<T: Copy>(
        &mut self,
        path: &str,
        m: &Map<String, Value>,
        key: &str,
        choices: &[(&str, T)],
        default: T,
    ) -> T {
        let Some(s) = self.string(path, m, key) else {
            return default;
        };
        match choices.iter().find(|(name, _)| *name == s) {
            Some((_, v)) => *v,
            None => {
                let names: Vec<String> = choices.iter().map(|(n, _)| format!("{n:?}")).collect();
                self.err(at(path, key), format!("expected {}, got {s:?}", names.join(" or ")));
                default
            }
        }
    }

    fn strings(&mut self, path: &str, m: &Map<String, Value>, key: &str) -> Vec<String> {
        match m.get(key) {
            None => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .enumerate()
                .filter_map(|(i, v)| match v {
                    Value::String(s) => Some(s.clone()),
                    other => {
                        self.err(
                            format!("{}[{i}]", at(path, key)),
                            format!("expected a string, got {}", type_name(other)),
                        );
                        None
                    }
                })
                .collect(),
            Some(other) => {
                self.err(at(path, key), format!("expected a list, got {}", type_name(other)));
                Vec::new()
            }
        }
    }

    fn path(&mut self, path: &str, m: &Map<String, Value>, key: &str) -> Option<PathBuf> {
        let s = self.string(path, m, key)?;
        match absolute(s) {
            Ok(p) => Some(p),
            Err(e) => {
                self.err(at(path, key), e);
                None
            }
        }
    }

    /// Fails on keys present in `m` that the chosen mode does not use.
    fn unused(&mut self, path: &str, m: &Map<String, Value>, keys: &[&str], why: &str) {
        for key in keys.iter().filter(|k| m.contains_key(**k)) {
            self.err(at(path, key), format!("not used {why}"));
        }
    }

    fn environment(&mut self, path: &str, v: Option<&Value>) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let Some(v) = v else { return out };
        let Some(m) = self.object(path, v) else { return out };
        for (key, value) in m {
            if let Err(e) = validate_env_key(key) {
                self.err(at(path, key), e);
            }
            match value {
                Value::Null => {}
                Value::String(s) => {
                    out.insert(key.clone(), s.clone());
                }
                other => self.err(
                    at(path, key),
                    format!("expected a string or null, got {}", type_name(other)),
                ),
            }
        }
        out
    }
}

/// An absolute, `..`-free path, normalized (no repeated or trailing `/`).
fn absolute(s: &str) -> std::result::Result<PathBuf, String> {
    let p = Path::new(s);
    if !p.is_absolute() {
        return Err(format!("{s:?} must be an absolute path (psbox.path builds one)"));
    }
    if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(format!("{s:?} must not contain '..'"));
    }
    Ok(p.components().collect())
}

const SECTIONS: [&str; 12] = [
    "sandbox",
    "home",
    "devices",
    "dbus",
    "network",
    "display",
    "xwayland",
    "links",
    "environment",
    "mount",
    "app",
    "task",
];

/// Parses the config at `v`. A task's config (`in_task`) ignores its own
/// `app` and `task` sections.
fn parse_config(c: &mut Checker, v: &Value, tasks: &BTreeMap<String, Value>, ctx: &Context<'_>, in_task: bool) -> Spec {
    let empty = Map::new();
    let top = c.object("config", v).unwrap_or(&empty);
    c.known("", top, &SECTIONS);
    let get = |key: &str| top.get(key);

    // sandbox
    let mut idle_timeout = 60;
    if let Some(v) = get("sandbox")
        && let Some(m) = c.object("sandbox", v)
    {
        c.known("sandbox", m, &["idle-timeout"]);
        match m.get("idle-timeout") {
            None => {}
            Some(n) => match n.as_u64() {
                Some(n) => idle_timeout = n,
                None => c.err("sandbox.idle-timeout", "expected a whole number of seconds"),
            },
        }
    }

    // home
    let home = match get("home") {
        None | Some(Value::Null) => None,
        Some(v) => c.object("home", v).and_then(|m| {
            c.known("home", m, &["mode", "path", "cache"]);
            let default_mode = if ctx.storage.is_some() { "path" } else { "temporary" };
            let mode = c.string("home", m, "mode").unwrap_or(default_mode);
            match mode {
                "temporary" => {
                    c.unused("home", m, &["path", "cache"], "with mode \"temporary\"");
                    Some(Home::Temporary)
                }
                "path" => {
                    let path = match m.contains_key("path") {
                        true => c.path("home", m, "path"),
                        false => match &ctx.storage {
                            Some(storage) => Some(storage.clone()),
                            None => {
                                c.err("home.path", "required with mode \"path\" in an unnamed sandbox");
                                None
                            }
                        },
                    };
                    let cache = c.choice(
                        "home",
                        m,
                        "cache",
                        &[("temporary", Cache::Temporary), ("persistent", Cache::Persistent)],
                        Cache::Temporary,
                    );
                    path.map(|path| Home::Path { path, cache })
                }
                other => {
                    c.err(
                        "home.mode",
                        format!("expected \"path\" or \"temporary\", got {other:?}"),
                    );
                    None
                }
            }
        }),
    };

    // devices
    let mut devices = vec![DeviceClass::Gpu, DeviceClass::Audio];
    if let Some(v) = get("devices")
        && let Some(m) = c.object("devices", v)
    {
        c.known(
            "devices",
            m,
            &["gpu", "audio", "camera", "gamepad", "security-key", "alsa", "usb"],
        );
        devices.clear();
        for (key, class, default) in [
            ("gpu", DeviceClass::Gpu, true),
            ("audio", DeviceClass::Audio, true),
            ("camera", DeviceClass::Camera, false),
            ("gamepad", DeviceClass::Gamepad, false),
            ("security-key", DeviceClass::SecurityKey, false),
            ("alsa", DeviceClass::Alsa, false),
        ] {
            if c.bool("devices", m, key, default) {
                devices.push(class);
            }
        }
        for (i, id) in c.strings("devices", m, "usb").into_iter().enumerate() {
            match usb_device(&id) {
                Ok(d) if !devices.contains(&d) => devices.push(d),
                Ok(_) => {}
                Err(e) => c.err(format!("devices.usb[{i}]"), e),
            }
        }
    }

    // dbus
    let dbus = match get("dbus") {
        None | Some(Value::Null) => None,
        Some(v) => c.object("dbus", v).map(|m| {
            c.known("dbus", m, &["mode", "talk"]);
            let mode = c.choice(
                "dbus",
                m,
                "mode",
                &[("portal", DbusMode::Portal), ("host", DbusMode::Host)],
                DbusMode::Portal,
            );
            let mut talk = Vec::new();
            match mode {
                DbusMode::Host => c.unused("dbus", m, &["talk"], "with mode \"host\", which reaches every name"),
                DbusMode::Portal => {
                    for (i, name) in c.strings("dbus", m, "talk").into_iter().enumerate() {
                        match validate_talk_name(&name) {
                            Ok(()) if !talk.contains(&name) => talk.push(name),
                            Ok(()) => {}
                            Err(e) => c.err(format!("dbus.talk[{i}]"), e),
                        }
                    }
                }
            }
            Dbus { mode, talk }
        }),
    };

    // network
    let network = match get("network") {
        None => NetworkSetting::Automatic,
        Some(Value::Null) => NetworkSetting::Loopback,
        Some(v) => match c.object("network", v) {
            Some(m) => {
                c.known("network", m, &["mode"]);
                c.choice(
                    "network",
                    m,
                    "mode",
                    &[
                        ("automatic", NetworkSetting::Automatic),
                        ("isolated", NetworkSetting::Isolated),
                        ("host", NetworkSetting::Host),
                    ],
                    NetworkSetting::Automatic,
                )
            }
            None => NetworkSetting::Automatic,
        },
    };

    // display and xwayland
    let display = match get("display") {
        None => true,
        Some(Value::Null) => false,
        Some(v) => {
            if let Some(m) = c.object("display", v) {
                c.known("display", m, &[]);
            }
            true
        }
    };
    let xwayland = match get("xwayland") {
        None | Some(Value::Null) => None,
        // A display set to null takes Xwayland with it.
        Some(_) if !display => None,
        Some(v) => c.object("xwayland", v).map(|m| {
            c.known("xwayland", m, &["mode", "scale"]);
            let mode = c.choice(
                "xwayland",
                m,
                "mode",
                &[
                    ("builtin", XwaylandMode::Builtin),
                    ("xwayland-satellite", XwaylandMode::Satellite),
                ],
                XwaylandMode::Builtin,
            );
            let scale = match mode {
                XwaylandMode::Builtin => c.choice(
                    "xwayland",
                    m,
                    "scale",
                    &[
                        ("native", XwaylandScale::Native),
                        ("round-up", XwaylandScale::RoundUp),
                        ("compositor", XwaylandScale::Compositor),
                    ],
                    XwaylandScale::Native,
                ),
                XwaylandMode::Satellite => {
                    c.unused(
                        "xwayland",
                        m,
                        &["scale"],
                        "with mode \"xwayland-satellite\" (only psbox's title bar proxy scales)",
                    );
                    XwaylandScale::Native
                }
            };
            Xwayland { mode, scale }
        }),
    };

    // links
    let mut links = Links {
        http: OpenIn::Host,
        https: OpenIn::Host,
        mailto: OpenIn::Host,
    };
    if let Some(v) = get("links")
        && let Some(m) = c.object("links", v)
    {
        c.known("links", m, &["http", "https", "mailto"]);
        let choices = [("host", OpenIn::Host), ("sandbox", OpenIn::Sandbox)];
        links.http = c.choice("links", m, "http", &choices, OpenIn::Host);
        links.https = c.choice("links", m, "https", &choices, OpenIn::Host);
        links.mailto = c.choice("links", m, "mailto", &choices, OpenIn::Host);
    }

    let environment = c.environment("environment", get("environment"));
    let mount = parse_mounts(c, get("mount"));

    let (app, task) = match (in_task, ctx.name) {
        (false, Some(name)) => {
            let app = parse_apps(c, get("app"), name, ctx);
            let task = parse_tasks(c, get("task"), tasks, &app, ctx);
            (app, task)
        }
        // A task's own app and task sections are its sandbox's; an unnamed
        // sandbox has neither.
        _ => (BTreeMap::new(), BTreeMap::new()),
    };

    Spec {
        idle_timeout,
        home,
        devices,
        dbus,
        network,
        display,
        xwayland,
        links,
        environment,
        mount,
        app,
        task,
    }
}

fn parse_mounts(c: &mut Checker, v: Option<&Value>) -> BTreeMap<String, MountSpec> {
    let mut out = BTreeMap::new();
    let Some(v) = v else { return out };
    let Some(entries) = c.object("mount", v) else {
        return out;
    };
    for (name, v) in entries {
        let path = at("mount", name);
        if let Err(e) = validate_name("mount", name) {
            c.err(&path, e);
        }
        if v.is_null() {
            continue;
        }
        let Some(m) = c.object(&path, v) else { continue };
        c.known(&path, m, &["source", "destination", "mode", "optional", "create"]);
        let mode = c.choice(
            &path,
            m,
            "mode",
            &[
                ("read-write", MountMode::ReadWrite),
                ("read-only", MountMode::ReadOnly),
                ("overlay", MountMode::Overlay),
                ("empty", MountMode::Empty),
            ],
            MountMode::ReadWrite,
        );
        let optional = c.bool(&path, m, "optional", false);
        let create = c.bool(&path, m, "create", false);
        let destination = m
            .contains_key("destination")
            .then(|| c.path(&path, m, "destination"))
            .flatten();
        let source = match mode {
            MountMode::Empty => {
                c.unused(&path, m, &["source"], "with mode \"empty\"");
                if create {
                    c.err(at(&path, "create"), "an empty mount has no source to create");
                }
                if destination.is_none() && !m.contains_key("destination") {
                    c.err(at(&path, "destination"), "required with mode \"empty\"");
                }
                None
            }
            _ => {
                let source = c.path(&path, m, "source");
                if source.is_none() && !m.contains_key("source") {
                    c.err(at(&path, "source"), "required");
                }
                source
            }
        };
        if create && optional {
            c.err(at(&path, "create"), "an optional mount is never created");
        }
        let Some(destination) = destination.or_else(|| source.clone()) else {
            continue;
        };
        out.insert(
            name.clone(),
            MountSpec {
                source,
                destination,
                mode,
                optional,
                create,
            },
        );
    }
    // Two mounts at one destination: the same mount twice merges, anything
    // else is a conflict.
    let mut seen: Vec<(String, MountSpec)> = Vec::new();
    let mut dropped = Vec::new();
    for (name, m) in &out {
        match seen.iter().find(|(_, s)| s.destination == m.destination) {
            Some((_, s)) if s == m => dropped.push(name.clone()),
            Some((first, _)) => c.err(
                at("mount", name),
                format!("{} is also the destination of mount {first}", m.destination.display()),
            ),
            None => seen.push((name.clone(), m.clone())),
        }
    }
    for name in dropped {
        out.remove(&name);
    }
    out
}

/// Desktop keys psbox writes itself, or that would break the entry.
const RESERVED_KEYS: [&str; 7] = [
    "Exec",
    "TryExec",
    "DBusActivatable",
    "Hidden",
    "Type",
    "Actions",
    "NoDisplay",
];

fn parse_keys(c: &mut Checker, path: &str, v: Option<&Value>, extra_reserved: &[&str]) -> BTreeMap<String, KeyValue> {
    let mut out = BTreeMap::new();
    let Some(v) = v else { return out };
    let Some(m) = c.object(path, v) else { return out };
    for (key, value) in m {
        let kpath = at(path, key);
        if key.is_empty() || !key.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-') {
            c.err(
                &kpath,
                "desktop keys are letters, digits, and '-' (translations go in an object)",
            );
            continue;
        }
        if RESERVED_KEYS.contains(&key.as_str()) || extra_reserved.contains(&key.as_str()) {
            c.err(&kpath, "psbox sets this key itself");
            continue;
        }
        let value = match value {
            Value::Null => KeyValue::Remove,
            Value::Bool(b) => KeyValue::Bool(*b),
            Value::String(s) => KeyValue::Str(s.clone()),
            Value::Array(items) => {
                let mut list = Vec::new();
                for (i, item) in items.iter().enumerate() {
                    match item {
                        Value::String(s) => list.push(s.clone()),
                        other => c.err(
                            format!("{kpath}[{i}]"),
                            format!("expected a string, got {}", type_name(other)),
                        ),
                    }
                }
                KeyValue::List(list)
            }
            Value::Object(o) => {
                let mut default = None;
                let mut locales = BTreeMap::new();
                for (locale, text) in o {
                    let Value::String(text) = text else {
                        c.err(
                            at(&kpath, locale),
                            format!("expected a string, got {}", type_name(text)),
                        );
                        continue;
                    };
                    if locale == "default" {
                        default = Some(text.clone());
                    } else if valid_locale(locale) {
                        locales.insert(locale.clone(), text.clone());
                    } else {
                        c.err(at(&kpath, locale), "not a locale (lang_COUNTRY.ENCODING@MODIFIER)");
                    }
                }
                match default {
                    Some(default) => KeyValue::Localized { default, locales },
                    None => {
                        c.err(at(&kpath, "default"), "required");
                        continue;
                    }
                }
            }
            Value::Number(_) => {
                c.err(&kpath, "expected a string, boolean, list, or translations");
                continue;
            }
        };
        out.insert(key.clone(), value);
    }
    out
}

/// A locale tag as Desktop Entry translations use it:
/// `lang[_COUNTRY][.ENCODING][@MODIFIER]`.
fn valid_locale(s: &str) -> bool {
    let (rest, modifier) = s.split_once('@').unwrap_or((s, ""));
    let (rest, encoding) = rest.split_once('.').unwrap_or((rest, ""));
    let (lang, country) = rest.split_once('_').unwrap_or((rest, ""));
    let word = |w: &str, empty_ok: bool| {
        (empty_ok && w.is_empty()) || (!w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
    };
    !lang.is_empty()
        && lang.chars().all(|c| c.is_ascii_lowercase())
        && word(country, true)
        && word(encoding, true)
        && word(modifier, true)
}

fn parse_apps(c: &mut Checker, v: Option<&Value>, sandbox: &str, ctx: &Context<'_>) -> BTreeMap<String, AppSpec> {
    let mut out = BTreeMap::new();
    let empty = Map::new();
    let entries = match v {
        None => &empty,
        Some(v) => c.object("app", v).unwrap_or(&empty),
    };
    for (name, v) in entries {
        let path = at("app", name);
        if let Err(e) = validate_name("app", name) {
            c.err(&path, e);
            continue;
        }
        if v.is_null() {
            continue;
        }
        let Some(m) = c.object(&path, v) else { continue };
        let kind = c.choice(
            &path,
            m,
            "kind",
            &[("application", Kind::Application), ("autostart", Kind::Autostart)],
            Kind::Application,
        );
        let id = match c.string(&path, m, "id") {
            Some(id) => id.to_string(),
            None => name.clone(),
        };
        if let Err(e) = validate_app_id(&id) {
            c.err(at(&path, "id"), e);
        }
        if c.bool(&path, m, "suppress", false) {
            c.known(&path, m, &["kind", "id", "suppress"]);
            out.insert(name.clone(), AppSpec::Suppress { kind, id });
            continue;
        }
        let source_id = match m.get("source-id") {
            None => None,
            Some(Value::Null) => None,
            Some(_) => c.string(&path, m, "source-id").map(String::from),
        };
        if let Some(s) = &source_id
            && let Err(e) = validate_app_id(s)
        {
            c.err(at(&path, "source-id"), e);
        }
        let remove_arguments = c.strings(&path, m, "remove-arguments");
        let keys = parse_keys(c, &at(&path, "keys"), m.get("keys"), &[]);
        match kind {
            Kind::Application => {
                c.known(
                    &path,
                    m,
                    &[
                        "kind",
                        "id",
                        "suppress",
                        "run",
                        "environment",
                        "source-id",
                        "remove-arguments",
                        "keys",
                        "actions",
                        "no-display",
                    ],
                );
                let default = name == sandbox;
                let mut run = c.strings(&path, m, "run");
                if !m.contains_key("run") {
                    match default {
                        true => run = vec![sandbox.to_string()],
                        false => c.err(at(&path, "run"), "required"),
                    }
                } else if run.is_empty() {
                    c.err(at(&path, "run"), "is empty");
                }
                // The default app inherits a host entry of its name unless
                // the config says otherwise.
                let source_id = match (default, m.contains_key("source-id")) {
                    (true, false) => (ctx.host_entry)(sandbox).then(|| sandbox.to_string()),
                    _ => source_id,
                };
                let environment = c.environment(&at(&path, "environment"), m.get("environment"));
                let actions = parse_actions(c, &at(&path, "actions"), m.get("actions"));
                let no_display = m.get("no-display").map(|_| c.bool(&path, m, "no-display", false));
                out.insert(
                    name.clone(),
                    AppSpec::Application(Application {
                        run,
                        environment,
                        id,
                        source_id,
                        remove_arguments,
                        keys,
                        actions,
                        no_display,
                        implicit: false,
                    }),
                );
            }
            Kind::Autostart => {
                c.known(
                    &path,
                    m,
                    &["kind", "id", "suppress", "app", "source-id", "remove-arguments", "keys"],
                );
                let app = match c.string(&path, m, "app") {
                    Some(app) => app.to_string(),
                    None => sandbox.to_string(),
                };
                out.insert(
                    name.clone(),
                    AppSpec::Autostart(Autostart {
                        app,
                        id,
                        source_id,
                        remove_arguments,
                        keys,
                    }),
                );
            }
        }
    }

    // The implicit default app, when the config neither defines nor
    // removes it.
    if !entries.contains_key(sandbox) {
        out.insert(
            sandbox.to_string(),
            AppSpec::Application(Application {
                run: vec![sandbox.to_string()],
                environment: BTreeMap::new(),
                id: sandbox.to_string(),
                source_id: (ctx.host_entry)(sandbox).then(|| sandbox.to_string()),
                remove_arguments: Vec::new(),
                keys: BTreeMap::new(),
                actions: BTreeMap::new(),
                no_display: None,
                implicit: true,
            }),
        );
    }

    // Autostart entries start an application; ids are unique per kind.
    let mut ids: BTreeMap<(Kind, &str), &str> = BTreeMap::new();
    for (name, a) in &out {
        if let AppSpec::Autostart(s) = a {
            match out.get(&s.app) {
                Some(AppSpec::Application(_)) => {}
                Some(_) => c.err(at(&at("app", name), "app"), format!("{} isn't an application", s.app)),
                None if s.app == sandbox => c.err(
                    at(&at("app", name), "app"),
                    "the default app was removed; name the application to start",
                ),
                None => c.err(at(&at("app", name), "app"), format!("no application named {}", s.app)),
            }
        }
        if let AppSpec::Application(a) = a
            && !a.has_entry()
        {
            continue;
        }
        match ids.get(&(a.kind(), a.id())) {
            Some(first) => c.err(
                at("app", name),
                format!("app {first} also has the {} id {}", a.kind(), a.id()),
            ),
            None => {
                ids.insert((a.kind(), a.id()), name);
            }
        }
    }
    out
}

fn parse_actions(c: &mut Checker, path: &str, v: Option<&Value>) -> BTreeMap<String, Option<Action>> {
    let mut out = BTreeMap::new();
    let Some(v) = v else { return out };
    let Some(m) = c.object(path, v) else { return out };
    for (name, v) in m {
        let apath = at(path, name);
        if name.is_empty() || !name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-') {
            c.err(&apath, "action names are letters, digits, and '-'");
            continue;
        }
        if v.is_null() {
            out.insert(name.clone(), None);
            continue;
        }
        let Some(a) = c.object(&apath, v) else { continue };
        let arguments = c.strings(&apath, a, "arguments");
        let mut keys_obj = a.clone();
        keys_obj.remove("arguments");
        let keys = parse_keys(c, &apath, Some(&Value::Object(keys_obj)), &[]);
        out.insert(name.clone(), Some(Action { arguments, keys }));
    }
    out
}

fn parse_tasks(
    c: &mut Checker,
    v: Option<&Value>,
    tasks: &BTreeMap<String, Value>,
    apps: &BTreeMap<String, AppSpec>,
    ctx: &Context<'_>,
) -> BTreeMap<String, TaskSpec> {
    let mut out = BTreeMap::new();
    let Some(v) = v else { return out };
    let Some(entries) = c.object("task", v) else { return out };
    for (name, v) in entries {
        let path = at("task", name);
        if !valid_word(name) {
            c.err(&path, "invalid task name: use letters, digits, '_', '-'");
            continue;
        }
        if v.is_null() {
            continue;
        }
        let Some(m) = c.object(&path, v) else { continue };
        c.known(&path, m, &["run", "script", "app", "overrides"]);
        let command = match (m.get("run"), m.get("script")) {
            (Some(_), None) => {
                let run = c.strings(&path, m, "run");
                if run.is_empty() {
                    c.err(at(&path, "run"), "is empty");
                }
                run
            }
            (None, Some(_)) => match c.string(&path, m, "script") {
                Some(script) => vec!["bash".into(), "-c".into(), script.into()],
                None => Vec::new(),
            },
            _ => {
                c.err(&path, "use exactly one of run and script");
                Vec::new()
            }
        };
        let app = c.string(&path, m, "app").map(String::from);
        if let Some(a) = &app {
            match apps.get(a) {
                Some(AppSpec::Application(_)) => {}
                _ => c.err(at(&path, "app"), format!("no application named {a}")),
            }
        }
        if let Some(o) = m.get("overrides")
            && let Some(o) = c.object(&at(&path, "overrides"), o)
        {
            for key in ["app", "task"] {
                if o.contains_key(key) {
                    c.err(at(&at(&path, "overrides"), key), "a task can't override this");
                }
            }
        }
        let Some(config) = tasks.get(name) else {
            c.err(&path, "missing from the evaluation");
            continue;
        };
        let mut tc = Checker::default();
        let mut spec = parse_config(&mut tc, config, &BTreeMap::new(), ctx, true);
        for (key, message) in tc.errors {
            // Errors in the inherited config are reported once, at the
            // sandbox's own keys; only the task's own are new.
            if !c.errors.iter().any(|(k, m)| *k == key && *m == message) {
                c.err(format!("{path} (with overrides) {key}"), message);
            }
        }
        spec.idle_timeout = 0;
        if app.is_some() && spec.dbus.is_none() {
            c.err(at(&path, "app"), "needs a session bus to use an app's portal identity");
        }
        out.insert(
            name.clone(),
            TaskSpec {
                command,
                app,
                spec: Box::new(spec),
            },
        );
    }
    out
}

/// Desktop file ids: dot-separated elements of letters, digits, `_`, and
/// `-`, not starting with a digit. A single element (such as `code-oss`) is
/// a valid desktop file id.
pub fn validate_app_id(id: &str) -> Result<()> {
    let ok = id.len() <= 255
        && id.split('.').all(|e| {
            !e.is_empty()
                && !e.starts_with(|c: char| c.is_ascii_digit())
                && e.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        });
    if !ok {
        bail!("invalid desktop file id {id:?}");
    }
    Ok(())
}

/// A well-known host bus name for `dbus.talk`: dot-separated elements of
/// letters, digits, `_`, and `-` (none starting with a digit), at least two.
/// The bus itself and the names the gateway already serves are refused.
fn validate_talk_name(name: &str) -> Result<()> {
    let ok = name.len() <= 255
        && name.split('.').count() >= 2
        && name.split('.').all(|e| {
            !e.is_empty()
                && !e.starts_with(|c: char| c.is_ascii_digit())
                && e.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        });
    if !ok {
        bail!("invalid bus name {name:?}");
    }
    if name == crate::dbus::conn::BUS_NAME || crate::portal::GATEWAY_NAMES.contains(&name) {
        bail!("{name} is always handled by psbox and cannot be listed");
    }
    Ok(())
}

fn validate_env_key(key: &str) -> Result<()> {
    let mut chars = key.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    if !first_ok || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("invalid environment variable name {key:?}");
    }
    Ok(())
}

fn valid_word(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The validation errors of `text`, by key, for the sandbox `app`.
    fn problems(text: &str) -> Vec<(String, String)> {
        let e = parse(text).unwrap_err();
        match e.downcast::<ConfigError>().expect("a config error").problem {
            ConfigProblem::Invalid(errors) => errors,
            other => panic!("expected validation errors, got {other:?}"),
        }
    }

    fn has(problems: &[(String, String)], key: &str, part: &str) -> bool {
        problems.iter().any(|(k, m)| k == key && m.contains(part))
    }

    fn application<'s>(spec: &'s Spec, name: &str) -> &'s Application {
        match &spec.app[name] {
            AppSpec::Application(a) => a,
            other => panic!("{name} is {other:?}"),
        }
    }

    #[test]
    fn an_empty_config_gets_the_defaults() {
        let s = parse("{}").unwrap();
        assert_eq!(s.idle_timeout, 60);
        assert_eq!(
            s.home,
            Some(Home::Path {
                path: "/home/u/Sandbox/app".into(),
                cache: Cache::Temporary
            })
        );
        assert_eq!(s.devices, vec![DeviceClass::Gpu, DeviceClass::Audio]);
        assert_eq!(s.dbus.as_ref().map(|d| d.mode), Some(DbusMode::Portal));
        assert_eq!(s.network, NetworkSetting::Automatic);
        assert!(s.display && s.xwayland.is_none());
        assert_eq!(s.links.host_schemes(), vec!["http", "https", "mailto"]);
        assert!(s.mount.is_empty() && s.task.is_empty());
        let app = application(&s, "app");
        assert!(app.implicit && !app.has_entry());
        assert_eq!(app.run, vec!["app"]);
    }

    #[test]
    fn null_removes_sections() {
        let s = parse("{ home: null, dbus: null, network: null, display: null, xwayland: {} }").unwrap();
        assert_eq!(s.home, None);
        assert_eq!(s.dbus, None);
        assert_eq!(s.network, NetworkSetting::Loopback);
        assert!(
            !s.display && s.xwayland.is_none(),
            "display: null takes Xwayland with it"
        );
    }

    #[test]
    fn sections_fill_in_their_defaults() {
        let s = parse(
            "{ home+: { cache: 'persistent' }, devices: { camera: true, usb: ['0483:5740', '0483:5740'] },
               xwayland: { scale: 'round-up' }, links+: { https: 'sandbox' } }",
        )
        .unwrap();
        assert_eq!(
            s.home,
            Some(Home::Path {
                path: "/home/u/Sandbox/app".into(),
                cache: Cache::Persistent
            })
        );
        // Setting devices keeps the defaults of the classes it leaves out.
        assert_eq!(
            s.devices,
            vec![
                DeviceClass::Gpu,
                DeviceClass::Audio,
                DeviceClass::Camera,
                DeviceClass::Usb {
                    vendor: 0x0483,
                    product: 0x5740
                }
            ]
        );
        let x = s.xwayland.unwrap();
        assert_eq!((x.mode, x.scale), (XwaylandMode::Builtin, XwaylandScale::RoundUp));
        assert_eq!(s.links.host_schemes(), vec!["http", "mailto"]);
        let none = parse("local psbox = import 'psbox'; { devices: psbox.devices.none }").unwrap();
        assert!(none.devices.is_empty());
    }

    #[test]
    fn keys_for_another_mode_and_unknown_keys_are_errors() {
        let p = problems(
            "{ home: { mode: 'temporary', cache: 'persistent' }, dbus: { mode: 'host', talk: ['org.a.B'] },
               xwayland: { mode: 'xwayland-satellite', scale: 'native' }, network: { mode: 'none' },
               devices+: { webcam: true }, sandbox+: { 'idle-timeout': -1 }, extra: {} }",
        );
        assert!(has(&p, "home.cache", "with mode \"temporary\""), "{p:?}");
        assert!(has(&p, "dbus.talk", "with mode \"host\""), "{p:?}");
        assert!(has(&p, "xwayland.scale", "xwayland-satellite"), "{p:?}");
        assert!(p.iter().any(|(k, _)| k == "network.mode"), "{p:?}");
        assert!(p.iter().any(|(k, _)| k == "devices.webcam"), "{p:?}");
        assert!(has(&p, "sandbox.idle-timeout", "whole number"), "{p:?}");
        assert!(p.iter().any(|(k, _)| k == "extra"), "{p:?}");
    }

    #[test]
    fn hidden_fields_are_errors() {
        let e = format!("{:#}", parse("{ environment+: { A:: 'x' } }").unwrap_err());
        assert!(e.contains("hidden fields never reach psbox: environment.A"), "{e}");
    }

    #[test]
    fn mounts() {
        let s = parse(
            "local psbox = import 'psbox';
             { mount+: {
                 code: psbox.mount.readWrite('/home/u/Code', create=true),
                 'a.b': psbox.mount.readOnly('/srv', '/data'),
                 tmp: psbox.mount.empty('/home/u/.cache'),
                 again: psbox.mount.readOnly('/srv', '/data'),
                 gone: null,
             } }",
        )
        .unwrap();
        assert_eq!(s.mount["code"].destination, Path::new("/home/u/Code"));
        assert!(s.mount["code"].create);
        assert_eq!(s.mount["tmp"].source, None);
        assert_eq!(s.mount.len(), 3, "the identical mount merges and null removes");
        assert_eq!(
            s.writable(),
            vec![PathBuf::from("/home/u/Sandbox/app"), "/home/u/Code".into()]
        );

        let p = problems(
            "{ mount+: { a: { source: '/a', destination: '/x' }, b: { source: '/b', destination: '/x' },
                         c: { source: 'rel' }, d: { mode: 'empty' }, e: { source: '/e', optional: true, create: true },
                         f: { mode: 'empty', destination: '/f', source: '/f' } } }",
        );
        assert!(has(&p, "mount.b", "also the destination of mount a"), "{p:?}");
        assert!(p.iter().any(|(k, _)| k == "mount.c.source"), "{p:?}");
        assert!(has(&p, "mount.d.destination", "required"), "{p:?}");
        assert!(has(&p, "mount.e.create", "never created"), "{p:?}");
        assert!(has(&p, "mount.f.source", "with mode \"empty\""), "{p:?}");
    }

    #[test]
    fn the_default_app() {
        // Implicit: inherits a host entry of the sandbox's name.
        let s = parse_named("firefox", "{}", &["firefox"]).unwrap();
        let a = application(&s, "firefox");
        assert!(a.implicit && a.has_entry());
        assert_eq!(a.source_id.as_deref(), Some("firefox"));

        // Defined: run and source-id are filled in unless set.
        let s = parse_named(
            "firefox",
            "{ app+: { firefox: { environment: { A: 'b' } } } }",
            &["firefox"],
        )
        .unwrap();
        let a = application(&s, "firefox");
        assert!(!a.implicit);
        assert_eq!(
            (a.run.as_slice(), a.source_id.as_deref()),
            (&["firefox".to_string()][..], Some("firefox"))
        );
        let s = parse_named("firefox", "{ app+: { firefox: { 'source-id': null } } }", &["firefox"]).unwrap();
        assert_eq!(application(&s, "firefox").source_id, None);

        // Removed.
        let s = parse_named("firefox", "{ app+: { firefox: null } }", &["firefox"]).unwrap();
        assert!(s.app.is_empty());
    }

    #[test]
    fn apps() {
        let s = parse(
            "{ app+: {
                 tool: { run: ['tool', '--x'], id: 'org.example.Tool', keys: { Name: { default: 'Tool', de: 'Werkzeug' }, Categories: ['Utility'] },
                         actions: { new: { arguments: ['--new'], Name: 'New' }, old: null } },
                 'tool-autostart': { kind: 'autostart', app: 'tool', id: 'org.example.Tool' },
                 hidden: { kind: 'autostart', id: 'org.example.Other', suppress: true },
             } }",
        )
        .unwrap();
        let tool = application(&s, "tool");
        assert_eq!(tool.id, "org.example.Tool");
        assert!(
            matches!(&tool.keys["Name"], KeyValue::Localized { default, locales } if default == "Tool" && locales["de"] == "Werkzeug")
        );
        assert_eq!(tool.keys["Categories"], KeyValue::List(vec!["Utility".into()]));
        assert_eq!(tool.actions["new"].as_ref().unwrap().arguments, vec!["--new"]);
        assert_eq!(tool.actions["old"], None);
        // Ids are unique per kind: an application and an autostart entry share one.
        assert_eq!(s.app["tool-autostart"].kind(), Kind::Autostart);
        assert!(
            matches!(&s.app["hidden"], AppSpec::Suppress { kind: Kind::Autostart, id } if id == "org.example.Other")
        );

        let p = problems(
            "{ app+: {
                 a: { run: ['a'], id: 'same' }, b: { run: ['b'], id: 'same' },
                 c: {}, d: { run: [] }, e: { kind: 'autostart', app: 'missing' }, f: { kind: 'autostart', app: 'h' },
                 g: { run: ['g'], keys: { Exec: 'x', 'Name[de]': 'y', Terminal: 1 } },
                 h: { kind: 'autostart', id: 'h', suppress: true },
                 i: { run: ['i'], id: '1bad' },
             } }",
        );
        assert!(has(&p, "app.b", "app a also has the application id same"), "{p:?}");
        assert!(has(&p, "app.c.run", "required"), "{p:?}");
        assert!(has(&p, "app.d.run", "is empty"), "{p:?}");
        assert!(has(&p, "app.e.app", "no application named missing"), "{p:?}");
        assert!(has(&p, "app.f.app", "h isn't an application"), "{p:?}");
        assert!(has(&p, "app.g.keys.Exec", "psbox sets this key itself"), "{p:?}");
        assert!(has(&p, "app.g.keys.Name[de]", "translations go in an object"), "{p:?}");
        assert!(p.iter().any(|(k, _)| k == "app.g.keys.Terminal"), "{p:?}");
        assert!(p.iter().any(|(k, _)| k == "app.i.id"), "{p:?}");

        let p = problems("{ app+: { app: null, start: { kind: 'autostart' } } }");
        assert!(has(&p, "app.start.app", "the default app was removed"), "{p:?}");
    }

    #[test]
    fn tasks_take_the_config_with_their_overrides() {
        let s = parse(
            "{ mount+: { bin: { source: '/b', mode: 'overlay' } },
               task+: {
                 update: { run: ['update'], app: 'app', overrides: { mount+: { bin+: { mode: 'read-write' } }, display: null } },
                 shell: { script: 'echo hi' },
               } }",
        )
        .unwrap();
        let update = &s.task["update"];
        assert_eq!(update.command, vec!["update"]);
        assert_eq!(update.app.as_deref(), Some("app"));
        assert_eq!(update.spec.idle_timeout, 0);
        assert_eq!(update.spec.mount["bin"].mode, MountMode::ReadWrite);
        assert!(!update.spec.display);
        assert_eq!(s.mount["bin"].mode, MountMode::Overlay);
        assert_eq!(s.task["shell"].command, vec!["bash", "-c", "echo hi"]);
        assert!(s.writable().contains(&PathBuf::from("/b")), "a task's read-write mount");

        let p = problems(
            "{ task+: {
                 a: { run: ['a'], script: 'b' }, b: { run: ['b'], app: 'nope' },
                 c: { run: ['c'], overrides: { app: {}, network: { mode: 'bogus' } } },
                 d: { run: ['d'], app: 'app', overrides: { dbus: null } },
             } }",
        );
        assert!(has(&p, "task.a", "exactly one of run and script"), "{p:?}");
        assert!(has(&p, "task.b.app", "no application named nope"), "{p:?}");
        assert!(has(&p, "task.c.overrides.app", "can't override"), "{p:?}");
        assert!(
            p.iter().any(|(k, _)| k == "task.c (with overrides) network.mode"),
            "{p:?}"
        );
        assert!(has(&p, "task.d.app", "needs a session bus"), "{p:?}");
    }

    #[test]
    fn to_json_round_trips() {
        let s = parse(
            "local psbox = import 'psbox';
             { devices+: { camera: true }, dbus+: { talk: ['org.a.B'] }, xwayland: {}, environment+: { A: 'b' },
               mount+: { code: psbox.mount.readWrite('/home/u/Code') },
               app+: { tool: { run: ['tool'], keys: { Name: 'Tool' } } },
               task+: { t: { run: ['t'] } } }",
        )
        .unwrap();
        let json = s.to_json();
        let tasks: BTreeMap<String, Value> = s.task.iter().map(|(n, t)| (n.clone(), t.spec.to_json())).collect();
        let ctx = Context {
            name: Some("app"),
            storage: Some("/home/u/Sandbox/app".into()),
            host_entry: &|_| false,
        };
        let again = Spec::from_json(&json, &tasks, &ctx).unwrap();
        assert_eq!(again.to_json(), json);
    }
}
