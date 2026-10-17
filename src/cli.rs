//! The commands that report rather than run something in a sandbox: `list`,
//! `status`, `mount`, `umount`, `stop`, `install`, `devices`, `check`, and
//! `completions`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use clap_complete::env::EnvCompleter;
use serde::Serialize;

use crate::devices::{self, HostPaths};
use crate::plan::{HostEnv, Identity, Plan, find_in_path, resolve_network};
use crate::protocol::{Request, Response};
use crate::spec::{DeviceClass, MountMode, Network, Spec, Tokens};
use crate::systemd::{self, Systemd};
use crate::ui::{self, BOLD, CYAN, Cell, DIM, GREEN, RED, Table, YELLOW, paint};
use crate::{client, install, sys};

/// One row of `psbox list`.
#[derive(Serialize)]
struct SandboxInfo {
    name: String,
    /// `running`, `stopped`, or `error` (the config does not load).
    state: &'static str,
    /// Running instances: `default` and named ones.
    instances: Vec<String>,
    /// Tasks running now.
    running_tasks: Vec<String>,
    /// The network the sandbox gets on this host.
    #[serde(skip_serializing_if = "Option::is_none")]
    network: Option<Network>,
    /// `network` is `host` only because pasta is not installed.
    network_fallback: bool,
    commands: Vec<String>,
    tasks: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// One line about a config error: where and what, without the source
/// excerpt TOML errors carry.
fn error_summary(e: &anyhow::Error) -> String {
    if let Some(c) = e.downcast_ref::<crate::spec::ConfigError>() {
        return c.summary();
    }
    let text = e.root_cause().to_string();
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let first = lines.next().unwrap_or_default();
    match (first.strip_prefix("TOML parse error at "), text.lines().last()) {
        (Some(at), Some(what)) if what != first => format!("{at}: {what}"),
        _ => first.to_string(),
    }
}

pub fn list(json: bool) -> Result<i32> {
    let host = HostEnv::from_process()?;
    let running: Vec<String> = Systemd::connect()?
        .active_units(&format!("{}*.service", systemd::UNIT_PREFIX))?
        .into_iter()
        .filter_map(|u| {
            u.strip_prefix(systemd::UNIT_PREFIX)?
                .strip_suffix(".service")
                .map(String::from)
        })
        .collect();
    let mut sandboxes = Vec::new();
    for (name, spec) in Spec::load_all(&host.config_dir)? {
        let mut instances = Vec::new();
        let mut running_tasks = Vec::new();
        for key in &running {
            match Identity::from_key(key) {
                Ok(Identity::Instance { sandbox, instance }) if sandbox == name => {
                    instances.push(instance.unwrap_or_else(|| "default".into()));
                }
                Ok(Identity::Task { sandbox, task }) if sandbox == name => running_tasks.push(task),
                _ => {}
            }
        }
        instances.sort();
        running_tasks.sort();
        let (mut network, mut network_fallback) = (None, false);
        let (commands, tasks, error) = match spec {
            Ok(s) => {
                let mut commands: Vec<String> = s.command.keys().cloned().collect();
                if commands.is_empty() {
                    commands.push(name.clone());
                }
                let error = match resolve_network(s.network, host.pasta) {
                    Some((n, fallback)) => {
                        (network, network_fallback) = (Some(n), fallback);
                        None
                    }
                    None => Some("network = \"isolated\" needs pasta, which is not installed".into()),
                };
                (commands, s.task.keys().cloned().collect(), error)
            }
            Err(e) => (Vec::new(), Vec::new(), Some(error_summary(&e))),
        };
        let state = match (&error, instances.is_empty() && running_tasks.is_empty()) {
            (Some(_), _) => "error",
            (None, true) => "stopped",
            (None, false) => "running",
        };
        sandboxes.push(SandboxInfo {
            name,
            state,
            instances,
            running_tasks,
            network,
            network_fallback,
            commands,
            tasks,
            error,
        });
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&sandboxes)?);
        return Ok(0);
    }
    if sandboxes.is_empty() {
        anstream::println!(
            "no sandboxes configured in {}",
            ui::code(ui::tilde(host.config_dir.join("sandboxes")))
        );
        return Ok(0);
    }
    let mut table = Table::new(&["SANDBOX", "STATE", "NETWORK", "COMMANDS", "TASKS"]).flex(3);
    for s in &sandboxes {
        let name = Cell::styled(BOLD, &s.name);
        let mut network = Cell::new(s.network.map(|n| n.to_string()).unwrap_or_default());
        if s.network_fallback {
            network = network.then(YELLOW, " (no pasta)");
        }
        let commands = Cell::new(s.commands.join(", "));
        let tasks = Cell::new(s.tasks.join(", "));
        match (&s.error, s.state) {
            (Some(e), _) => table.marked_row(
                Cell::styled(RED, "✗"),
                vec![
                    name,
                    Cell::styled(RED, "config error"),
                    Cell::new(""),
                    Cell::styled(DIM, e.clone()),
                ],
            ),
            (None, "running") => {
                let mut what = s.instances.clone();
                what.extend(s.running_tasks.iter().map(|t| format!("task {t}")));
                let mut state = Cell::styled(GREEN, "running");
                if what != ["default"] {
                    state = state.then(DIM, format!(" · {}", what.join(", ")));
                }
                table.marked_row(Cell::styled(GREEN, "●"), vec![name, state, network, commands, tasks]);
            }
            _ => table.marked_row(
                Cell::styled(DIM, "○"),
                vec![name, Cell::styled(DIM, "stopped"), network, commands, tasks],
            ),
        }
    }
    table.print();
    Ok(0)
}

