//! The sandbox configuration file: one TOML file per sandbox.
//!
//! `$XDG_CONFIG_HOME/psbox/sandboxes/<name>.toml`; the file name is the
//! sandbox name. Unknown keys are errors.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Spec {
    /// `private` (default), `tmpfs`, `none`, or a host path.
    #[serde(default)]
    pub home: Home,
    /// `tmpfs` (default) mounts an empty `~/.cache`; `home` keeps it.
    #[serde(default)]
    pub cache: Cache,
    /// Device and media classes. Omitted means `["gpu", "audio"]`.
    pub devices: Option<Vec<DeviceClass>>,
    /// `portal` (default): a private session bus with portals and
    /// notifications forwarded to the host. `host`: the host session and
    /// system buses, unfiltered. `off`: no session bus.
    #[serde(default)]
    pub dbus: Dbus,
    /// Host session bus names the sandbox may call through the gateway
    /// (with `dbus = "portal"`). Empty unless listed: each name is an opt-in
    /// to that host service, unfiltered.
    #[serde(default)]
    pub dbus_talk: Vec<String>,
    /// Where links the sandbox opens go, by scheme.
    #[serde(default)]
    pub open_urls: OpenUrls,
    /// `isolated` (own network namespace with outbound connections through
    /// `pasta`), `host`, or `none` (loopback only). Unset means `isolated`
    /// when pasta is installed and `host` otherwise (see
    /// [`crate::plan::resolve_network`]).
    pub network: Option<Network>,
    /// Wayland, and Xwayland when `xwayland` is on.
    #[serde(default = "default_true")]
    pub display: bool,
    /// How X11 apps run: `off` (default), or on a private Xwayland with
    /// psbox's title bars (`builtin`) or satellite's.
    #[serde(default)]
    pub xwayland: Xwayland,
    /// How X11 apps are scaled on a scaled display, with
    /// `xwayland = "builtin"`.
    #[serde(default)]
    pub xwayland_scale: XwaylandScale,
    /// App id registered with the host portal for this sandbox's processes
    /// (a desktop file id such as `org.gnome.Calculator`).
    pub app_id: Option<String>,
    /// Extra environment for every process.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Host paths shared read-write at the same path.
    #[serde(default)]
    pub rw: Vec<String>,
    /// Host paths shared read-only at the same path.
    #[serde(default)]
    pub ro: Vec<String>,
    /// Named mounts.
    #[serde(default)]
    pub mount: BTreeMap<String, MountSpec>,
    /// Named commands (entry points) that join the sandbox.
    #[serde(default)]
    pub command: BTreeMap<String, CommandSpec>,
    /// Autostart entries, by desktop file id.
    #[serde(default)]
    pub autostart: BTreeMap<String, AutostartSpec>,
    /// Oneshot tasks (updates, cleanups) run in their own sandbox instance.
    #[serde(default)]
    pub task: BTreeMap<String, TaskSpec>,
    /// Seconds with no workload before the sandbox stops.
    #[serde(default = "default_idle")]
    pub idle_timeout: u64,
}

fn default_idle() -> u64 {
    60
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Network {
    Host,
    Isolated,
    None,
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Network::Host => "host",
            Network::Isolated => "isolated",
            Network::None => "none",
        })
    }
}

/// `xwayland`: X11 apps run on a private Xwayland that
/// `xwayland-satellite` serves, turning each X11 window into a Wayland
/// window on the host; `off` is no X server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Xwayland {
    #[default]
    Off,
    /// With psbox drawing the title bars of X11 windows when the
    /// compositor does not (`psbox decorations`).
    Builtin,
    /// Satellite on its own, with its own title bars.
    #[serde(rename = "xwayland-satellite")]
    Satellite,
}

impl fmt::Display for Xwayland {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Xwayland::Off => "off",
            Xwayland::Builtin => "builtin",
            Xwayland::Satellite => "xwayland-satellite",
        })
    }
}

/// `xwayland-scale`: how X11 apps meet a scaled display. psbox's
/// decorations proxy changes the scale xwayland-satellite sees, so it needs
/// `xwayland = "builtin"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Dbus {
    #[default]
    Portal,
    Host,
    Off,
}

/// `[open-urls]`: the sandbox's default handler for each scheme the OpenURI
/// portal accepts. `host` (the default) sends links to the host's default
/// app through the portal; `sandbox` leaves them to whatever handles them
/// inside the sandbox (a browser keeps `http` and `https`, a mail client
/// `mailto`). Apps that call the portal themselves are not affected.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenUrls {
    #[serde(default)]
    pub http: OpenIn,
    #[serde(default)]
    pub https: OpenIn,
    #[serde(default)]
    pub mailto: OpenIn,
}

