//! `psbox install`: desktop and autostart entries for every sandbox.
//!
//! Entries go to `$XDG_DATA_HOME/applications` and `$XDG_CONFIG_HOME/autostart`
//! under the same ids as the host's entries, so the sandboxed launcher
//! replaces the host one in menus and at login. Only files carrying the
//! psbox generator marker are ever overwritten or removed, and each names the
//! sandbox it was generated for, so a config that fails to load keeps its
//! entries.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::desktop::{self, Entry, GENERATOR_MARKER, SANDBOX_TAG, Target};
use crate::plan::HostEnv;
use crate::spec::{Spec, Tokens};
use crate::sys::path_cstr;

/// Where entries are read from and written to.
#[derive(Debug, Clone)]
pub struct Dirs {
    pub applications: PathBuf,
    pub autostart: PathBuf,
    /// `$XDG_DATA_DIRS`, searched for host entries.
    pub data_dirs: Vec<PathBuf>,
    /// `$XDG_CONFIG_DIRS`, searched for host autostart entries.
    pub config_dirs: Vec<PathBuf>,
}

impl Dirs {
    pub fn from_env(host: &HostEnv) -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let list = |k: &str, default: &str| -> Vec<PathBuf> {
            var(k)
                .unwrap_or_else(|| default.into())
                .split(':')
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .collect()
        };
        let data_home = var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| host.home.join(".local/share"));
        let config_home = var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| host.home.join(".config"));
        Self {
            applications: data_home.join("applications"),
            autostart: config_home.join("autostart"),
            data_dirs: list("XDG_DATA_DIRS", "/usr/local/share:/usr/share"),
            config_dirs: list("XDG_CONFIG_DIRS", "/etc/xdg"),
        }
    }
}

/// One file to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generated {
    pub path: PathBuf,
    pub body: String,
    pub sandbox: String,
}

/// Every configured entry, and what could not be generated.
#[derive(Debug, Default)]
pub struct Generation {
    pub entries: Vec<Generated>,
    /// Entries that could not be generated, as (sandbox, config entry,
    /// problem).
    pub warnings: Vec<(String, String, String)>,
    /// Paths of entries that could not be built; installed files there are
    /// kept.
    pub failed: Vec<PathBuf>,
}

/// Who generated a file: `None` if psbox did not write it, `Some(None)` for
/// a psbox file without an `@sandbox` line, else the sandbox it names.
fn owner(path: &Path) -> Option<Option<String>> {
    let text = fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    if lines.next() != Some(GENERATOR_MARKER) {
        return None;
    }
    Some(lines.next().and_then(|l| l.strip_prefix(SANDBOX_TAG)).map(String::from))
}

fn is_generated(path: &Path) -> bool {
    owner(path).is_some()
}

/// Finds a host entry `<id>.desktop` under `<dir>/<sub>`, skipping files
/// psbox wrote.
fn find_host(dirs: &[PathBuf], sub: &str, id: &str) -> Option<PathBuf> {
    dirs.iter()
        .map(|d| d.join(sub).join(format!("{id}.desktop")))
        .find(|p| p.is_file() && !is_generated(p))
}

struct Source<'a> {
    from: &'a str,
    entry: Option<&'a str>,
    set: &'a BTreeMap<String, String>,
    remove_args: &'a [String],
}

fn build(
    source: &Source<'_>,
    search: &[(&[PathBuf], &str)],
    target: &Target<'_>,
    tokens: &Tokens<'_>,
) -> Result<Entry> {
    let mut entry = match source.entry {
        Some(text) => Entry::parse(text).context("inline entry")?,
        None => {
            let path = search
                .iter()
                .find_map(|(dirs, sub)| find_host(dirs, sub, source.from))
                .with_context(|| format!("no host entry {}.desktop", source.from))?;
            let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
            Entry::parse(&text).with_context(|| format!("{}", path.display()))?
        }
    };
    desktop::rewrite(&mut entry, target, source.remove_args)?;
    for (k, v) in source.set {
        entry.set(desktop::MAIN, k, v);
    }
    if let Some(icon) = entry
        .get(desktop::MAIN, "Icon")
        .filter(|i| i.starts_with('~') || i.starts_with('@'))
    {
        let expanded = tokens.expand(icon)?;
        entry.set(desktop::MAIN, "Icon", &expanded.to_string_lossy());
    }
    Ok(entry)
}

