//! `psbox install`: desktop and autostart entries for every sandbox.
//!
//! Each `app` entry of a config is one file: an application in
//! `$XDG_DATA_HOME/applications`, an autostart entry in
//! `$XDG_CONFIG_HOME/autostart`, under its id, so the sandboxed entry
//! replaces a host one with the same id in menus and at login. Only files
//! carrying the psbox generator marker are ever overwritten or removed, and
//! each names the sandbox it was generated for, so a config that fails to
//! load keeps its entries. When sandboxes claim the same file, the one whose
//! name sorts first gets it.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::desktop::{self, ACTION_PREFIX, Entry, GENERATOR_MARKER, SANDBOX_TAG, Target};
use crate::plan::HostEnv;
use crate::spec::{Action, AppSpec, KeyValue, Kind, Spec};
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
    /// The directories in this process's environment.
    pub fn for_host(host: &HostEnv) -> Self {
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
    /// Things worth knowing that leave nothing out, as (sandbox, config
    /// entry, note).
    pub notes: Vec<(String, String, String)>,
    /// Paths of entries that could not be built, or that another sandbox
    /// claims; installed files there are kept.
    pub failed: Vec<PathBuf>,
}

/// Who generated a file: `None` if psbox did not write it, `Some(None)` for
/// a psbox file without an `@sandbox` line, else the sandbox it names.
pub fn owner(path: &Path) -> Option<Option<String>> {
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

/// The relative paths GIO looks up a desktop file id at: `<id>.desktop`,
/// and with one `-` taken as a directory (`kde4-foo` in `kde4/foo.desktop`).
fn id_paths(id: &str) -> Vec<PathBuf> {
    let mut out = vec![PathBuf::from(format!("{id}.desktop"))];
    for (i, _) in id.match_indices('-') {
        out.push(Path::new(&id[..i]).join(format!("{}.desktop", &id[i + 1..])));
    }
    out
}

/// Finds a host entry for `id` under `<dir>/<sub>`, skipping files psbox
/// wrote.
fn find_host(dirs: &[PathBuf], sub: &str, id: &str) -> Option<PathBuf> {
    dirs.iter()
        .flat_map(|d| id_paths(id).into_iter().map(move |p| d.join(sub).join(p)))
        .find(|p| p.is_file() && !is_generated(p))
}

/// Whether the host has an application entry `id` that psbox did not
/// write, for the implicit default app to inherit.
pub fn host_entry_exists(id: &str, host: &HostEnv) -> bool {
    let dirs = Dirs::for_host(host);
    let data_home = dirs.applications.parent().map(Path::to_path_buf).into_iter();
    let search: Vec<PathBuf> = data_home.chain(dirs.data_dirs.iter().cloned()).collect();
    find_host(&search, "applications", id).is_some()
}

/// Applies Desktop Entry keys from a config to `section`.
fn apply_keys(entry: &mut Entry, section: &str, keys: &BTreeMap<String, KeyValue>) -> Result<()> {
    for (key, value) in keys {
        match value {
            KeyValue::Remove => entry.remove_all(section, key),
            KeyValue::Bool(b) => entry.set(section, key, if *b { "true" } else { "false" }),
            KeyValue::Str(s) => {
                entry.remove_all(section, key);
                entry.set(
                    section,
                    key,
                    &desktop::escape(s).with_context(|| format!("keys.{key}"))?,
                );
            }
            KeyValue::List(items) => entry.set(
                section,
                key,
                &desktop::escape_list(items).with_context(|| format!("keys.{key}"))?,
            ),
            KeyValue::Localized { default, locales } => {
                entry.remove_all(section, key);
                entry.set(
                    section,
                    key,
                    &desktop::escape(default).with_context(|| format!("keys.{key}"))?,
                );
                for (locale, text) in locales {
                    let escaped = desktop::escape(text).with_context(|| format!("keys.{key}.{locale}"))?;
                    entry.set(section, &format!("{key}[{locale}]"), &escaped);
                }
            }
        }
    }
    Ok(())
}

/// Where host entries of one kind are found: each set of data directories
/// with the subdirectory to look in.
type Search<'a> = [(&'a [PathBuf], &'a str)];

/// What an entry is built from.
struct Source<'a> {
    /// The host entry to copy, and where to look for it.
    copy: Option<(&'a str, &'a Search<'a>)>,
    /// The name of the app entry, `Name` when written from scratch.
    name: &'a str,
    keys: &'a BTreeMap<String, KeyValue>,
    actions: Option<&'a BTreeMap<String, Option<Action>>>,
    remove_arguments: &'a [String],
    no_display: Option<bool>,
}

fn build(source: &Source<'_>, target: &Target<'_>) -> Result<Entry> {
    let mut entry = match source.copy {
        Some((id, search)) => {
            let path = search
                .iter()
                .find_map(|(dirs, sub)| find_host(dirs, sub, id))
                .with_context(|| format!("no host entry {id}.desktop"))?;
            let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
            let mut entry = Entry::parse(&text).with_context(|| format!("{}", path.display()))?;
            desktop::rewrite(&mut entry, target, source.remove_arguments)?;
            entry
        }
        None => {
            let mut entry = Entry::new();
            entry.set(desktop::MAIN, "Name", &desktop::escape(source.name)?);
            entry
        }
    };
    apply_keys(&mut entry, desktop::MAIN, source.keys)?;
    for (name, action) in source.actions.into_iter().flatten() {
        let section = format!("{ACTION_PREFIX}{name}");
        match action {
            None => entry.remove_section(&section),
            Some(a) => {
                entry.add_section(&section);
                if !a.keys.contains_key("Name") && entry.get(&section, "Name").is_none() {
                    entry.set(&section, "Name", &desktop::escape(name)?);
                }
                apply_keys(&mut entry, &section, &a.keys)?;
                entry.set(&section, "Exec", &target.exec(&a.arguments));
            }
        }
    }
    match source.no_display {
        Some(true) => entry.set(desktop::MAIN, "NoDisplay", "true"),
        Some(false) => entry.remove(desktop::MAIN, "NoDisplay"),
        None => {}
    }
    desktop::finish(&mut entry, target);
    Ok(entry)
}

/// Generates every configured entry, from sandboxes in name order: when two
/// claim one file, the first gets it. Entries are independent: one that
/// cannot be built is reported and skipped.
pub fn generate(sandboxes: &[(String, Spec)], dirs: &Dirs, psbox: &Path) -> Generation {
    let mut out: Vec<Generated> = Vec::new();
    let mut warnings = Vec::new();
    let mut notes = Vec::new();
    let mut failed = Vec::new();
    let data_home: Vec<PathBuf> = dirs.applications.parent().map(Path::to_path_buf).into_iter().collect();
    let config_home: Vec<PathBuf> = dirs.autostart.parent().map(Path::to_path_buf).into_iter().collect();
    let apps: &Search = &[(&data_home, "applications"), (&dirs.data_dirs, "applications")];
    let autostart: &Search = &[
        (&config_home, "autostart"),
        (&dirs.config_dirs, "autostart"),
        (&data_home, "applications"),
        (&dirs.data_dirs, "applications"),
    ];
    let mut sorted: Vec<&(String, Spec)> = sandboxes.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, spec) in sorted {
        let mut push = |path: PathBuf, r: Result<Entry>, what: String| match r {
            Ok(entry) => {
                if let Some(prev) = out.iter().find(|g| g.path == path) {
                    warnings.push((
                        name.clone(),
                        what,
                        format!(
                            "{} also belongs to {}, whose name sorts first",
                            crate::ui::tilde(&path),
                            prev.sandbox
                        ),
                    ));
                    failed.push(path);
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
        for (app_name, app) in &spec.app {
            let what = format!("app.{app_name}");
            let dir = match app.kind() {
                Kind::Application => &dirs.applications,
                Kind::Autostart => &dirs.autostart,
            };
            let path = dir.join(format!("{}.desktop", app.id()));
            match app {
                AppSpec::Suppress { id, .. } => push(path, Ok(desktop::hidden(id)), what),
                AppSpec::Application(a) if !a.has_entry() => notes.push((
                    name.clone(),
                    what,
                    format!(
                        "no {app_name}.desktop to inherit, so the default app has no menu entry or portal identity; define app.{app_name} or set it to null"
                    ),
                )),
                AppSpec::Application(a) => {
                    let target = Target {
                        psbox,
                        sandbox: name,
                        app: app_name,
                    };
                    let source = Source {
                        copy: a.source_id.as_deref().map(|id| (id, apps)),
                        name: app_name,
                        keys: &a.keys,
                        actions: Some(&a.actions),
                        remove_arguments: &a.remove_arguments,
                        no_display: a.no_display,
                    };
                    push(path, build(&source, &target), what);
                }
                AppSpec::Autostart(s) => {
                    let target = Target {
                        psbox,
                        sandbox: name,
                        app: &s.app,
                    };
                    let source = Source {
                        copy: s.source_id.as_deref().map(|id| (id, autostart)),
                        name: app_name,
                        keys: &s.keys,
                        actions: None,
                        remove_arguments: &s.remove_arguments,
                        no_display: None,
                    };
                    push(path, build(&source, &target), what);
                }
            }
        }
    }
    Generation {
        entries: out,
        warnings,
        notes,
        failed,
    }
}

/// The sandbox, other than `sandbox`, whose installed application entry
/// has the portal identity `id`.
pub fn claimant(id: &str, sandbox: &str, dirs: &Dirs) -> Option<String> {
    owner(&dirs.applications.join(format!("{id}.desktop")))
        .flatten()
        .filter(|s| s != sandbox)
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

    fn setup(tag: &str) -> (PathBuf, Dirs) {
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
            "[Desktop Entry]\nName=Code\nExec=code --new-window %F\nTryExec=code\nIcon=code-oss\n\
             Actions=new-empty-window;\n\n[Desktop Action new-empty-window]\nName=New Empty Window\nExec=code --new-window %F\n",
        )
        .unwrap();
        fs::write(
            root.join("etc/xdg/autostart/steam-autostart.desktop"),
            "[Desktop Entry]\nName=Steam\nExec=steam -silent\n",
        )
        .unwrap();
        (root, dirs)
    }

    fn spec(name: &str, text: &str, host_entries: &[&str]) -> Spec {
        crate::spec::parse_named(name, &format!("local psbox = import 'psbox';\n{text}"), host_entries)
            .unwrap_or_else(|e| panic!("{e:#}"))
    }

    fn specs() -> Vec<(String, Spec)> {
        let code = spec(
            "code",
            "{ app+: {
                 code: { run: ['code'], id: 'code-oss', 'source-id': 'code-oss',
                         'remove-arguments': ['--new-window'], keys: { StartupWMClass: 'code-oss' } },
                 insiders: { run: [psbox.path([psbox.home, '.bin', 'code-insiders'])],
                             keys: { Name: 'Code Insiders', Icon: psbox.storageFor('bin') + '/icon.png',
                                     MimeType: ['x-scheme-handler/vscode'] } },
               } }",
            &[],
        );
        let steam = spec(
            "steam",
            "{ app+: { 'steam-autostart': { kind: 'autostart', suppress: true } } }",
            &[],
        );
        vec![("code".into(), code), ("steam".into(), steam)]
    }

    fn psbox() -> &'static Path {
        Path::new("/usr/bin/psbox")
    }

    #[test]
    fn generates_and_installs_entries() {
        let (root, dirs) = setup("gen");
        let generation = generate(&specs(), &dirs, psbox());
        assert!(generation.warnings.is_empty(), "{:?}", generation.warnings);
        // steam's implicit default app has no host entry to inherit.
        assert_eq!(generation.notes.len(), 1);
        assert_eq!(
            (generation.notes[0].0.as_str(), generation.notes[0].1.as_str()),
            ("steam", "app.steam")
        );
        let gen_ = &generation.entries;
        assert_eq!(gen_.len(), 3);

        let code = gen_.iter().find(|g| g.path.ends_with("code-oss.desktop")).unwrap();
        let e = Entry::parse(&code.body).unwrap();
        assert_eq!(
            e.get(desktop::MAIN, "Exec"),
            Some("/usr/bin/psbox run code --app code -- %F")
        );
        assert_eq!(e.get(desktop::MAIN, "TryExec"), None);
        assert_eq!(e.get(desktop::MAIN, "StartupWMClass"), Some("code-oss"));
        assert_eq!(e.get(desktop::MAIN, "DBusActivatable"), Some("false"));
        assert_eq!(e.get(desktop::MAIN, "Actions"), Some("new-empty-window;"));
        assert_eq!(
            e.get("Desktop Action new-empty-window", "Exec"),
            Some("/usr/bin/psbox run code --app code -- %F")
        );

        let insiders = gen_.iter().find(|g| g.path.ends_with("insiders.desktop")).unwrap();
        let e = Entry::parse(&insiders.body).unwrap();
        assert_eq!(e.get(desktop::MAIN, "Name"), Some("Code Insiders"));
        assert_eq!(e.get(desktop::MAIN, "Type"), Some("Application"));
        assert_eq!(e.get(desktop::MAIN, "Icon"), Some("/home/u/Sandbox/code.bin/icon.png"));
        assert_eq!(e.get(desktop::MAIN, "MimeType"), Some("x-scheme-handler/vscode;"));
        assert_eq!(
            e.get(desktop::MAIN, "Exec"),
            Some("/usr/bin/psbox run code --app insiders -- %U")
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
        let (root, dirs) = setup("missing");
        let s = spec("x", "{ app+: { x: { 'source-id': 'x' } } }", &[]);
        let generation = generate(&[("x".into(), s)], &dirs, psbox());
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
    fn autostart_entries_start_their_app() {
        let (root, dirs) = setup("autostart");
        let s = spec(
            "steam",
            "{ app+: { 'steam-autostart': { kind: 'autostart', 'source-id': 'steam-autostart',
                                            'remove-arguments': ['-silent'] } } }",
            &[],
        );
        let generation = generate(&[("steam".into(), s)], &dirs, psbox());
        assert!(generation.warnings.is_empty(), "{:?}", generation.warnings);
        assert_eq!(generation.entries.len(), 1);
        assert!(generation.entries[0].path.starts_with(&dirs.autostart));
        let e = Entry::parse(&generation.entries[0].body).unwrap();
        assert_eq!(
            e.get(desktop::MAIN, "Exec"),
            Some("/usr/bin/psbox run steam --app steam --")
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn builds_entries_from_keys_and_actions() {
        let (root, dirs) = setup("keys");
        let s = spec(
            "app",
            "{ app+: { app: {
                 keys: { Name: { default: 'App', de: 'Anwendung' }, Categories: ['Game', 'Emulator'], Terminal: false },
                 actions: { 'new-window': { Name: 'New Window', arguments: ['--new-window'] } },
                 'no-display': true,
               } } }",
            &[],
        );
        let generation = generate(&[("app".into(), s)], &dirs, psbox());
        assert!(generation.warnings.is_empty(), "{:?}", generation.warnings);
        let e = Entry::parse(&generation.entries[0].body).unwrap();
        assert_eq!(e.get(desktop::MAIN, "Name"), Some("App"));
        assert_eq!(e.get(desktop::MAIN, "Name[de]"), Some("Anwendung"));
        assert_eq!(e.get(desktop::MAIN, "Categories"), Some("Game;Emulator;"));
        assert_eq!(e.get(desktop::MAIN, "Terminal"), Some("false"));
        assert_eq!(e.get(desktop::MAIN, "NoDisplay"), Some("true"));
        assert_eq!(e.get(desktop::MAIN, "Exec"), Some("/usr/bin/psbox run app --app app"));
        assert_eq!(e.get(desktop::MAIN, "Actions"), Some("new-window;"));
        assert_eq!(e.get("Desktop Action new-window", "Name"), Some("New Window"));
        assert_eq!(
            e.get("Desktop Action new-window", "Exec"),
            Some("/usr/bin/psbox run app --app app -- --new-window")
        );

        // Keys and actions set to null remove what was copied.
        let s = spec(
            "code-oss",
            "{ app+: { 'code-oss': { 'source-id': 'code-oss',
                                     keys: { Icon: null, Name: 'Mine' },
                                     actions: { 'new-empty-window': null } } } }",
            &[],
        );
        let generation = generate(&[("code-oss".into(), s)], &dirs, psbox());
        let e = Entry::parse(&generation.entries[0].body).unwrap();
        assert_eq!(e.get(desktop::MAIN, "Icon"), None);
        assert_eq!(e.get(desktop::MAIN, "Name"), Some("Mine"));
        assert_eq!(e.get(desktop::MAIN, "Actions"), None);
        assert!(e.actions().is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_implicit_default_app_inherits_a_host_entry() {
        let (root, dirs) = setup("implicit");
        let s = spec("code-oss", "{}", &["code-oss"]);
        let generation = generate(&[("code-oss".into(), s)], &dirs, psbox());
        assert!(generation.notes.is_empty() && generation.warnings.is_empty());
        let e = Entry::parse(&generation.entries[0].body).unwrap();
        assert_eq!(e.get(desktop::MAIN, "Name"), Some("Code"));
        assert_eq!(
            e.get(desktop::MAIN, "Exec"),
            Some("/usr/bin/psbox run code-oss --app code-oss -- --new-window %F")
        );

        // An entry only in the user's own applications directory counts too.
        fs::create_dir_all(&dirs.applications).unwrap();
        fs::write(
            dirs.applications.join("mine.desktop"),
            "[Desktop Entry]\nName=Mine\nExec=mine\n",
        )
        .unwrap();
        let s = spec("mine", "{}", &["mine"]);
        let generation = generate(&[("mine".into(), s)], &dirs, psbox());
        assert!(generation.failed.is_empty(), "{:?}", generation.warnings);
        let e = Entry::parse(&generation.entries[0].body).unwrap();
        assert_eq!(e.get(desktop::MAIN, "Name"), Some("Mine"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_sandbox_whose_name_sorts_first_gets_a_shared_file() {
        let (root, dirs) = setup("shared");
        let web = |name: &str| spec(name, "{ app+: { web: { run: ['web'], id: 'shared' } } }", &[]);
        let generation = generate(
            &[("beta".into(), web("beta")), ("alpha".into(), web("alpha"))],
            &dirs,
            psbox(),
        );
        let shared = dirs.applications.join("shared.desktop");
        let winners: Vec<&str> = generation
            .entries
            .iter()
            .filter(|g| g.path == shared)
            .map(|g| g.sandbox.as_str())
            .collect();
        assert_eq!(winners, ["alpha"]);
        assert!(
            generation
                .warnings
                .iter()
                .any(|(sandbox, _, why)| sandbox == "beta" && why.contains("also belongs to alpha")),
            "{:?}",
            generation.warnings
        );
        assert!(generation.failed.contains(&shared));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn another_sandboxs_entry_claims_its_id() {
        let (root, dirs) = setup("claimed");
        fs::create_dir_all(&dirs.applications).unwrap();
        fs::write(
            dirs.applications.join("shared.desktop"),
            format!("{GENERATOR_MARKER}\n{SANDBOX_TAG}alpha\n[Desktop Entry]\n"),
        )
        .unwrap();
        assert_eq!(claimant("shared", "beta", &dirs).as_deref(), Some("alpha"));
        assert_eq!(claimant("shared", "alpha", &dirs), None);
        assert_eq!(claimant("other", "beta", &dirs), None);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn ids_map_dashes_to_directories_as_gio_does() {
        assert_eq!(
            id_paths("kde4-foo"),
            [PathBuf::from("kde4-foo.desktop"), PathBuf::from("kde4/foo.desktop")]
        );
    }
}
