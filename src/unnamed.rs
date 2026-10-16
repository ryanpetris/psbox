//! `psbox sandbox`: options for an unnamed, oneshot sandbox and the settings
//! file its supervisor reads in place of a named config.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::plan::HostEnv;
use crate::spec::{Spec, Tokens};

/// Exactly one way to provide the home.
#[derive(clap::Args, Debug, Default)]
#[group(required = true, multiple = false)]
pub struct HomeArgs {
    /// Bind this directory read-write as the home.
    #[arg(long, value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
    home: Option<String>,
    /// An empty in-memory home.
    #[arg(long)]
    home_tmpfs: bool,
    /// No home.
    #[arg(long)]
    no_home: bool,
}

#[derive(clap::Args, Debug, Default)]
pub struct Options {
    #[command(flatten)]
    home: HomeArgs,
    /// `tmpfs` (default) hides `~/.cache`; `home` keeps it.
    #[arg(long, value_name = "MODE", value_parser = ["tmpfs", "home"])]
    cache: Option<String>,
    /// Comma-separated device classes, replacing `gpu,audio`; `none` for no
    /// devices.
    #[arg(long, value_name = "LIST")]
    devices: Option<String>,
    /// `portal` (default): a private bus with portals; `host`: the host
    /// buses; `off`: no bus.
    #[arg(long, value_name = "MODE", value_parser = ["portal", "host", "off"])]
    dbus: Option<String>,
    /// A host bus name the sandbox may call through the gateway (with the
    /// `portal` bus); repeat for more.
    #[arg(long, value_name = "NAME")]
    dbus_talk: Vec<String>,
    /// `isolated` (outbound only, through pasta; the default when pasta is
    /// installed, otherwise `host`), `host`, or `none`.
    #[arg(long, value_name = "MODE", value_parser = ["host", "isolated", "none"])]
    network: Option<String>,
    /// No Wayland and no Xwayland.
    #[arg(long, conflicts_with = "xwayland")]
    no_display: bool,
    /// Serve X11 apps with a private Xwayland.
    #[arg(long)]
    xwayland: bool,
    /// Share a host path read-write, at the same path or at DST.
    #[arg(long, value_name = "SRC[:DST]", value_hint = clap::ValueHint::AnyPath)]
    bind: Vec<String>,
    /// Share a host path read-only, at the same path or at DST.
    #[arg(long, value_name = "SRC[:DST]", value_hint = clap::ValueHint::AnyPath)]
    ro_bind: Vec<String>,
    /// Set an environment variable.
    #[arg(long, value_name = "K=V")]
    env: Vec<String>,
}

impl Options {
    /// The settings file body. Paths are made absolute here, relative ones
    /// against `cwd`, so the supervisor needs no context from the caller.
    pub fn settings(&self, host: &HostEnv, cwd: &Path) -> Result<String> {
        let path = |p: &str| -> Result<String> {
            if p.starts_with("@self") {
                bail!("{p:?}: @self needs a named sandbox");
            }
            let raw = if p.starts_with('~') || p.starts_with('@') {
                let tokens = Tokens {
                    home: &host.home,
                    root: &host.root,
                    name: "",
                };
                tokens.expand(p)?
            } else {
                cwd.join(p)
            };
            // A trailing `/` marks a directory; keep it through normalizing.
            let slash = if p.ends_with('/') && p.len() > 1 { "/" } else { "" };
            Ok(format!("{}{slash}", normalize(&raw).to_string_lossy()))
        };

        let mut t = toml::Table::new();
        let home = match (&self.home.home, self.home.home_tmpfs, self.home.no_home) {
            (Some(dir), _, _) => path(dir)?,
            (None, true, _) => "tmpfs".into(),
            (None, false, true) => "none".into(),
            (None, false, false) => bail!("choose --home, --home-tmpfs, or --no-home"),
        };
        t.insert("home".into(), home.into());
        for (key, value) in [("cache", &self.cache), ("dbus", &self.dbus), ("network", &self.network)] {
            if let Some(v) = value {
                t.insert(key.into(), v.clone().into());
            }
        }
        if !self.dbus_talk.is_empty() {
            let names: Vec<toml::Value> = self.dbus_talk.iter().map(|n| n.clone().into()).collect();
            t.insert("dbus-talk".into(), names.into());
        }
        if let Some(list) = &self.devices {
            let classes: Vec<toml::Value> = match list.trim() {
                "none" => Vec::new(),
                l => l.split(',').map(|c| c.trim().to_string().into()).collect(),
            };
            t.insert("devices".into(), classes.into());
        }
        t.insert("display".into(), (!self.no_display).into());
        t.insert("xwayland".into(), self.xwayland.into());

        let mut env = toml::Table::new();
        for kv in &self.env {
            let (k, v) = kv
                .split_once('=')
                .with_context(|| format!("--env {kv:?}: expected K=V"))?;
            env.insert(k.into(), v.into());
        }
        t.insert("env".into(), env.into());

        let mut mounts = toml::Table::new();
        for (flag, mode, specs) in [("bind", "rw", &self.bind), ("ro-bind", "ro", &self.ro_bind)] {
            for (i, spec) in specs.iter().enumerate() {
                let (src, dst) = spec.split_once(':').unwrap_or((spec, spec));
                if src.is_empty() || dst.is_empty() || dst.contains(':') {
                    bail!("--{flag} {spec:?}: expected SRC or SRC:DST");
                }
                let (src, dst) = (path(src)?, path(dst)?);
                let mut m = toml::Table::new();
                m.insert("host".into(), src.into());
                m.insert("at".into(), dst.into());
                m.insert("mode".into(), mode.into());
                mounts.insert(format!("{flag}-{i:03}"), m.into());
            }
        }
        t.insert("mount".into(), mounts.into());
        // The supervisor stops as soon as the command has exited.
        t.insert("idle-timeout".into(), 0.into());

        let text = toml::to_string(&t)?;
        Spec::parse(&text)?;
        Ok(text)
    }
}