/// Generates every configured entry. Entries are independent: one that
/// cannot be built is reported and skipped.
pub fn generate(sandboxes: &[(String, Spec)], host: &HostEnv, dirs: &Dirs, psbox: &Path) -> Generation {
    let mut out: Vec<Generated> = Vec::new();
    let mut warnings = Vec::new();
    let mut failed = Vec::new();
    let apps: &[(&[PathBuf], &str)] = &[(&dirs.data_dirs, "applications")];
    let autostart: &[(&[PathBuf], &str)] = &[(&dirs.config_dirs, "autostart"), (&dirs.data_dirs, "applications")];
    for (name, spec) in sandboxes {
        let tokens = Tokens {
            home: &host.home,
            root: &host.root,
            name,
        };
        let mut push = |path: PathBuf, r: Result<Entry>, what: String| match r {
            Ok(entry) => {
                if let Some(prev) = out.iter().find(|g| g.path == path) {
                    warnings.push((
                        name.clone(),
                        what,
                        format!("{} is already generated for {}", crate::ui::tilde(&path), prev.sandbox),
                    ));
                } else {
                    out.push(Generated {
                        path,
                        body: entry.render(name),
                        sandbox: name.clone(),
                    });
                }
            }
            Err(e) => {
                warnings.push((name.clone(), what, format!("{e:#}")));
                failed.push(path);
            }
        };
        for (cmd_name, cmd) in &spec.command {
            let Some(d) = &cmd.desktop else { continue };
            let id = desktop_id(cmd_name, cmd.app_id.as_deref(), d.id.as_deref());
            let source = Source {
                from: d.from.as_deref().unwrap_or(&id),
                entry: d.entry.as_deref(),
                set: &d.set,
                remove_args: &d.remove_args,
            };
            let target = Target {
                psbox,
                sandbox: name,
                command: cmd_name,
            };
            let path = dirs.applications.join(format!("{id}.desktop"));
            push(
                path,
                build(&source, apps, &target, &tokens),
                format!("[command.{cmd_name}]"),
            );
        }
        for (id, a) in &spec.autostart {
            let path = dirs.autostart.join(format!("{id}.desktop"));
            let what = format!("[autostart.{id}]");
            if a.hidden {
                push(path, Ok(desktop::hidden(id)), what);
                continue;
            }
            let command = a.command.clone().unwrap_or_else(|| name.clone());
            let source = Source {
                from: a.from.as_deref().unwrap_or(id),
                entry: a.entry.as_deref(),
                set: &a.set,
                remove_args: &a.remove_args,
            };
            let target = Target {
                psbox,
                sandbox: name,
                command: &command,
            };
            push(path, build(&source, autostart, &target, &tokens), what);
        }
    }
    Generation {
        entries: out,
        warnings,
        failed,
    }
}

/// The desktop file id of a command's entry.
pub fn desktop_id(command: &str, app_id: Option<&str>, id: Option<&str>) -> String {
    id.or(app_id).unwrap_or(command).to_string()
}

