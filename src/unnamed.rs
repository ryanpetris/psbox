//! `psbox sandbox`: options for an unnamed, oneshot sandbox and the settings
//! file its supervisor reads in place of a named config.
//!
//! The settings are a config like any other, evaluated by the client:
//! profiles, then a file, then each `-c KEY=VALUE` as a Jsonnet object that
//! merges into the key, then the `--bind` and `--ro-bind` mounts. The
//! evaluated JSON goes to the supervisor, so it needs no context from the
//! caller.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::jsonnet::{self, Layer};
use crate::plan::HostEnv;
use crate::spec::{self, Spec};

/// Mount names `--bind` and `--ro-bind` make, which `-c` cannot write.
pub const BIND_PREFIX: &str = "bind-";
pub const RO_BIND_PREFIX: &str = "ro-bind-";

#[derive(clap::Args, Debug, Default)]
pub struct Options {
    /// Set a config key, such as `xwayland.mode=builtin` or
    /// `environment.EDITOR=vi`; repeat for more. Quote a name containing a
    /// dot: `mount."a.b".source=/x`.
    #[arg(short = 'c', long = "config", value_name = "KEY=VALUE")]
    config: Vec<String>,
    /// Start from a profile: `~/.config/psbox/profiles/NAME.libsonnet`, or
    /// a built-in one; repeat for more.
    #[arg(long, value_name = "NAME")]
    profile: Vec<String>,
    /// Start from a config file, after the profiles.
    #[arg(long, value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    file: Option<PathBuf>,
    /// Give the sandbox the portal identity of this desktop entry id; the
    /// host portal must accept it.
    #[arg(long, value_name = "ID")]
    app_id: Option<String>,
    /// Share a host path read-write, at the same path or at DST.
    #[arg(long, value_name = "SRC[:DST]", value_hint = clap::ValueHint::AnyPath)]
    bind: Vec<String>,
    /// Share a host path read-only, at the same path or at DST.
    #[arg(long, value_name = "SRC[:DST]", value_hint = clap::ValueHint::AnyPath)]
    ro_bind: Vec<String>,
}

/// What `psbox sandbox` leaves for the supervisor.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Settings {
    /// The evaluated config.
    pub config: Value,
    /// The portal identity, from `--app-id`.
    pub app_id: Option<String>,
}

impl Settings {
    /// The checked config.
    pub fn spec(&self) -> Result<Spec> {
        let none = |_: &str| false;
        let ctx = spec::Context {
            name: None,
            storage: None,
            host_entry: &none,
        };
        let mut spec =
            Spec::from_json(&self.config, &Default::default(), &ctx).map_err(|problem| spec::ConfigError {
                name: None,
                file: None,
                problem,
            })?;
        // The supervisor stops as soon as the command has exited.
        spec.idle_timeout = 0;
        Ok(spec)
    }
}

/// What a `-c` value means at a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    Bool,
    Number,
    Text,
    /// Text that names a host path, made absolute against the working
    /// directory.
    Path,
    /// JSON: a list or an object.
    Json,
}

/// The kind of value a key takes, and whether `null` removes it.
fn want(key: &[String]) -> (Want, bool) {
    let k: Vec<&str> = key.iter().map(String::as_str).collect();
    match k.as_slice() {
        ["sandbox", "idle-timeout"] => (Want::Number, false),
        ["sandbox", "32-bit"] => (Want::Bool, false),
        ["home" | "dbus" | "network" | "display" | "xwayland"] => (Want::Json, true),
        ["home", "path"] => (Want::Path, false),
        [
            "devices",
            "gpu" | "audio" | "camera" | "gamepad" | "security-key" | "alsa",
        ] => (Want::Bool, false),
        ["devices", "usb"] | ["dbus", "talk"] => (Want::Json, false),
        ["environment", _] => (Want::Text, true),
        ["mount", _] => (Want::Json, true),
        ["mount", _, "source" | "destination"] => (Want::Path, false),
        ["mount", _, "optional" | "create"] => (Want::Bool, false),
        _ => (Want::Text, false),
    }
}