/// Resolves `.` and `..` without touching the filesystem, so symlinks in a
/// share's path stay where the user named them.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{Dbus, Home, MountMode, Network};

    fn host() -> HostEnv {
        HostEnv {
            uid: 1000,
            gid: 1000,
            user: "u".into(),
            home: "/home/u".into(),
            runtime_dir: "/run/user/1000".into(),
            wayland_socket: None,
            session_bus: None,
            config_dir: "/home/u/.config/psbox".into(),
            root: "/home/u/Sandbox".into(),
            pasta: true,
        }
    }

    fn spec(o: &Options) -> Result<Spec> {
        Spec::parse(&o.settings(&host(), Path::new("/work/dir"))?)
    }

    #[test]
    fn builds_settings_from_flags() {
        let o = Options {
            home: HomeArgs {
                home: Some("../client".into()),
                ..Default::default()
            },
            devices: Some("gpu, camera".into()),
            dbus: Some("host".into()),
            dbus_talk: vec![],
            no_display: true,
            bind: vec!["~/Downloads".into(), "data:/srv/data".into()],
            ro_bind: vec!["/etc/hosts:~/hosts".into()],
            env: vec!["A=b=c".into()],
            ..Default::default()
        };
        let s = spec(&o).unwrap();
        assert_eq!(s.home, Home::Path("/work/client".into()));
        assert_eq!(s.device_classes().len(), 2);
        assert_eq!(s.dbus, Dbus::Host);
        assert!(!s.display && !s.xwayland && s.network.is_none());
        let x = Options {
            home: HomeArgs {
                home_tmpfs: true,
                ..Default::default()
            },
            xwayland: true,
            network: Some("host".into()),
            ..Default::default()
        };
        let xs = spec(&Options {
            bind: vec!["~/games/".into()],
            ..x
        })
        .unwrap();
        assert_eq!(xs.mount["bind-000"].host.as_deref(), Some("/home/u/games/"));
        let talk = spec(&Options {
            home: HomeArgs {
                home_tmpfs: true,
                ..Default::default()
            },
            dbus_talk: vec!["org.a.b".into(), "org.c.d".into()],
            ..Default::default()
        })
        .unwrap();
        assert_eq!(talk.dbus_talk, ["org.a.b", "org.c.d"]);
        assert!(xs.display && xs.xwayland && xs.network == Some(Network::Host));
        assert_eq!(s.env["A"], "b=c");
        assert_eq!(s.idle_timeout, 0);
        let downloads = &s.mount["bind-000"];
        assert_eq!(downloads.host.as_deref(), Some("/home/u/Downloads"));
        assert_eq!(downloads.at.as_deref(), Some("/home/u/Downloads"));
        assert_eq!(s.mount["bind-001"].host.as_deref(), Some("/work/dir/data"));
        assert_eq!(s.mount["bind-001"].at.as_deref(), Some("/srv/data"));
        let hosts = &s.mount["ro-bind-000"];
        assert_eq!(
            (hosts.mode, hosts.at.as_deref()),
            (MountMode::Ro, Some("/home/u/hosts"))
        );
    }

    #[test]
    fn home_choices_and_errors() {
        let tmpfs = Options {
            home: HomeArgs {
                home_tmpfs: true,
                ..Default::default()
            },
            devices: Some("none".into()),
            ..Default::default()
        };
        let s = spec(&tmpfs).unwrap();
        assert_eq!(s.home, Home::Tmpfs);
        assert!(s.device_classes().is_empty());

        let none = Options {
            home: HomeArgs {
                no_home: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(spec(&none).unwrap().home, Home::None);

        let bad = |o: Options| spec(&o).unwrap_err().to_string();
        assert!(bad(Options::default()).contains("--home"));
        let mut o = Options {
            home: HomeArgs {
                home: Some("@self".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(bad(o).contains("@self"));
        o = Options {
            home: HomeArgs {
                home_tmpfs: true,
                ..Default::default()
            },
            network: Some("lan".into()),
            ..Default::default()
        };
        assert!(bad(o).contains("lan"));
        for spec in ["/src:/dst:ro", "/etc/hosts:", ":/dst", ""] {
            let o = Options {
                home: HomeArgs {
                    home_tmpfs: true,
                    ..Default::default()
                },
                bind: vec![spec.into()],
                ..Default::default()
            };
            assert!(bad(o).contains("SRC:DST"), "{spec:?}");
        }
    }
}