/// Replaces the file at `path` with `body`. The content goes to a new,
/// uniquely named file beside it, which is then renamed over it, both
/// relative to the directory opened once. `O_EXCL` refuses an existing file
/// or symlink by that name, so nothing placed in the directory (which a
/// sandbox sharing it could write) can redirect the write elsewhere.
fn replace(path: &Path, body: &str) -> io::Result<()> {
    let dir = fs::File::open(path.parent().expect("entry paths have a parent"))?;
    let file_name = path.file_name().expect("entry paths have a file name");
    let name = path_cstr(Path::new(file_name))?;
    let mut attempt = 0;
    let (tmp, mut file) = loop {
        let mut tmp = OsString::from(".");
        tmp.push(file_name);
        tmp.push(format!(".{}-{attempt}.psbox-tmp", std::process::id()));
        let tmp = path_cstr(Path::new(&tmp))?;
        // SAFETY: a valid directory descriptor and C string.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                tmp.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o644,
            )
        };
        if fd >= 0 {
            // SAFETY: openat returned a new descriptor we now own.
            break (tmp, unsafe { fs::File::from_raw_fd(fd) });
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::AlreadyExists || attempt >= 100 {
            return Err(e);
        }
        attempt += 1;
    };
    let written = file.write_all(body.as_bytes()).and_then(|()| {
        // SAFETY: a valid directory descriptor and C strings.
        let r = unsafe { libc::renameat(dir.as_raw_fd(), tmp.as_ptr(), dir.as_raw_fd(), name.as_ptr()) };
        if r < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    });
    if written.is_err() {
        // SAFETY: a valid directory descriptor and C string.
        unsafe { libc::unlinkat(dir.as_raw_fd(), tmp.as_ptr(), 0) };
    }
    written
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Entries written where there was no file.
    pub added: Vec<PathBuf>,
    /// psbox files rewritten with new content.
    pub updated: Vec<PathBuf>,
    pub unchanged: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    /// Files in the way that psbox did not write.
    pub skipped: Vec<PathBuf>,
}