/// Splits `KEY=VALUE` and the key into its segments; a segment may be a
/// JSON string, to hold a `.` or `=`.
fn split_setting(setting: &str) -> Result<(Vec<String>, &str)> {
    let mut key = Vec::new();
    let mut rest = setting;
    loop {
        let segment;
        if rest.starts_with('"') {
            let mut end = None;
            let mut escaped = false;
            for (i, ch) in rest.char_indices().skip(1) {
                match (escaped, ch) {
                    (true, _) => escaped = false,
                    (false, '\\') => escaped = true,
                    (false, '"') => {
                        end = Some(i);
                        break;
                    }
                    _ => {}
                }
            }
            let end = end.ok_or_else(|| anyhow!("-c {setting:?}: unterminated quote"))?;
            segment = serde_json::from_str::<String>(&rest[..=end]).with_context(|| format!("-c {setting:?}"))?;
            rest = &rest[end + 1..];
        } else {
            let end = rest.find(['.', '=']).unwrap_or(rest.len());
            segment = rest[..end].to_string();
            rest = &rest[end..];
        }
        if segment.is_empty() {
            bail!("-c {setting:?}: expected KEY=VALUE with a dotted KEY");
        }
        key.push(segment);
        match rest.chars().next() {
            Some('.') => rest = &rest[1..],
            Some('=') => return Ok((key, &rest[1..])),
            _ => bail!("-c {setting:?}: expected KEY=VALUE"),
        }
    }
}

/// `~` and `~/...` against the home, and relative paths against `cwd`,
/// resolving `.` and `..` without touching the filesystem, so symlinks in a
/// share's path stay where the user named them.
fn cli_path(p: &str, host: &HostEnv, cwd: &Path) -> String {
    let raw = match p.strip_prefix('~') {
        Some("") => host.home.clone(),
        Some(rest) if rest.starts_with('/') => host.home.join(&rest[1..]),
        _ => cwd.join(p),
    };
    let mut out = PathBuf::new();
    for c in raw.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out.to_string_lossy().into_owned()
}

/// The value a `-c` setting gives its key.
fn setting_value(key: &[String], value: &str, host: &HostEnv, cwd: &Path) -> Result<Value> {
    let (want, nullable) = want(key);
    let shown = key.join(".");
    if value == "null" && nullable {
        return Ok(Value::Null);
    }
    if value.starts_with(['"', '{', '[']) {
        return serde_json::from_str(value).with_context(|| format!("-c {shown}: {value:?} isn't valid JSON"));
    }
    Ok(match want {
        Want::Bool => match value {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => bail!("-c {shown}: expected true or false, got {value:?}"),
        },
        Want::Number => Value::from(
            value
                .parse::<u64>()
                .with_context(|| format!("-c {shown}: expected a whole number, got {value:?}"))?,
        ),
        Want::Text => Value::String(value.to_string()),
        Want::Path => Value::String(cli_path(value, host, cwd)),
        Want::Json => bail!(
            "-c {shown}: expected {}JSON, got {value:?}",
            if nullable { "null or " } else { "" }
        ),
    })
}

/// Jsonnet for an object that sets `key` to `value`, merging into what is
/// already there; a section a profile set to null is started afresh.
fn setting_snippet(key: &[String], value: &Value) -> String {
    let name = serde_json::to_string(&key[0]).expect("strings serialize");
    if key.len() == 1 {
        return format!("{{ {name}: {value} }}");
    }
    let inner = setting_snippet(&key[1..], value);
    format!("{{ {name}: (if {name} in super && super[{name}] != null then super[{name}] else {{}}) + {inner} }}")
}