fn mode_name(mode: MountMode) -> &'static str {
    match mode {
        MountMode::Ro => "read-only",
        MountMode::Rw => "read-write",
        MountMode::Overlay => "overlay",
        MountMode::Empty => "empty",
    }
}

pub fn status(id: &Identity, json: bool) -> Result<i32> {
    let Response::Status(s) = client::request(id, Request::Status)? else {
        bail!("unexpected response to a status request");
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&s)?);
        return Ok(0);
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut facts = vec![format!("init {}", s.init_pid)];
    facts.push(match s.process_count {
        1 => "1 process".into(),
        n => format!("{n} processes"),
    });
    facts.push(ui::count(s.clients, "client"));
    facts.push(match s.network_fallback {
        true => format!("network {} (pasta not installed)", s.network),
        false => format!("network {}", s.network),
    });
    if let Some(display) = s.display {
        facts.push(format!("X :{display}"));
    }
    anstream::println!(
        "{} {}  {} for {}  {}",
        paint(GREEN, "●"),
        paint(BOLD, id.describe()),
        paint(GREEN, "running"),
        ui::duration(now.saturating_sub(s.started)),
        paint(DIM, format!("· {}", facts.join(" · ")))
    );
    if !s.restart_pending.is_empty() {
        anstream::println!(
            "  {} {} {}",
            paint(YELLOW, "restart pending:"),
            s.restart_pending.join(", "),
            paint(DIM, "— applies when the sandbox is idle")
        );
    }

    let mut processes = Table::new(&["PID", "COMMAND", "COMMAND LINE"]).flex(2);
    for p in &s.processes {
        let mut line = Cell::new(p.argv.first().cloned().unwrap_or_default());
        if p.argv.len() > 1 {
            line = line.then(DIM, format!(" {}", p.argv[1..].join(" ")));
        }
        processes.row(vec![p.pid.to_string().into(), Cell::styled(BOLD, &p.command), line]);
    }
    let mut mounts = Table::new(&["MOUNTED", "AT", "MODE"]).flex(0);
    for m in &s.mounts {
        mounts.row(vec![
            Cell::styled(CYAN, ui::tilde(&m.host)),
            Cell::styled(CYAN, ui::tilde(&m.at)),
            Cell::styled(DIM, mode_name(m.mode)),
        ]);
    }
    let mut devices = Table::new(&["CLASS", "DEVICE", "PATH"]);
    for d in &s.devices {
        let path = d
            .links
            .iter()
            .find_map(|l| {
                l.split_once("by-path/")
                    .map(|(_, p)| p.trim_end_matches("-render").to_string())
            })
            .unwrap_or_default();
        devices.row(vec![
            d.classes.join(", ").into(),
            Cell::styled(CYAN, format!("/dev/{}", d.node)),
            Cell::styled(DIM, path),
        ]);
    }
    for table in [processes, mounts, devices] {
        if !table.is_empty() {
            anstream::println!();
            table.print();
        }
    }
    Ok(0)
}

/// `path` made absolute against the current directory, unless it starts
/// with a path token.
fn absolute(path: &str) -> Result<String> {
    if path.starts_with('/') || path.starts_with('~') || path.starts_with('@') {
        return Ok(path.to_string());
    }
    Ok(std::env::current_dir()?.join(path).to_string_lossy().into_owned())
}