/// Writes generated entries and removes psbox-written entries that are no
/// longer configured. Entries of `failed_sandboxes` (configs that did not
/// load) and entries that could not be built are kept; psbox files that name
/// no sandbox are removed. With `dry_run`, only reports.
pub fn apply(generation: &Generation, failed_sandboxes: &[String], dirs: &Dirs, dry_run: bool) -> Result<Report> {
    let mut report = Report::default();
    for g in &generation.entries {
        // Only a missing file is free to write; one that cannot be read may
        // be someone else's, so it is left alone like a file psbox did not
        // write.
        let existing = match fs::read_to_string(&g.path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => {
                report.skipped.push(g.path.clone());
                continue;
            }
        };
        match &existing {
            Some(text) if *text == g.body => report.unchanged.push(g.path.clone()),
            Some(text) if text.lines().next() != Some(GENERATOR_MARKER) => report.skipped.push(g.path.clone()),
            _ => {
                if !dry_run {
                    let dir = g.path.parent().expect("entry paths have a parent");
                    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
                    replace(&g.path, &g.body).with_context(|| format!("write {}", g.path.display()))?;
                }
                match existing {
                    Some(_) => report.updated.push(g.path.clone()),
                    None => report.added.push(g.path.clone()),
                }
            }
        }
    }
    for dir in [&dirs.applications, &dirs.autostart] {
        let Ok(entries) = fs::read_dir(dir) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "desktop")
                || generation.entries.iter().any(|g| g.path == path)
                || generation.failed.contains(&path)
            {
                continue;
            }
            let stale = match owner(&path) {
                Some(Some(sandbox)) => !failed_sandboxes.contains(&sandbox),
                Some(None) => true,
                None => false,
            };
            if stale {
                if !dry_run {
                    fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
                }
                report.removed.push(path);
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("psbox-install-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn replace_does_not_follow_a_planted_link() {
        let d = tempdir("replace");
        let victim = d.join("victim");
        fs::write(&victim, "keep").unwrap();
        let entry = d.join("app.desktop");
        let planted = d.join(format!(".app.desktop.{}-0.psbox-tmp", std::process::id()));
        std::os::unix::fs::symlink(&victim, &planted).unwrap();

        replace(&entry, "[Desktop Entry]\n").unwrap();
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(fs::read_to_string(&entry).unwrap(), "[Desktop Entry]\n");
        assert!(fs::symlink_metadata(&planted).unwrap().file_type().is_symlink());
        let _ = fs::remove_dir_all(&d);
    }

    fn setup(tag: &str) -> (PathBuf, Dirs, HostEnv) {
        let root = tempdir(tag);
        let dirs = Dirs {
            applications: root.join("home/.local/share/applications"),
            autostart: root.join("home/.config/autostart"),
            data_dirs: vec![root.join("usr/share")],
            config_dirs: vec![root.join("etc/xdg")],
        };
        fs::create_dir_all(root.join("usr/share/applications")).unwrap();
        fs::create_dir_all(root.join("etc/xdg/autostart")).unwrap();
        fs::write(
            root.join("usr/share/applications/code-oss.desktop"),
            "[Desktop Entry]\nName=Code\nExec=code --new-window %F\nTryExec=code\nIcon=code-oss\n",
        )
        .unwrap();
        fs::write(
            root.join("etc/xdg/autostart/steam-autostart.desktop"),
            "[Desktop Entry]\nName=Steam\nExec=steam -silent\n",
        )
        .unwrap();
        let host = HostEnv {
            uid: 1000,
            gid: 1000,
            user: "u".into(),
            home: "/home/u".into(),
            runtime_dir: "/run/user/1000".into(),
            wayland_socket: None,
            session_bus: None,
            config_dir: root.join("config"),
            root: "/home/u/Sandbox".into(),
            pasta: true,
        };
        (root, dirs, host)
    }

    fn specs() -> Vec<(String, Spec)> {
        let code = Spec::parse(
            r#"
            [command.code]
            exec = ["code"]
            desktop = { id = "code-oss", remove-args = ["--new-window"], set = { StartupWMClass = "code-oss" } }

            [command.insiders]
            exec = ["~/.bin/code-insiders"]
            desktop.entry = """
            [Desktop Entry]
            Name=Code Insiders
            Exec=launch %u
            Icon=@self.bin/icon.png
            Type=Application
            """
            "#,
        )
        .unwrap();
        let steam = Spec::parse(
            r#"
            [command.steam]
            exec = ["steam"]

            [autostart.steam-autostart]
            hidden = true
            "#,
        )
        .unwrap();
        vec![("code".into(), code), ("steam".into(), steam)]
    }

    #[test]
    fn generates_and_installs_entries() {
        let (root, dirs, host) = setup("gen");
        let generation = generate(&specs(), &host, &dirs, Path::new("/usr/bin/psbox"));
        assert!(generation.warnings.is_empty(), "{:?}", generation.warnings);
        let gen_ = &generation.entries;
        assert_eq!(gen_.len(), 3);

        let code = gen_.iter().find(|g| g.path.ends_with("code-oss.desktop")).unwrap();
        let e = Entry::parse(&code.body).unwrap();
        assert_eq!(
            e.get(desktop::MAIN, "Exec"),
            Some("/usr/bin/psbox run code --command code -- %F")
        );
        assert_eq!(e.get(desktop::MAIN, "TryExec"), None);
        assert_eq!(e.get(desktop::MAIN, "StartupWMClass"), Some("code-oss"));
        assert_eq!(e.get(desktop::MAIN, "DBusActivatable"), Some("false"));

        let insiders = gen_.iter().find(|g| g.path.ends_with("insiders.desktop")).unwrap();
        let e = Entry::parse(&insiders.body).unwrap();
        assert_eq!(e.get(desktop::MAIN, "Icon"), Some("/home/u/Sandbox/code.bin/icon.png"));
        assert_eq!(
            e.get(desktop::MAIN, "Exec"),
            Some("/usr/bin/psbox run code --command insiders -- %u")
        );

        let hidden = gen_
            .iter()
            .find(|g| g.path.ends_with("autostart/steam-autostart.desktop"))
            .unwrap();
        assert!(hidden.body.contains("Hidden=true"));

        // A file psbox did not write is never replaced.
        fs::create_dir_all(&dirs.applications).unwrap();
        fs::write(
            dirs.applications.join("insiders.desktop"),
            "[Desktop Entry]\nName=Mine\n",
        )
        .unwrap();
        // Stale psbox files are removed: one from a deleted sandbox and one
        // naming no sandbox. A file of a sandbox whose config failed to load
        // is kept, as is a foreign one.
        let psbox_file = |name: &str, tag: Option<&str>| {
            let tag = tag.map(|t| format!("{SANDBOX_TAG}{t}\n")).unwrap_or_default();
            fs::write(
                dirs.applications.join(name),
                format!("{GENERATOR_MARKER}\n{tag}[Desktop Entry]\n"),
            )
            .unwrap();
        };
        psbox_file("gone.desktop", Some("deleted"));
        psbox_file("untagged.desktop", None);
        psbox_file("broken.desktop", Some("broken"));
        fs::write(
            dirs.applications.join("other.desktop"),
            "# @generator other\n[Desktop Entry]\n",
        )
        .unwrap();

        // A file that cannot be read as text is someone else's too.
        let unreadable = dirs.autostart.join("steam-autostart.desktop");
        fs::create_dir_all(&dirs.autostart).unwrap();
        fs::write(&unreadable, b"\xff\xfe not text").unwrap();

        let failed = vec!["broken".to_string()];

        let dry = apply(&generation, &failed, &dirs, true).unwrap();
        assert_eq!(dry.added.len(), 1);
        assert!(!dirs.applications.join("code-oss.desktop").exists());

        let r = apply(&generation, &failed, &dirs, false).unwrap();
        assert_eq!(r.added.len(), 1);
        assert_eq!(
            r.skipped,
            vec![dirs.applications.join("insiders.desktop"), unreadable.clone()]
        );
        assert_eq!(fs::read(&unreadable).unwrap(), b"\xff\xfe not text");
        fs::remove_file(&unreadable).unwrap();
        apply(&generation, &failed, &dirs, false).unwrap();
        let mut removed = r.removed.clone();
        removed.sort();
        assert_eq!(
            removed,
            vec![
                dirs.applications.join("gone.desktop"),
                dirs.applications.join("untagged.desktop")
            ]
        );
        assert!(dirs.applications.join("broken.desktop").exists());
        assert!(dirs.applications.join("other.desktop").exists());
        assert!(dirs.applications.join("code-oss.desktop").exists());

        let again = apply(&generation, &failed, &dirs, false).unwrap();
        assert!(again.added.is_empty() && again.updated.is_empty() && again.removed.is_empty());
        assert_eq!(again.unchanged.len(), 2);

        // An entry that could not be built keeps its installed file.
        let partial = Generation {
            entries: generation.entries[1..].to_vec(),
            failed: vec![generation.entries[0].path.clone()],
            ..Default::default()
        };
        let kept = apply(&partial, &[], &dirs, false).unwrap();
        assert!(!kept.removed.contains(&generation.entries[0].path));
        assert!(kept.removed.contains(&dirs.applications.join("broken.desktop")));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reports_missing_host_entries() {
        let (root, dirs, host) = setup("missing");
        let spec = Spec::parse("[command.x]\nexec = [\"x\"]\ndesktop = {}\n").unwrap();
        let generation = generate(&[("x".into(), spec)], &host, &dirs, Path::new("/usr/bin/psbox"));
        assert!(generation.entries.is_empty());
        assert!(
            generation.warnings[0].2.contains("no host entry x.desktop"),
            "{:?}",
            generation.warnings
        );
        assert_eq!(generation.failed, vec![dirs.applications.join("x.desktop")]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn autostart_commands_use_the_autostart_entry() {
        let (root, dirs, host) = setup("autostart");
        let spec = Spec::parse(
            "[command.steam]\nexec = [\"steam\"]\n[autostart.steam-autostart]\ncommand = \"steam\"\nremove-args = [\"-silent\"]\n",
        )
        .unwrap();
        let generation = generate(&[("steam".into(), spec)], &host, &dirs, Path::new("/usr/bin/psbox"));
        assert!(generation.warnings.is_empty(), "{:?}", generation.warnings);
        let e = Entry::parse(&generation.entries[0].body).unwrap();
        assert_eq!(
            e.get(desktop::MAIN, "Exec"),
            Some("/usr/bin/psbox run steam --command steam --")
        );
        fs::remove_dir_all(&root).unwrap();
    }
}