impl Options {
    /// The settings: evaluated, checked, with paths made absolute (relative
    /// ones against `cwd`).
    pub fn settings(&self, host: &HostEnv, cwd: &Path) -> Result<Settings> {
        let mut layers = Vec::new();
        for name in &self.profile {
            let file = host.config_dir.join("profiles").join(format!("{name}.libsonnet"));
            if file.exists() {
                layers.push(Layer::File(file));
            } else if jsonnet::BUILTIN_PROFILES.iter().any(|(n, _)| n == name) {
                layers.push(Layer::Builtin(name.clone()));
            } else {
                bail!("no profile named {name} · {} doesn't exist", crate::ui::tilde(&file));
            }
        }
        if let Some(file) = &self.file {
            layers.push(Layer::File(PathBuf::from(cli_path(&file.to_string_lossy(), host, cwd))));
        }
        for (i, setting) in self.config.iter().enumerate() {
            let (key, value) = split_setting(setting)?;
            if let Some(first) = key.first()
                && matches!(first.as_str(), "app" | "task")
            {
                bail!("-c {}: an unnamed sandbox has no {first}s", key.join("."));
            }
            if let [m, name, ..] = key.as_slice()
                && m == "mount"
                && (name.starts_with(BIND_PREFIX) || name.starts_with(RO_BIND_PREFIX))
            {
                bail!(
                    "-c {}: mount names starting with {BIND_PREFIX} or {RO_BIND_PREFIX} belong to --bind and --ro-bind",
                    key.join(".")
                );
            }
            let value = setting_value(&key, value, host, cwd)?;
            layers.push(Layer::Snippet {
                name: format!("<-c #{}>", i + 1),
                text: setting_snippet(&key, &value),
            });
        }
        let mut mounts = serde_json::Map::new();
        for (flag, prefix, mode, specs) in [
            ("bind", BIND_PREFIX, "read-write", &self.bind),
            ("ro-bind", RO_BIND_PREFIX, "read-only", &self.ro_bind),
        ] {
            for (i, spec) in specs.iter().enumerate() {
                let (src, dst) = spec.split_once(':').unwrap_or((spec, spec));
                if src.is_empty() || dst.is_empty() || dst.contains(':') {
                    bail!("--{flag} {spec:?}: expected SRC or SRC:DST");
                }
                mounts.insert(
                    format!("{prefix}{i:03}"),
                    serde_json::json!({
                        "source": cli_path(src, host, cwd),
                        "destination": cli_path(dst, host, cwd),
                        "mode": mode,
                    }),
                );
            }
        }
        if !mounts.is_empty() {
            layers.push(Layer::Snippet {
                name: "<--bind>".into(),
                text: format!("{{ mount+: {} }}", Value::Object(mounts)),
            });
        }

        let evaluation = jsonnet::evaluate(&jsonnet::Input {
            layers,
            name: None,
            home: &host.home,
            root: &host.root,
            forbidden: spec::canonical(std::slice::from_ref(&host.root)),
            before_read: None,
        })
        .map_err(|f| spec::ConfigError {
            name: None,
            file: None,
            problem: f.problem,
        })?;
        let settings = Settings {
            config: evaluation.config,
            app_id: self.app_id.clone(),
        };
        let spec = settings.spec()?;
        jsonnet::check_files(&evaluation.files, &spec::canonical(&spec.writable())).map_err(|e| anyhow!("{e}"))?;
        if let Some(id) = &settings.app_id {
            spec::validate_app_id(id)?;
            match spec.dbus.as_ref().map(|d| d.mode) {
                Some(spec::DbusMode::Portal) => {}
                Some(spec::DbusMode::Host) => {
                    bail!("--app-id needs the private bus; with the host bus the portal sees psbox, not the app")
                }
                None => bail!("--app-id needs a session bus"),
            }
        }
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{DbusMode, DeviceClass, Home, MountMode, NetworkSetting, XwaylandMode, XwaylandScale};

    fn host() -> HostEnv {
        HostEnv {
            uid: 1000,
            gid: 1000,
            user: "u".into(),
            home: "/home/u".into(),
            runtime_dir: "/run/user/1000".into(),
            wayland_socket: None,
            session_bus: None,
            config_dir: "/nonexistent/psbox".into(),
            root: "/home/u/Sandbox".into(),
            pasta: true,
            exe: "/usr/bin/psbox".into(),
        }
    }

    fn spec(o: &Options) -> Result<Spec> {
        o.settings(&host(), Path::new("/work/dir"))?.spec()
    }

    fn with(config: &[&str]) -> Options {
        Options {
            config: config.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn defaults_to_a_temporary_home() {
        let s = spec(&Options::default()).unwrap();
        assert_eq!(s.home, Some(Home::Temporary));
        assert_eq!(s.idle_timeout, 0);
        assert!(s.app.is_empty());
    }

    #[test]
    fn settings_from_config_keys() {
        let s = spec(&with(&[
            "home.mode=path",
            "home.path=../client",
            "devices.camera=true",
            "devices.usb=[\"0483:5740\"]",
            "dbus.mode=host",
            "display=null",
            "environment.A=b=c",
            "environment.N=1.0",
            "environment.Q=\"null\"",
            "network.mode=host",
            "sandbox.32-bit=true",
        ]))
        .unwrap();
        assert!(s.allow_32bit);
        assert_eq!(
            s.home,
            Some(Home::Path {
                path: "/work/client".into(),
                cache: spec::Cache::Temporary
            })
        );
        assert!(s.devices.contains(&DeviceClass::Camera));
        assert!(s.devices.contains(&DeviceClass::Usb {
            vendor: 0x0483,
            product: 0x5740
        }));
        assert_eq!(s.dbus.as_ref().map(|d| d.mode), Some(DbusMode::Host));
        assert!(!s.display && s.xwayland.is_none());
        assert_eq!(s.network, NetworkSetting::Host);
        assert_eq!(s.environment["A"], "b=c");
        assert_eq!(s.environment["N"], "1.0");
        assert_eq!(s.environment["Q"], "null");

        let x = spec(&with(&["xwayland={}", "xwayland.scale=round-up"])).unwrap();
        let xw = x.xwayland.unwrap();
        assert_eq!((xw.mode, xw.scale), (XwaylandMode::Builtin, XwaylandScale::RoundUp));
        assert!(
            spec(&with(&[
                "xwayland.mode=xwayland-satellite",
                "xwayland.scale=compositor"
            ]))
            .is_err()
        );
        let removed = spec(&with(&["environment.A=x", "environment.A=null"])).unwrap();
        assert!(!removed.environment.contains_key("A"));
        // A section set to null comes back with its defaults.
        let back = spec(&with(&["dbus=null", "dbus.mode=portal"])).unwrap();
        assert_eq!(back.dbus.map(|d| d.mode), Some(DbusMode::Portal));
        let quoted = spec(&with(&["mount.\"a.b\".source=/srv", "mount.\"a.b\".mode=read-only"])).unwrap();
        assert_eq!(quoted.mount["a.b"].mode, MountMode::ReadOnly);
    }

    #[test]
    fn binds_and_profiles() {
        let s = spec(&Options {
            bind: vec!["~/Music".into(), "data:/srv/data".into()],
            ro_bind: vec!["/etc/hosts:~/hosts".into()],
            profile: vec!["browser".into()],
            ..Default::default()
        })
        .unwrap();
        assert_eq!(s.mount["bind-000"].source.as_deref(), Some(Path::new("/home/u/Music")));
        assert_eq!(s.mount["bind-001"].source.as_deref(), Some(Path::new("/work/dir/data")));
        assert_eq!(s.mount["bind-001"].destination, Path::new("/srv/data"));
        let hosts = &s.mount["ro-bind-000"];
        assert_eq!(
            (hosts.mode, hosts.destination.as_path()),
            (MountMode::ReadOnly, Path::new("/home/u/hosts"))
        );
        assert!(s.devices.contains(&DeviceClass::Camera), "the browser profile");
        assert!(s.mount.contains_key("downloads"), "the browser profile");
    }

    #[test]
    fn errors() {
        let bad = |o: Options| format!("{:#}", spec(&o).unwrap_err());
        assert!(bad(with(&["app.x.run=[\"x\"]"])).contains("no apps"));
        assert!(bad(with(&["mount.bind-000.source=/x"])).contains("--bind"));
        assert!(bad(with(&["devices.gpu=yes"])).contains("true or false"));
        assert!(bad(with(&["sandbox.idle-timeout=soon"])).contains("whole number"));
        assert!(bad(with(&["network.mode=lan"])).contains("lan"));
        assert!(bad(with(&["nokey"])).contains("KEY=VALUE"));
        assert!(bad(with(&["home.storage=x"])).contains("unknown key"));
        assert!(
            bad(Options {
                profile: vec!["nope".into()],
                ..Default::default()
            })
            .contains("no profile named nope")
        );
        assert!(
            bad(Options {
                app_id: Some("org.example.App".into()),
                config: vec!["dbus=null".into()],
                ..Default::default()
            })
            .contains("needs a session bus")
        );
        for spec in ["/src:/dst:ro", "/etc/hosts:", ":/dst", ""] {
            let o = Options {
                bind: vec![spec.into()],
                ..Default::default()
            };
            assert!(bad(o).contains("SRC:DST"), "{spec:?}");
        }
    }

    #[test]
    fn key_segments() {
        let (k, v) = split_setting("mount.\"a.b\".source=/x=y").unwrap();
        assert_eq!(k, ["mount", "a.b", "source"]);
        assert_eq!(v, "/x=y");
        assert!(split_setting("mount..x=1").is_err());
        assert!(split_setting("\"open=1").is_err());
    }
}