pub fn mount(id: &Identity, host: &str, at: Option<String>, ro: bool) -> Result<i32> {
    let host = absolute(host)?;
    let mode = if ro { MountMode::Ro } else { MountMode::Rw };
    let dst = at.clone().unwrap_or_else(|| host.clone());
    client::request(
        id,
        Request::Mount {
            host: host.clone(),
            at: dst.clone(),
            mode,
        },
    )
    .map_err(|e| refused(e, format!("couldn't mount {} in {}", ui::tilde(&host), id.describe())))?;
    let place = match at {
        Some(_) => format!(" at {}", ui::code(ui::tilde(&dst))),
        None => String::new(),
    };
    ui::done(format!(
        "mounted {}{place} in {} {}",
        ui::code(ui::tilde(&host)),
        paint(BOLD, id.describe()),
        paint(DIM, format!("({})", mode_name(mode)))
    ));
    Ok(0)
}

/// Says what failed when the sandbox refused a request; other errors (such
/// as the sandbox not running) already say what is wrong.
fn refused(e: anyhow::Error, what: String) -> anyhow::Error {
    match e.is::<client::Refused>() {
        true => e.context(what),
        false => e,
    }
}

pub fn umount(id: &Identity, at: String) -> Result<i32> {
    client::request(id, Request::Umount { at: at.clone() })
        .map_err(|e| refused(e, format!("couldn't unmount {} in {}", ui::tilde(&at), id.describe())))?;
    ui::done(format!(
        "unmounted {} in {}",
        ui::code(ui::tilde(&at)),
        paint(BOLD, id.describe())
    ));
    Ok(0)
}

/// Runs a task, then has the sandbox's running instances refresh the
/// overlays over the directories the task could write, so they see what it
/// changed (see [`Request::Refresh`]). Problems refreshing are warnings; the
/// task's own exit code stands.
pub fn task(sandbox: &str, task: &str, args: Vec<String>, create_dirs: bool) -> Result<i32> {
    let id = Identity::Task {
        sandbox: sandbox.into(),
        task: task.into(),
    };
    let code = client::run(&id, Some(task.into()), args, false, create_dirs)?;
    if let Err(e) = refresh_after_task(sandbox, task) {
        ui::warning(format!(
            "running instances of {sandbox} may not show what the task changed ({e:#}); {} restarts them",
            ui::code(format!("psbox stop {sandbox}"))
        ));
    }
    Ok(code)
}

fn refresh_after_task(sandbox: &str, task: &str) -> Result<()> {
    let host = HostEnv::from_process()?;
    let hosts = writable_hosts(&Spec::load(sandbox, &host.config_dir)?, sandbox, task, &host)?;
    if hosts.is_empty() {
        return Ok(());
    }
    let running = Systemd::connect()?.active_units(&format!("{}{sandbox}*.service", systemd::UNIT_PREFIX))?;
    for key in running
        .iter()
        .filter_map(|u| u.strip_prefix(systemd::UNIT_PREFIX)?.strip_suffix(".service"))
    {
        let instance = match Identity::from_key(key) {
            Ok(i @ Identity::Instance { .. }) if i.sandbox() == sandbox => i,
            _ => continue,
        };
        // One instance that cannot refresh does not stop the others.
        if let Err(e) = client::request(&instance, Request::Refresh { hosts: hosts.clone() }) {
            ui::warning(format!(
                "{} still shows what was there before the task ({e:#}); {} restarts it",
                instance.describe(),
                ui::code(format!("psbox stop {}", instance.cli_args()))
            ));
        }
    }
    Ok(())
}

/// The host directories behind a task's `writable` mounts.
fn writable_hosts(spec: &Spec, sandbox: &str, task: &str, host: &HostEnv) -> Result<Vec<String>> {
    let Some(t) = spec.task.get(task) else {
        return Ok(Vec::new());
    };
    let tokens = Tokens {
        home: &host.home,
        root: &host.root,
        name: sandbox,
    };
    t.writable
        .iter()
        .filter_map(|name| spec.mount.get(name)?.host.as_deref())
        .map(|h| Ok(tokens.expand(h)?.to_string_lossy().into_owned()))
        .collect()
}

pub fn stop(id: &Identity) -> Result<i32> {
    client::request(id, Request::Stop)?;
    // Wait for systemd to see the service go, so that a command right after
    // this one does not activate the socket again.
    let systemd = Systemd::connect()?;
    let unit = systemd::service_unit(&id.key());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while systemd.is_up(&unit)? && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    ui::done(format!("stopped {}", paint(BOLD, id.describe())));
    Ok(0)
}