impl OpenUrls {
    /// The schemes whose links go to the host.
    pub fn host_schemes(&self) -> Vec<&'static str> {
        [("http", self.http), ("https", self.https), ("mailto", self.mailto)]
            .into_iter()
            .filter(|(_, at)| *at == OpenIn::Host)
            .map(|(scheme, _)| scheme)
            .collect()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OpenIn {
    #[default]
    Host,
    Sandbox,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Home {
    #[default]
    Private,
    Tmpfs,
    None,
    Path(String),
}

impl<'de> Deserialize<'de> for Home {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(match s.as_str() {
            "private" => Home::Private,
            "tmpfs" => Home::Tmpfs,
            "none" => Home::None,
            _ => Home::Path(s),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cache {
    #[default]
    Tmpfs,
    Home,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct MountSpec {
    /// Host source. Required except for `empty`.
    pub host: Option<String>,
    /// Sandbox destination. Defaults to `host`.
    pub at: Option<String>,
    pub mode: MountMode,
    /// Skip silently when the host source is missing.
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MountMode {
    /// Read-only bind.
    Ro,
    /// Read-write bind.
    Rw,
    /// Read-only source with a writable layer that is discarded.
    Overlay,
    /// An empty tmpfs that hides whatever is at the destination.
    Empty,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct CommandSpec {
    pub exec: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Overrides the sandbox `app-id` for processes started by this command.
    /// Defaults to the desktop entry id when the command has one.
    pub app_id: Option<String>,
    /// A desktop entry that launches this command.
    pub desktop: Option<DesktopSpec>,
}

/// Where a desktop entry comes from and how it is adjusted.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct DesktopSpec {
    /// Desktop file id (file name without `.desktop`). Defaults to the
    /// command's `app-id`, else the command name.
    pub id: Option<String>,
    /// Copy this host entry (by id). Defaults to `id`. Ignored with `entry`.
    pub from: Option<String>,
    /// The whole entry, instead of copying a host entry.
    pub entry: Option<String>,
    /// Keys to set in `[Desktop Entry]`, such as `Name` or `StartupWMClass`.
    #[serde(default)]
    pub set: BTreeMap<String, String>,
    /// Arguments to drop from every `Exec` line (field codes are kept).
    #[serde(default)]
    pub remove_args: Vec<String>,
}

/// A oneshot task. It sees the sandbox's mounts, with `writable` mounts made
/// read-write, and by default gets no devices, session bus, or display.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct TaskSpec {
    /// The command line. Use either `exec` or `script`.
    pub exec: Option<Vec<String>>,
    /// A script run with `bash -c`.
    pub script: Option<String>,
    /// Overrides the sandbox `home`.
    pub home: Option<Home>,
    /// Overrides the sandbox `cache`.
    pub cache: Option<Cache>,
    /// Names of `[mount.<name>]` entries mounted read-write for the task.
    #[serde(default)]
    pub writable: Vec<String>,
    /// Device classes; none unless listed.
    pub devices: Option<Vec<DeviceClass>>,
    /// Session bus; `off` unless set.
    pub dbus: Option<Dbus>,
    /// Overrides the sandbox `network`.
    pub network: Option<Network>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// An autostart entry: start a command at login, or hide a host autostart
/// entry with the same id.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct AutostartSpec {
    /// The command to start. Required unless `hidden`.
    pub command: Option<String>,
    /// Hide the host autostart entry instead of starting anything.
    #[serde(default)]
    pub hidden: bool,
    /// Copy this host autostart entry (by id). Defaults to the entry's id.
    pub from: Option<String>,
    pub entry: Option<String>,
    #[serde(default)]
    pub set: BTreeMap<String, String>,
    #[serde(default)]
    pub remove_args: Vec<String>,
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

impl FromStr for DeviceClass {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        Ok(match s {
            "gpu" => DeviceClass::Gpu,
            "camera" => DeviceClass::Camera,
            "gamepad" => DeviceClass::Gamepad,
            "alsa" => DeviceClass::Alsa,
            "security-key" => DeviceClass::SecurityKey,
            "audio" => DeviceClass::Audio,
            _ => {
                let Some(rest) = s.strip_prefix("usb:") else {
                    return Err(format!("unknown device class {s:?}"));
                };
                let (v, p) = rest
                    .split_once(':')
                    .ok_or_else(|| format!("expected usb:<vendor>:<product>, got {s:?}"))?;
                let hex = |x: &str| {
                    if x.len() != 4 {
                        return Err(format!("USB ids are four hex digits, got {x:?}"));
                    }
                    u16::from_str_radix(x, 16).map_err(|_| format!("invalid USB id {x:?}"))
                };
                DeviceClass::Usb {
                    vendor: hex(v)?,
                    product: hex(p)?,
                }
            }
        })
    }
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

impl<'de> Deserialize<'de> for DeviceClass {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
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

/// Command and mount names.
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

/// A config that does not parse or validate.
#[derive(Debug)]
pub struct ConfigError {
    /// The sandbox, when the config is a named one.
    pub name: Option<String>,
    pub file: Option<PathBuf>,
    pub problem: ConfigProblem,
}

#[derive(Debug)]
pub enum ConfigProblem {
    /// TOML syntax or schema (unknown keys, wrong types): parsing stops at
    /// the first error. `location` is `line L, column C` and `excerpt` the
    /// source lines with a caret, when known.
    Parse {
        location: Option<String>,
        message: String,
        excerpt: Vec<String>,
    },
    /// Every validation error, as (config entry, message).
    Invalid(Vec<(String, String)>),
}

impl ConfigProblem {
    fn parse(e: impl fmt::Display) -> Self {
        let text = e.to_string();
        let lines: Vec<&str> = text.lines().collect();
        match lines.first().and_then(|l| l.strip_prefix("TOML parse error at ")) {
            Some(location) => {
                let excerpt: Vec<String> = lines[1..]
                    .iter()
                    .take_while(|l| l.contains('|'))
                    .filter(|l| !l.trim().eq("|"))
                    .map(|l| l.to_string())
                    .collect();
                let rest = lines[1..].iter().skip_while(|l| l.contains('|'));
                Self::Parse {
                    location: Some(location.to_string()),
                    message: rest.copied().collect::<Vec<_>>().join(" "),
                    excerpt,
                }
            }
            None => Self::Parse {
                location: None,
                message: text.trim().to_string(),
                excerpt: Vec::new(),
            },
        }
    }
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
        // Unnamed sandboxes have settings made from flags, not a config.
        let (subject, plural) = match &self.name {
            Some(name) => (format!("{}'s config", paint(BOLD, name)), false),
            None => ("the settings".to_string(), true),
        };
        let what = match (&self.problem, plural) {
            (ConfigProblem::Parse { .. }, false) => "doesn't parse".to_string(),
            (ConfigProblem::Parse { .. }, true) => "don't parse".to_string(),
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
                let mut t = Table::new(&["ENTRY", "ERROR"]);
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

impl Spec {
    /// Parses and validates a configuration file body.
    pub fn parse(text: &str) -> Result<Self> {
        Self::parse_in(text, None)
    }

    /// Parses a configuration, resolving `extends` against
    /// `<config_dir>/profiles` (then the built-in profiles).
    pub fn parse_in(text: &str, config_dir: Option<&Path>) -> Result<Self> {
        Self::check(text, config_dir).map_err(|problem| {
            ConfigError {
                name: None,
                file: None,
                problem,
            }
            .into()
        })
    }

    fn check(text: &str, config_dir: Option<&Path>) -> std::result::Result<Self, ConfigProblem> {
        let table: toml::Table = toml::from_str(text).map_err(ConfigProblem::parse)?;
        let merged = resolve_profiles(table, config_dir, &mut Vec::new())
            .map_err(|e| ConfigProblem::Invalid(vec![("extends".into(), format!("{e:#}"))]))?;
        let spec: Spec = Spec::deserialize(merged).map_err(ConfigProblem::parse)?;
        let errors = spec.validate();
        match errors.is_empty() {
            true => Ok(spec),
            false => Err(ConfigProblem::Invalid(errors)),
        }
    }

    /// Loads `<config_dir>/sandboxes/<name>.toml`.
    pub fn load(name: &str, config_dir: &Path) -> Result<Self> {
        validate_sandbox_name(name)?;
        let file = config_dir.join("sandboxes").join(format!("{name}.toml"));
        match std::fs::read_to_string(&file) {
            Ok(text) => Self::check(&text, Some(config_dir)).map_err(|problem| {
                ConfigError {
                    name: Some(name.to_string()),
                    file: Some(file),
                    problem,
                }
                .into()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(crate::ui::with_fix(
                anyhow!("no sandbox named {name} · {} doesn't exist", crate::ui::tilde(&file)),
                format!(
                    "{} shows the configured sandboxes; {} runs without one",
                    crate::ui::code("psbox list"),
                    crate::ui::code("psbox sandbox")
                ),
            )),
            Err(e) => Err(e).with_context(|| format!("read {}", file.display())),
        }
    }

    /// The spec a task runs with: the sandbox's mounts (named `writable`
    /// ones read-write), the task's own command, devices, bus, and home, no
    /// display, and stopping as soon as the task is done.
    pub fn for_task(&self, task: &str) -> Result<Self> {
        let t = self.task.get(task).with_context(|| format!("unknown task {task:?}"))?;
        let mut spec = self.clone();
        let exec = match (&t.exec, &t.script) {
            (Some(e), _) => e.clone(),
            (None, Some(script)) => vec!["bash".into(), "-c".into(), script.clone()],
            (None, None) => unreachable!("validated"),
        };
        if let Some(home) = &t.home {
            spec.home = home.clone();
        }
        if let Some(cache) = t.cache {
            spec.cache = cache;
        }
        for name in &t.writable {
            if let Some(m) = spec.mount.get_mut(name) {
                m.mode = MountMode::Rw;
            }
        }
        spec.devices = Some(t.devices.clone().unwrap_or_default());
        spec.dbus = t.dbus.unwrap_or(Dbus::Off);
        if t.network.is_some() {
            spec.network = t.network;
        }
        spec.display = false;
        spec.xwayland = Xwayland::Off;
        spec.xwayland_scale = XwaylandScale::Native;
        spec.env.extend(t.env.clone());
        spec.command = BTreeMap::from([(
            task.to_string(),
            CommandSpec {
                exec,
                env: BTreeMap::new(),
                app_id: None,
                desktop: None,
            },
        )]);
        spec.autostart.clear();
        spec.task.clear();
        spec.idle_timeout = 0;
        Ok(spec)
    }

    /// Loads every `<config_dir>/sandboxes/*.toml`, in name order. Each
    /// file is independent: one that fails reports its own error.
    pub fn load_all(config_dir: &Path) -> Result<Vec<(String, Result<Self>)>> {
        Ok(Self::names(config_dir)?
            .into_iter()
            .map(|name| {
                let spec = validate_sandbox_name(&name).and_then(|_| Self::load(&name, config_dir));
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
                if path.extension()? != "toml" {
                    return None;
                }
                Some(path.file_stem()?.to_str()?.to_string())
            })
            .collect();
        names.sort();
        Ok(names)
    }

    /// Checks everything parsing does not, and returns every problem found,
    /// as (config entry, message).
    fn validate(&self) -> Vec<(String, String)> {
        let mut problems = Vec::new();
        let mut check = |entry: String, r: Result<()>| {
            if let Err(e) = r {
                problems.push((entry, format!("{e:#}")));
            }
        };
        for key in self.env.keys() {
            check("env".into(), validate_env_key(key));
        }
        if let Some(id) = &self.app_id {
            check("app-id".into(), validate_app_id(id));
        }
        if self.xwayland_scale != XwaylandScale::Native && self.xwayland != Xwayland::Builtin {
            check(
                "xwayland-scale".into(),
                Err(anyhow!(
                    "needs xwayland = \"builtin\", whose title bar proxy does the scaling"
                )),
            );
        }
        if !self.dbus_talk.is_empty() && self.dbus != Dbus::Portal {
            check(
                "dbus-talk".into(),
                Err(anyhow!(
                    "needs dbus = \"portal\"; the host bus already reaches every name"
                )),
            );
        }
        for name in &self.dbus_talk {
            check("dbus-talk".into(), validate_talk_name(name));
        }
        for (name, m) in &self.mount {
            let entry = format!("[mount.{name}]");
            check(entry.clone(), validate_name("mount", name));
            check(
                entry,
                match (m.mode, &m.host, &m.at) {
                    (MountMode::Empty, Some(_), _) => Err(anyhow!("empty mounts take no host")),
                    (MountMode::Empty, None, None) => Err(anyhow!("empty mounts need at")),
                    (MountMode::Empty, None, Some(_)) => Ok(()),
                    (_, None, _) => Err(anyhow!("host is required")),
                    _ => Ok(()),
                },
            );
        }
        for (name, c) in &self.command {
            let entry = format!("[command.{name}]");
            check(entry.clone(), validate_name("command", name));
            if c.exec.is_empty() {
                check(entry.clone(), Err(anyhow!("exec is empty")));
            }
            for key in c.env.keys() {
                check(entry.clone(), validate_env_key(key));
            }
            if let Some(id) = &c.app_id {
                check(entry.clone(), validate_app_id(id));
            }
            if let Some(d) = &c.desktop {
                for id in d.id.iter().chain(d.from.iter()) {
                    check(entry.clone(), validate_app_id(id).context("desktop entry"));
                }
            }
        }
        for (name, t) in &self.task {
            let entry = format!("[task.{name}]");
            if !valid_word(name) {
                check(
                    entry.clone(),
                    Err(anyhow!("invalid task name: use letters, digits, '_', '-'")),
                );
            }
            check(
                entry.clone(),
                match (&t.exec, &t.script) {
                    (Some(e), None) if e.is_empty() => Err(anyhow!("exec is empty")),
                    (Some(_), None) | (None, Some(_)) => Ok(()),
                    _ => Err(anyhow!("use exactly one of exec and script")),
                },
            );
            for m in &t.writable {
                check(
                    entry.clone(),
                    match self.mount.get(m) {
                        None => Err(anyhow!("no mount named {m:?}")),
                        Some(spec) if spec.mode == MountMode::Empty => Err(anyhow!("mount {m:?} is empty")),
                        Some(_) => Ok(()),
                    },
                );
            }
            for key in t.env.keys() {
                check(entry.clone(), validate_env_key(key));
            }
        }
        for (id, a) in &self.autostart {
            let entry = format!("[autostart.{id}]");
            check(entry.clone(), validate_app_id(id));
            check(
                entry,
                match (&a.command, a.hidden) {
                    (Some(_), true) => Err(anyhow!("use either command or hidden")),
                    (None, false) => Err(anyhow!("command is required unless hidden")),
                    (Some(c), false) if !self.command.contains_key(c) && !self.command.is_empty() => {
                        Err(anyhow!("unknown command {c:?}"))
                    }
                    _ => Ok(()),
                },
            );
        }
        problems
    }

    /// Device classes, applying the default when omitted.
    pub fn device_classes(&self) -> Vec<DeviceClass> {
        self.devices
            .clone()
            .unwrap_or_else(|| vec![DeviceClass::Gpu, DeviceClass::Audio])
    }
}

/// App ids follow desktop file ids: dot-separated elements of letters,
/// digits, `_`, and `-`, not starting with a digit. A single element (such
/// as `code-oss`) is a valid desktop file id.
fn validate_app_id(id: &str) -> Result<()> {
    let ok = id.len() <= 255
        && id.split('.').all(|e| {
            !e.is_empty()
                && !e.starts_with(|c: char| c.is_ascii_digit())
                && e.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        });
    if !ok {
        bail!("invalid app id {id:?}");
    }
    Ok(())
}

/// A well-known host bus name for `dbus-talk`: dot-separated elements of
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

/// Built-in profiles, by name.
fn builtin_profile(name: &str) -> Option<&'static str> {
    match name {
        "browser" => Some(include_str!("assets/profiles/browser.toml")),
        _ => None,
    }
}

/// Top-level lists that accumulate across profiles instead of replacing.
const ACCUMULATING: [&str; 4] = ["rw", "ro", "devices", "dbus-talk"];

/// Applies `extends`: each listed profile (itself possibly extending
/// others) is merged in order, then `table` on top. Tables merge key by
/// key, `rw`/`ro`/`devices` accumulate, and anything else from later
/// sources replaces earlier values.
fn resolve_profiles(mut table: toml::Table, config_dir: Option<&Path>, stack: &mut Vec<String>) -> Result<toml::Table> {
    let extends = match table.remove("extends") {
        None => return Ok(table),
        Some(toml::Value::Array(items)) => items,
        Some(_) => bail!("extends must be a list of profile names"),
    };
    let mut merged = toml::Table::new();
    for item in extends {
        let name = item
            .as_str()
            .context("extends must be a list of profile names")?
            .to_string();
        if !valid_word(&name) {
            bail!("invalid profile name {name:?}");
        }
        if stack.contains(&name) {
            bail!("profile {name} extends itself ({} -> {name})", stack.join(" -> "));
        }
        let user = config_dir.map(|d| d.join("profiles").join(format!("{name}.toml")));
        let text = match user.as_ref().filter(|p| p.is_file()) {
            Some(p) => std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?,
            None => builtin_profile(&name)
                .with_context(|| format!("unknown profile {name:?}"))?
                .to_string(),
        };
        let profile: toml::Table = toml::from_str(&text).with_context(|| format!("profile {name}"))?;
        stack.push(name);
        let profile = resolve_profiles(profile, config_dir, stack)?;
        stack.pop();
        merge_tables(&mut merged, profile, true);
    }
    merge_tables(&mut merged, table, true);
    Ok(merged)
}

fn merge_tables(base: &mut toml::Table, over: toml::Table, top: bool) {
    for (key, value) in over {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge_tables(b, o, false),
            (Some(toml::Value::Array(b)), toml::Value::Array(o)) if top && ACCUMULATING.contains(&key.as_str()) => {
                for v in o {
                    if !b.contains(&v) {
                        b.push(v);
                    }
                }
            }
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

fn validate_env_key(key: &str) -> Result<()> {
    let mut chars = key.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    if !first_ok || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("invalid environment variable name {key:?}");
    }
    Ok(())
}

/// What path tokens expand to.
///
/// - `~`, `~/...`: the host home (the same path inside the sandbox)
/// - `@root`, `@root/...`: the sandbox root (`$PSBOX_ROOT`, default `~/Sandbox`)
/// - `@self`, `@self/...`: `<root>/<name>`, this sandbox's default home
/// - `@self.<type>`, `@self.<type>/...`: `<root>/<name>.<type>`
pub struct Tokens<'a> {
    pub home: &'a Path,
    pub root: &'a Path,
    pub name: &'a str,
}

impl Tokens<'_> {
    /// Expands tokens and requires an absolute, `..`-free result.
    pub fn expand(&self, value: &str) -> Result<PathBuf> {
        let path = if let Some(rest) = token_rest(value, "~") {
            join(self.home, rest)
        } else if let Some(rest) = token_rest(value, "@root") {
            join(self.root, rest)
        } else if let Some(rest) = value.strip_prefix("@self") {
            let (head, tail) = match rest.find('/') {
                Some(i) => (&rest[..i], Some(&rest[i + 1..])),
                None => (rest, None),
            };
            let dir = match head.strip_prefix('.') {
                None if head.is_empty() => self.root.join(self.name),
                Some(kind) if valid_word(kind) => self.root.join(format!("{}.{kind}", self.name)),
                _ => bail!("{value:?}: expected @self, @self/<path>, or @self.<type>"),
            };
            join(&dir, tail.unwrap_or(""))
        } else if value.starts_with('@') {
            bail!("{value:?}: unknown token (use @root or @self)");
        } else {
            PathBuf::from(value)
        };
        if !path.is_absolute() {
            bail!("{value:?} must be absolute (or start with ~, @root, or @self)");
        }
        if path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            bail!("{value:?} must not contain '..'");
        }
        Ok(path)
    }
}

/// For `token` or `token/rest`, returns `rest` (empty for the bare token).
fn token_rest<'v>(value: &'v str, token: &str) -> Option<&'v str> {
    let rest = value.strip_prefix(token)?;
    if rest.is_empty() {
        Some("")
    } else {
        rest.strip_prefix('/')
    }
}

fn join(base: &Path, rest: &str) -> PathBuf {
    if rest.is_empty() {
        base.to_path_buf()
    } else {
        base.join(rest)
    }
}

fn valid_word(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
        env = { EDITOR = "vi", QT_QPA_PLATFORM = "xcb" }
        rw = ["~/Downloads", "~/Music"]
        devices = ["gpu", "audio", "usb:0483:5740"]

        [mount.library]
        host = "/srv/library"
        at = "~/Library"
        mode = "rw"

        [mount.etc-ssh]
        at = "/etc/ssh"
        mode = "empty"

        [command.code]
        exec = ["code"]

        [command.code-url-handler]
        exec = ["code-url-handler"]
        app-id = "code-url-handler"
    "#;

    #[test]
    fn missing_sandboxes_directory_means_none_but_unreadable_is_an_error() {
        let dir = std::env::temp_dir().join(format!("psbox-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(Spec::names(&dir).unwrap().is_empty());
        std::fs::write(dir.join("sandboxes"), "").unwrap();
        assert!(Spec::names(&dir).is_err(), "a file where the directory belongs");
        std::fs::remove_file(dir.join("sandboxes")).unwrap();
        std::fs::create_dir(dir.join("sandboxes")).unwrap();
        std::fs::write(dir.join("sandboxes/b.toml"), "").unwrap();
        std::fs::write(dir.join("sandboxes/a.toml"), "").unwrap();
        std::fs::write(dir.join("sandboxes/notes.txt"), "").unwrap();
        assert_eq!(Spec::names(&dir).unwrap(), ["a", "b"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reports_every_validation_problem() {
        let e = Spec::parse("env = { \"1X\" = \"a\" }\n[mount.m]\nmode = \"ro\"\n[task.t]\n").unwrap_err();
        assert_eq!(
            e.to_string(),
            "the settings have 3 errors\n\n  ENTRY       ERROR\n  env         invalid environment variable name \"1X\"\n  [mount.m]   host is required\n  [task.t]    use exactly one of exec and script"
        );
        let c = e.downcast_ref::<ConfigError>().unwrap();
        assert_eq!(
            c.summary(),
            "3 errors · env: invalid environment variable name \"1X\", …"
        );

        let e = Spec::parse("home = = \"tmpfs\"\n").unwrap_err();
        let c = e.downcast_ref::<ConfigError>().unwrap();
        assert_eq!(c.summary(), "line 1, column 8: extra `=`, expected nothing");
        assert_eq!(
            e.to_string(),
            "the settings don't parse\n\n  line 1, column 8: extra `=`, expected nothing\n  1 │ home = = \"tmpfs\"\n    │        ^"
        );
    }

    #[test]
    fn parses_full_example() {
        let s = Spec::parse(SAMPLE).unwrap();
        assert_eq!(s.home, Home::Private);
        assert_eq!(s.rw.len(), 2);
        assert_eq!(s.mount["library"].mode, MountMode::Rw);
        assert_eq!(
            s.device_classes()[2],
            DeviceClass::Usb {
                vendor: 0x0483,
                product: 0x5740
            }
        );
        assert_eq!(s.command["code-url-handler"].exec, vec!["code-url-handler"]);
        assert_eq!(s.idle_timeout, 60);
        assert_eq!(s.dbus, Dbus::Portal);
        assert!(s.xwayland == Xwayland::Off && s.network.is_none());
        for (value, x) in [
            ("off", Xwayland::Off),
            ("builtin", Xwayland::Builtin),
            ("xwayland-satellite", Xwayland::Satellite),
        ] {
            let parsed = Spec::parse(&format!("xwayland = \"{value}\"")).unwrap().xwayland;
            assert_eq!((parsed, parsed.to_string()), (x, value.to_string()));
        }
        for (value, scale) in [
            ("native", XwaylandScale::Native),
            ("round-up", XwaylandScale::RoundUp),
            ("compositor", XwaylandScale::Compositor),
        ] {
            let parsed = Spec::parse(&format!("xwayland = \"builtin\"\nxwayland-scale = \"{value}\""))
                .unwrap()
                .xwayland_scale;
            assert_eq!((parsed, parsed.to_string()), (scale, value.to_string()));
            assert_eq!(value.parse::<XwaylandScale>().unwrap(), scale);
        }
        // Only the decorations proxy can change the scale satellite sees.
        for xwayland in ["off", "xwayland-satellite"] {
            let text = format!("xwayland = \"{xwayland}\"\nxwayland-scale = \"round-up\"");
            assert!(Spec::parse(&text).is_err(), "{xwayland}");
        }
        Spec::parse("xwayland = \"xwayland-satellite\"\nxwayland-scale = \"native\"").unwrap();
        assert!(Spec::parse("xwayland = true").is_err());
        assert!(Spec::parse("xwayland = \"satellite\"").is_err());
        assert_eq!(
            s.command["code-url-handler"].app_id.as_deref(),
            Some("code-url-handler")
        );
    }

    #[test]
    fn profiles_merge() {
        let dir = std::env::temp_dir().join(format!("psbox-profiles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("profiles")).unwrap();
        std::fs::write(
            dir.join("profiles/dev.toml"),
            "extends = [\"ssh\"]\nrw = [\"~/Projects\", \"~/Music\"]\ncache = \"home\"\n[env]\nEDITOR = \"vi\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("profiles/ssh.toml"),
            "[mount.ssh]\nhost = \"~/.ssh\"\nmode = \"ro\"\noptional = true\n[mount.etc-ssh]\nat = \"/etc/ssh\"\nmode = \"empty\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("profiles/loop.toml"), "extends = [\"loop\"]\n").unwrap();

        let s = Spec::parse_in(
            "extends = [\"dev\"]\nrw = [\"~/Music\", \"~/Downloads\"]\ncache = \"tmpfs\"\nenv = { EDITOR = \"code\", LANG = \"C\" }\n",
            Some(&dir),
        )
        .unwrap();
        assert_eq!(s.rw, vec!["~/Projects", "~/Music", "~/Downloads"]);
        assert_eq!(s.cache, Cache::Tmpfs, "the sandbox wins over its profiles");
        assert_eq!(s.env["EDITOR"], "code");
        assert_eq!(s.env["LANG"], "C");
        assert!(s.mount.contains_key("ssh") && s.mount.contains_key("etc-ssh"));

        let browser = Spec::parse("extends = [\"browser\"]").unwrap();
        assert!(browser.device_classes().contains(&DeviceClass::Camera));
        assert!(browser.device_classes().contains(&DeviceClass::SecurityKey));
        assert_eq!(browser.rw, vec!["~/Downloads/"]);

        assert!(
            Spec::parse_in("extends = [\"loop\"]", Some(&dir))
                .unwrap_err()
                .to_string()
                .contains("extends itself")
        );
        assert!(Spec::parse("extends = [\"nope\"]").is_err());
        assert!(Spec::parse("extends = \"dev\"").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tasks() {
        let s = Spec::parse(
            r#"
            devices = ["gpu", "audio"]
            [mount.bin]
            host = "@self.bin"
            at = "~/.bin"
            mode = "overlay"

            [task.update]
            home = "tmpfs"
            writable = ["bin"]
            script = "set -e; cd ~/.bin; echo updating"
            "#,
        )
        .unwrap();
        let t = s.for_task("update").unwrap();
        assert_eq!(t.home, Home::Tmpfs);
        assert_eq!(t.mount["bin"].mode, MountMode::Rw);
        assert_eq!(t.devices, Some(vec![]));
        assert_eq!(t.dbus, Dbus::Off);
        assert_eq!(t.xwayland, Xwayland::Off);
        assert_eq!(t.idle_timeout, 0);
        assert_eq!(t.command["update"].exec[..2], ["bash".to_string(), "-c".to_string()]);
        assert!(s.for_task("nope").is_err());
        assert!(Spec::parse("[task.x]\nexec = [\"a\"]\nscript = \"b\"").is_err());
        assert!(Spec::parse("[task.x]\nexec = [\"a\"]\nwritable = [\"missing\"]").is_err());
        assert!(Spec::parse("[task.\"a.b\"]\nexec = [\"a\"]").is_err());
    }

    #[test]
    fn desktop_and_autostart_entries() {
        let s = Spec::parse(
            r#"
            [command.steam]
            exec = ["steam"]
            desktop = { id = "com.valvesoftware.Steam", set = { StartupWMClass = "steam" } }

            [autostart.steam-autostart]
            hidden = true

            [autostart.com.valvesoftware.Steam]
            command = "steam"
            remove-args = ["-silent"]
            "#,
        );
        // Dotted ids need quoting as TOML keys; unquoted they nest.
        assert!(s.is_err());
        let s = Spec::parse(
            r#"
            [command.steam]
            exec = ["steam"]
            desktop = { id = "com.valvesoftware.Steam", set = { StartupWMClass = "steam" } }

            [autostart.steam-autostart]
            hidden = true

            [autostart."com.valvesoftware.Steam"]
            command = "steam"
            remove-args = ["-silent"]
            "#,
        )
        .unwrap();
        let d = s.command["steam"].desktop.as_ref().unwrap();
        assert_eq!(d.set["StartupWMClass"], "steam");
        assert!(s.autostart["steam-autostart"].hidden);
        assert_eq!(s.autostart["com.valvesoftware.Steam"].remove_args, vec!["-silent"]);
        assert!(Spec::parse("[autostart.x]\nhidden = true\ncommand = \"a\"").is_err());
        assert!(Spec::parse("[autostart.x]\nfrom = \"y\"").is_err());
        assert!(Spec::parse("[command.a]\nexec = [\"a\"]\n[autostart.x]\ncommand = \"b\"").is_err());
    }

    #[test]
    fn dbus_and_app_ids() {
        assert_eq!(Spec::parse("dbus = \"off\"").unwrap().dbus, Dbus::Off);
        assert_eq!(Spec::parse("dbus = \"host\"").unwrap().dbus, Dbus::Host);
        assert!(Spec::parse("app-id = \"org.gnome.Calculator\"").is_ok());
        assert!(Spec::parse("app-id = \"org..x\"").is_err());
        assert!(Spec::parse("app-id = \"org.1x\"").is_err());
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(Spec::parse("no-such-key = []").is_err());
        assert!(Spec::parse("[mount.x]\nhost = \"/a\"\nmode = \"rw\"\nextra = 1").is_err());
    }

    #[test]
    fn dbus_talk_is_opt_in_and_needs_the_portal_bus() {
        assert!(Spec::parse("").unwrap().dbus_talk.is_empty());
        let s = Spec::parse("dbus-talk = [\"org.freedesktop.secrets\", \"org.kde.kwalletd6\"]").unwrap();
        assert_eq!(s.dbus_talk, ["org.freedesktop.secrets", "org.kde.kwalletd6"]);
        assert!(Spec::parse("dbus = \"host\"\ndbus-talk = [\"org.a.b\"]").is_err());
        assert!(Spec::parse("dbus = \"off\"\ndbus-talk = [\"org.a.b\"]").is_err());
        for bad in [
            "org",
            ":1.5",
            "org..b",
            "org.1b",
            "org.a b",
            "org.freedesktop.DBus",
            "org.freedesktop.portal.Desktop",
            "org.freedesktop.Notifications",
        ] {
            assert!(Spec::parse(&format!("dbus-talk = [{bad:?}]")).is_err(), "{bad}");
        }
    }

    #[test]
    fn rejects_bad_values() {
        assert!(Spec::parse("devices = [\"webcam\"]").is_err());
        assert!(Spec::parse("devices = [\"usb:483:5740\"]").is_err());
        assert!(Spec::parse("env = { \"1BAD\" = \"x\" }").is_err());
        assert!(Spec::parse("[mount.x]\nmode = \"rw\"").is_err());
        assert!(Spec::parse("[mount.x]\nhost = \"/a\"\nmode = \"empty\"").is_err());
        assert!(Spec::parse("[command.x]\nexec = []").is_err());
    }

    #[test]
    fn home_values() {
        assert_eq!(Spec::parse("home = \"tmpfs\"").unwrap().home, Home::Tmpfs);
        assert_eq!(
            Spec::parse("home = \"@root/other\"").unwrap().home,
            Home::Path("@root/other".into())
        );
    }

    #[test]
    fn expands_tokens() {
        let t = Tokens {
            home: Path::new("/home/u"),
            root: Path::new("/home/u/Sandbox"),
            name: "retroarch",
        };
        let ok = |v: &str| t.expand(v).unwrap();
        assert_eq!(ok("~"), Path::new("/home/u"));
        assert_eq!(ok("~/Music"), Path::new("/home/u/Music"));
        assert_eq!(ok("@root"), Path::new("/home/u/Sandbox"));
        assert_eq!(ok("@root/steam"), Path::new("/home/u/Sandbox/steam"));
        assert_eq!(ok("@self"), Path::new("/home/u/Sandbox/retroarch"));
        assert_eq!(ok("@self/x"), Path::new("/home/u/Sandbox/retroarch/x"));
        assert_eq!(ok("@self.bin"), Path::new("/home/u/Sandbox/retroarch.bin"));
        assert_eq!(ok("@self.bin/app"), Path::new("/home/u/Sandbox/retroarch.bin/app"));
        assert_eq!(ok("/etc/ssh"), Path::new("/etc/ssh"));
        for bad in [
            "relative",
            "~/../x",
            "~user",
            "@rootx",
            "@data/bin",
            "@self.",
            "@self.a.b",
            "@selfish",
        ] {
            assert!(t.expand(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn sandbox_names_never_contain_dots() {
        assert!(validate_sandbox_name("code-insiders").is_ok());
        assert!(validate_sandbox_name("my_app").is_ok());
        for bad in ["retroarch.bin", ".", "", "-x", "a/b", "sandbox"] {
            assert!(validate_sandbox_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn device_class_round_trip() {
        for s in ["gpu", "camera", "gamepad", "alsa", "audio", "usb:0483:5740"] {
            assert_eq!(s.parse::<DeviceClass>().unwrap().to_string(), s);
        }
    }
}