/// Stops every running sandbox through systemd, which also reaches
/// sandboxes still running an older psbox. Unnamed sandboxes lose their
/// socket units too, as they are never started again.
pub fn stop_all() -> Result<i32> {
    let systemd = Systemd::connect()?;
    let units = systemd.active_units(&format!("{}*.service", systemd::UNIT_PREFIX))?;
    let mut code = 0;
    let mut stopping = Vec::new();
    for unit in units {
        let key = unit
            .strip_prefix(systemd::UNIT_PREFIX)
            .and_then(|u| u.strip_suffix(".service"))
            .unwrap_or(&unit)
            .to_string();
        let id = Identity::from_key(&key).ok();
        let name = id.as_ref().map_or_else(|| key.clone(), Identity::describe);
        let mut r = systemd.stop(&unit);
        if r.is_ok() && matches!(id, Some(Identity::Unnamed { .. })) {
            r = systemd.stop(&systemd::socket_unit(&key));
        }
        match r {
            Ok(()) => stopping.push((unit, name)),
            Err(e) => {
                ui::error(&e.context(format!("couldn't stop {name}")));
                code = 1;
            }
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && stopping.iter().any(|(unit, _)| systemd.is_up(unit).unwrap_or(false))
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    for (unit, name) in &stopping {
        if systemd.is_up(unit).unwrap_or(false) {
            ui::error(&anyhow::anyhow!("{name} is still stopping after 5s"));
            code = 1;
        } else {
            ui::done(format!("stopped {}", paint(BOLD, name)));
        }
    }

    // An unnamed sandbox's socket unit outlives it when its client was
    // killed between starting the unit and connecting; nothing else stops
    // it, since unnamed sandboxes are never started again.
    let strays = systemd.active_units(&format!("{}sandbox.*.socket", systemd::UNIT_PREFIX))?;
    let mut cleared = 0;
    for unit in strays {
        match systemd.stop(&unit) {
            Ok(()) => cleared += 1,
            Err(e) => {
                ui::error(&e.context(format!("couldn't stop {unit}")));
                code = 1;
            }
        }
    }
    if cleared > 0 {
        ui::done(format!(
            "removed {} left by unnamed sandboxes",
            ui::count(cleared, "socket unit")
        ));
    }
    if stopping.is_empty() && cleared == 0 && code == 0 {
        anstream::println!("no sandboxes are running");
    }
    Ok(code)
}

pub fn install(dry_run: bool, verbose: bool) -> Result<i32> {
    let host = HostEnv::from_process()?;
    let mut code = 0;
    let mut specs = Vec::new();
    let mut failed = Vec::new();
    for (name, spec) in Spec::load_all(&host.config_dir)? {
        match spec {
            Ok(s) => specs.push((name, s)),
            Err(e) => {
                failed.push((name, error_summary(&e)));
                code = 1;
            }
        }
    }
    if !failed.is_empty() {
        let (has, its) = match failed.len() {
            1 => ("has", "its"),
            _ => ("have", "their"),
        };
        ui::warning(format!(
            "{} {has} a config error {}",
            ui::count(failed.len(), "sandbox"),
            paint(DIM, format!("· {its} installed entries are kept"))
        ));
        let mut table = Table::new(&["SANDBOX", "ERROR"]);
        for (name, summary) in &failed {
            table.row(vec![Cell::styled(BOLD, name), Cell::new(summary.clone())]);
        }
        anstream::eprintln!();
        for line in table.render(None) {
            anstream::eprintln!("{line}");
        }
        let run = match failed.as_slice() {
            [(name, _)] => format!("psbox run {name}"),
            _ => "psbox run <sandbox>".into(),
        };
        ui::fix(format!("{} shows the error in full", ui::code(run)));
        anstream::eprintln!();
    }
    let failed: Vec<String> = failed.into_iter().map(|(name, _)| name).collect();
    let dirs = install::Dirs::from_env(&host);
    let exe = std::env::current_exe()?;
    let generation = install::generate(&specs, &host, &dirs, &exe);
    if !generation.warnings.is_empty() {
        let n = generation.warnings.len();
        let (entries, them) = match n {
            1 => ("desktop entry", "it"),
            _ => ("desktop entries", "them"),
        };
        ui::warning(format!(
            "{n} {entries} couldn't be made {}",
            paint(DIM, format!("· psbox install leaves {them} out"))
        ));
        let mut table = Table::new(&["SANDBOX", "ENTRY", "PROBLEM"]);
        for (sandbox, entry, problem) in &generation.warnings {
            table.row(vec![
                Cell::styled(BOLD, sandbox),
                Cell::new(entry.clone()),
                Cell::new(problem.clone()),
            ]);
        }
        anstream::eprintln!();
        for line in table.render(None) {
            anstream::eprintln!("{line}");
        }
        if generation
            .warnings
            .iter()
            .any(|(_, _, p)| p.starts_with("no host entry"))
        {
            ui::fix(format!(
                "install the app, or write the entry in the config with {}",
                ui::code("desktop.entry")
            ));
        }
        anstream::eprintln!();
        code = 1;
    }
    let report = install::apply(&generation, &failed, &dirs, dry_run)?;
    if !report.skipped.is_empty() {
        let n = report.skipped.len();
        ui::warning(format!(
            "left {} alone {}",
            ui::count(n, "file"),
            paint(
                DIM,
                format!("· psbox didn't write {}", if n == 1 { "it" } else { "them" })
            )
        ));
        for p in &report.skipped {
            anstream::eprintln!("  {}", ui::code(ui::tilde(p)));
        }
        code = 1;
    }

    let (add, update, remove) = if dry_run {
        ("would add", "would update", "would remove")
    } else {
        ("added", "updated", "removed")
    };
    let sandbox_of = |p: &PathBuf| {
        generation
            .entries
            .iter()
            .find(|g| g.path == *p)
            .map(|g| g.sandbox.clone())
            .unwrap_or_default()
    };
    let mut groups: BTreeMap<String, Vec<(Cell, &str, &PathBuf)>> = BTreeMap::new();
    let mut add_line =
        |group: String, marker: Cell, verb, path| groups.entry(group).or_default().push((marker, verb, path));
    for p in &report.added {
        add_line(sandbox_of(p), Cell::styled(GREEN, "+"), add, p);
    }
    for p in &report.updated {
        add_line(sandbox_of(p), Cell::styled(YELLOW, "~"), update, p);
    }
    if verbose {
        for p in &report.unchanged {
            add_line(sandbox_of(p), Cell::styled(DIM, "="), "unchanged", p);
        }
    }
    let removed: Vec<_> = report
        .removed
        .iter()
        .map(|p| (Cell::styled(RED, "−"), remove, p))
        .collect();

    if dry_run {
        anstream::println!("{}", paint(DIM, "dry run: nothing was written"));
    }
    let print_group = |heading: &str, lines: &[(Cell, &str, &PathBuf)]| {
        anstream::println!("{}", paint(BOLD, heading));
        let mut table = Table::headless(2).flex(1);
        for (marker, verb, path) in lines {
            table.marked_row(
                marker.clone(),
                vec![(*verb).into(), Cell::styled(CYAN, ui::tilde(path))],
            );
        }
        for line in table.render(ui::stdout_width()) {
            anstream::println!("  {line}");
        }
    };
    for (sandbox, lines) in &groups {
        print_group(sandbox, lines);
    }
    if !removed.is_empty() {
        print_group("no longer configured", &removed);
    }

    let changed = report.added.len() + report.updated.len() + report.removed.len();
    if changed == 0 && code == 0 {
        let n = report.unchanged.len();
        let what = match n {
            0 => "no entries are configured".to_string(),
            1 => "the 1 entry is up to date".to_string(),
            n => format!("all {n} entries are up to date"),
        };
        ui::done(what);
    } else {
        let parts: Vec<String> = [
            (report.added.len(), add),
            (report.updated.len(), update),
            (report.removed.len(), remove),
            (report.unchanged.len(), "unchanged"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| format!("{n} {what}"))
        .collect();
        if !parts.is_empty() {
            if !groups.is_empty() || !removed.is_empty() {
                anstream::println!();
            }
            anstream::println!("{}", paint(DIM, parts.join(" · ")));
        }
    }
    Ok(code)
}

/// One device in `psbox devices --json`.
#[derive(Serialize)]
struct DeviceInfo {
    device: String,
    number: String,
    subsystem: String,
    classes: Vec<String>,
    id: Option<String>,
    name: String,
    path: Option<String>,
}

const LISTED_CLASSES: [DeviceClass; 5] = [
    DeviceClass::Gpu,
    DeviceClass::Camera,
    DeviceClass::Gamepad,
    DeviceClass::SecurityKey,
    DeviceClass::Alsa,
];

pub fn devices(all: bool, json: bool) -> Result<i32> {
    let host = HostPaths::system();
    let mut keys = devices::scan(&host);
    keys.sort();
    let mut rows = Vec::new();
    let mut others = 0;
    for (major, minor) in keys {
        let Some(d) = devices::read_device(&host, major, minor) else {
            continue;
        };
        let classes = devices::classify(&d, &LISTED_CLASSES);
        if classes.is_empty() && !all {
            others += 1;
            continue;
        }
        let description = devices::describe(&host, &d);
        rows.push((d, classes, description));
    }
    let rank = |classes: &[DeviceClass]| {
        classes
            .first()
            .and_then(|c| LISTED_CLASSES.iter().position(|l| l == c))
            .unwrap_or(LISTED_CLASSES.len())
    };
    rows.sort_by(|a, b| rank(&a.1).cmp(&rank(&b.1)).then_with(|| a.0.devname.cmp(&b.0.devname)));

    if json {
        let list: Vec<DeviceInfo> = rows
            .into_iter()
            .map(|(d, classes, desc)| DeviceInfo {
                device: format!("/dev/{}", d.devname),
                number: format!("{}:{}", d.major, d.minor),
                subsystem: d.subsystem,
                classes: classes.iter().map(ToString::to_string).collect(),
                id: desc.id,
                name: desc.name,
                path: desc.path,
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(0);
    }

    let header: &[&str] = if all {
        &["CLASS", "DEVICE", "SUBSYSTEM", "ID", "DESCRIPTION"]
    } else {
        &["CLASS", "DEVICE", "ID", "DESCRIPTION"]
    };
    let mut table = Table::new(header).flex(header.len() - 1);
    for (d, classes, desc) in &rows {
        let mut description = Cell::new(desc.name.clone());
        if let Some(p) = &desc.path {
            description = description.then(DIM, format!(" {p}"));
        }
        let mut cells = vec![
            Cell::new(classes.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")),
            Cell::styled(CYAN, format!("/dev/{}", d.devname)),
        ];
        if all {
            cells.push(Cell::styled(DIM, d.subsystem.clone()));
        }
        cells.push(desc.id.clone().unwrap_or_default().into());
        cells.push(description);
        table.row(cells);
    }
    if !table.is_empty() {
        table.print();
    }
    let missing: Vec<String> = LISTED_CLASSES
        .iter()
        .filter(|c| !rows.iter().any(|(_, classes, _)| classes.contains(c)))
        .map(ToString::to_string)
        .collect();
    let mut notes = Vec::new();
    if let Some((last, rest)) = missing.split_last() {
        let names = if rest.is_empty() {
            last.clone()
        } else {
            format!("{} or {last}", rest.join(", "))
        };
        notes.push(format!("no {names} found"));
    }
    if all {
        notes.push("grant one USB device with usb:VENDOR:PRODUCT (its ID)".into());
    } else if others > 0 {
        notes.push(format!("{others} other devices: psbox devices --all"));
    }
    if !notes.is_empty() {
        anstream::println!("  {}", paint(DIM, notes.join(" · ")));
    }
    Ok(0)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Ok,
    Warn,
    Fail,
}

/// Runs the startup check for a sandbox and each of its tasks, without
/// starting anything.
pub fn check_sandbox(name: &str) -> Result<i32> {
    let host = HostEnv::from_process()?;
    let spec = Spec::load(name, &host.config_dir)?;
    let mut ids = vec![Identity::Instance {
        sandbox: name.into(),
        instance: None,
    }];
    ids.extend(spec.task.keys().map(|task| Identity::Task {
        sandbox: name.into(),
        task: task.clone(),
    }));
    let mut code = 0;
    // Tasks usually share the sandbox's mounts, and so its problems: those
    // are named once instead of repeating the same report.
    let mut first: Option<Vec<crate::plan::Problem>> = None;
    let mut same = Vec::new();
    for id in &ids {
        let checked = Plan::build(id, &spec, &host).and_then(|plan| {
            plan.preflight(id, false)?;
            Ok(plan)
        });
        match checked {
            Ok(plan) => {
                ui::done(format!("{} can start", paint(BOLD, id.describe())));
                if plan.network_fallback {
                    client::warn_network_fallback(id);
                }
            }
            Err(e) => {
                code = 1;
                let problems = e.downcast_ref::<crate::plan::Report>().map(|r| r.problems().to_vec());
                match (&first, &problems, id) {
                    (Some(f), Some(p), Identity::Task { task, .. }) if f == p => {
                        same.push(task.clone());
                        continue;
                    }
                    (None, _, Identity::Instance { .. }) => first = problems,
                    _ => {}
                }
                ui::error(&e);
            }
        }
    }
    if !same.is_empty() {
        let tasks = if same.len() == 1 { "task" } else { "tasks" };
        anstream::eprintln!(
            "{} its {tasks} {} can't start either, for the same reasons",
            paint(ui::RED, "✗"),
            same.join(", ")
        );
    }
    Ok(code)
}

/// The oldest kernel psbox supports: it uses the newest interfaces without
/// fallbacks.
const REQUIRED_KERNEL: (u32, u32) = (7, 2);

pub fn check() -> Result<i32> {
    let mut items: Vec<(Level, &str, String)> = Vec::new();
    let need = sys::landlock::REQUIRED_ABI;
    items.push(match sys::landlock::abi_version() {
        Ok(abi) if abi >= need => (Level::Ok, "Landlock", format!("ABI {abi} (needs {need})")),
        Ok(abi) => (Level::Fail, "Landlock", format!("ABI {abi}, needs {need}")),
        Err(e) => (Level::Fail, "Landlock", format!("unavailable: {e}")),
    });

    let (major, minor) = REQUIRED_KERNEL;
    items.push(match sys::ns::kernel_version() {
        Ok(v) if v >= REQUIRED_KERNEL => (Level::Ok, "kernel", format!("{}.{} (needs {major}.{minor})", v.0, v.1)),
        Ok(v) => (Level::Fail, "kernel", format!("{}.{}, needs {major}.{minor}", v.0, v.1)),
        Err(e) => (Level::Fail, "kernel", format!("{e}")),
    });

    // A real attempt, explained by the usual settings when it fails.
    let read = |p: &str| std::fs::read_to_string(p).map(|s| s.trim().to_string()).ok();
    items.push(match sys::ns::probe_user_namespace() {
        Ok(()) => (Level::Ok, "user namespaces", "available".into()),
        Err(_) if read("/proc/sys/user/max_user_namespaces").is_some_and(|v| v == "0") => (
            Level::Fail,
            "user namespaces",
            "disabled (user.max_user_namespaces = 0)".into(),
        ),
        Err(_) if read("/proc/sys/kernel/apparmor_restrict_unprivileged_userns").is_some_and(|v| v == "1") => (
            Level::Fail,
            "user namespaces",
            "restricted by AppArmor (kernel.apparmor_restrict_unprivileged_userns = 1)".into(),
        ),
        Err(e) => (Level::Fail, "user namespaces", format!("cannot create one: {e}")),
    });

    items.push(if Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
        (Level::Ok, "cgroup v2", "unified hierarchy".into())
    } else {
        (Level::Fail, "cgroup v2", "not mounted at /sys/fs/cgroup".into())
    });

    let units = [systemd::socket_unit(""), systemd::service_unit("")];
    let mut units_missing = false;
    items.push(match Systemd::connect() {
        Ok(sd) => {
            let missing: Vec<&str> = units
                .iter()
                .filter(|u| sd.unit_file_state(u).is_err())
                .map(String::as_str)
                .collect();
            units_missing = !missing.is_empty();
            if missing.is_empty() {
                (Level::Ok, "systemd units", units.join(", "))
            } else {
                (
                    Level::Fail,
                    "systemd units",
                    format!("{} not installed", missing.join(", ")),
                )
            }
        }
        Err(e) => (
            Level::Fail,
            "systemd units",
            format!("cannot reach the user manager: {e:#}"),
        ),
    });

    for (tool, level, needed) in [
        ("dbus-daemon", Level::Fail, "dbus = \"portal\" (the default)"),
        ("xwayland-satellite", Level::Warn, "X11 apps (xwayland)"),
        ("Xwayland", Level::Warn, "X11 apps (xwayland)"),
    ] {
        items.push(match find_in_path(tool) {
            Some(p) => (Level::Ok, tool, ui::tilde(p)),
            None => (level, tool, format!("not found — needed for {needed}")),
        });
    }
    items.push(match find_in_path("pasta") {
        Some(p) => (Level::Ok, "pasta", ui::tilde(p)),
        None => (
            Level::Warn,
            "pasta",
            "not installed: sandboxes without network fall back to the host network".into(),
        ),
    });

    let mut table = Table::headless(2);
    for (level, name, detail) in &items {
        let (marker, style) = match level {
            Level::Ok => (Cell::styled(GREEN, "✓"), DIM),
            Level::Warn => (Cell::styled(YELLOW, "!"), YELLOW),
            Level::Fail => (Cell::styled(RED, "✗"), RED),
        };
        table.marked_row(marker, vec![(*name).into(), Cell::styled(style, detail.clone())]);
    }
    table.print();
    if units_missing {
        anstream::println!(
            "{}",
            ui::fix_line(format!(
                "install them with your package, or run {} in a psbox checkout",
                ui::code("make dev/install")
            ))
        );
    }
    Ok(if items.iter().any(|(l, _, _)| *l == Level::Fail) {
        1
    } else {
        0
    })
}

/// `psbox open-uri`, the link handler inside sandboxes (see
/// [`crate::plan::PSBOX_DIR`]): asks the OpenURI portal on the session
/// bus to open `uri` in the host's default app. The activation token the
/// app that opened the link was given goes along, so the window that shows
/// the link can come to the front. It waits for the portal's response: the
/// host portal cancels a request whose caller disconnects, which would close
/// the app chooser it shows when no default app is set.
pub fn open_uri(uri: &str) -> Result<i32> {
    use crate::dbus::message::Message;
    use crate::dbus::{Conn, Value};
    use crate::portal::{OPEN_URI_IFACE, PORTAL_NAME, PORTAL_PATH};

    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS")
        .map_err(|_| anyhow::anyhow!("can't open {uri}: there is no session bus"))?;
    let bus = Conn::connect(&address).map_err(|e| anyhow::anyhow!("can't open {uri}: session bus: {e}"))?;
    let mut options = Value::Dict("s".into(), "v".into(), Vec::new());
    if let Ok(token) = std::env::var("XDG_ACTIVATION_TOKEN")
        && !token.is_empty()
    {
        options.dict_set("activation_token", Value::Str(token));
    }
    let failed = |e: std::io::Error| anyhow::anyhow!("can't open {uri}: {e}");
    // Subscribed before calling, so neither the response nor the portal
    // leaving can fall in between.
    for rule in [
        format!("type='signal',sender='{PORTAL_NAME}',interface='org.freedesktop.portal.Request',member='Response'"),
        format!("type='signal',sender='org.freedesktop.DBus',member='NameOwnerChanged',arg0='{PORTAL_NAME}'"),
    ] {
        bus.bus_call("AddMatch", &[Value::Str(rule)]).map_err(failed)?;
    }
    let call = Message::method_call(
        PORTAL_NAME,
        PORTAL_PATH,
        OPEN_URI_IFACE,
        "OpenURI",
        &[Value::Str(String::new()), Value::Str(uri.into()), options],
    );
    let reply = bus.call(&call).map_err(failed)?;
    let handle = reply
        .args()
        .ok()
        .and_then(|a| a.first().and_then(|v| v.as_str().map(str::to_string)))
        .ok_or_else(|| anyhow::anyhow!("can't open {uri}: the portal returned no request"))?;
    loop {
        let (msg, _) = bus.recv().map_err(failed)?;
        if msg.is("org.freedesktop.portal.Request", "Response") && msg.path.as_deref() == Some(handle.as_str()) {
            return match msg.args().ok().and_then(|a| a.first().and_then(Value::as_u32)) {
                Some(0) => Ok(0),
                Some(1) => bail!("{uri} was not opened: cancelled"),
                _ => bail!("{uri} was not opened"),
            };
        }
        // A portal being activated shows up here too, with a new owner.
        let gone = msg.is(crate::dbus::conn::BUS_NAME, "NameOwnerChanged")
            && msg
                .args()
                .ok()
                .and_then(|a| a.get(2).and_then(|v| v.as_str().map(str::is_empty)))
                == Some(true);
        if gone {
            bail!("can't open {uri}: the portal went away");
        }
    }
}

/// Prints the script that registers psbox's completions with `shell`.
/// Completion calls back into this binary, so the script is best sourced
/// fresh at shell startup rather than saved.
pub fn completions(shell: &dyn EnvCompleter) -> Result<i32> {
    let argv0 = std::env::args_os()
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| "psbox".into());
    let completer = if argv0.is_relative() && argv0.components().count() > 1 {
        std::env::current_dir()?.join(argv0)
    } else {
        argv0
    };
    let mut out = std::io::stdout().lock();
    shell.write_registration("COMPLETE", "psbox", "psbox", &completer.to_string_lossy(), &mut out)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tasks_writable_hosts_are_expanded() {
        let host = HostEnv {
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
            exe: "/usr/bin/psbox".into(),
        };
        let spec = Spec::parse(
            "[mount.bin]\nhost = \"@self.bin/\"\nat = \"~/.bin\"\nmode = \"overlay\"\n\
             [mount.data]\nhost = \"~/data\"\nmode = \"ro\"\n\
             [task.update]\nexec = [\"x\"]\nwritable = [\"bin\"]\n\
             [task.check]\nexec = [\"x\"]\n",
        )
        .unwrap();
        let hosts = writable_hosts(&spec, "app", "update", &host).unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!(Path::new(&hosts[0]), Path::new("/home/u/Sandbox/app.bin"));
        assert!(writable_hosts(&spec, "app", "check", &host).unwrap().is_empty());
    }

    #[test]
    fn config_errors_fit_on_one_line() {
        let toml_error = Spec::parse("broken = = toml").unwrap_err();
        let summary = error_summary(&toml_error);
        assert!(summary.starts_with("line 1, column"), "{summary}");
        assert!(!summary.contains('\n') && !summary.contains('|'), "{summary}");
        assert_eq!(error_summary(&anyhow::anyhow!("plain")), "plain");
    }
}
