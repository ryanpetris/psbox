//! psbox: private-home application sandboxes with live mounts and device
//! hotplug, built directly on Linux namespaces, Landlock, and cgroups.
//!
//! The library is the whole program; `main.rs` only calls [`main`]. It is a
//! library so that the fuzz targets in `fuzz/` can reach the parsers
//! (through the `fuzz` module, built only for them).

mod bus;
mod cli;
mod client;
mod connector;
mod dbus;
mod desktop;
mod devices;
mod forker;
mod gateway;
mod inject;
mod install;
mod plan;
mod portal;
mod protocol;
mod root;
mod spawn;
mod spec;
mod supervisor;
mod sys;
mod systemd;
mod ui;
mod unnamed;
mod x11;

#[cfg(fuzzing)]
pub mod fuzz;

use std::process::ExitCode;

use anyhow::Result;
use clap::builder::styling::Styles;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use clap_complete::engine::{ArgValueCandidates, CompletionCandidate};

#[derive(Parser)]
#[command(
    name = "psbox",
    version = option_env!("PSBOX_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")),
    about = "Run applications in private-home sandboxes",
    styles = STYLES,
    after_help = EXAMPLES,
)]
struct Cli {
    /// When to use colors.
    #[arg(
        long,
        global = true,
        value_name = "WHEN",
        default_value = "auto",
        display_order = 1000
    )]
    color: clap::ColorChoice,
    #[command(subcommand)]
    command: Cmd,
}

const STYLES: Styles = Styles::styled()
    .header(ui::GREEN.bold())
    .usage(ui::GREEN.bold())
    .literal(ui::CYAN.bold())
    .placeholder(ui::CYAN)
    .error(ui::RED.bold())
    .valid(ui::GREEN)
    .invalid(ui::YELLOW);

const EXAMPLES: &str = "\x1b[1;32mExamples:\x1b[0m
  psbox run firefox -- https://example.com
  psbox run code -i work
  psbox sandbox --home-tmpfs --bind . -- ./build.sh
  psbox mount firefox ~/Music --ro";

/// The commands of `psbox --help`, in groups.
const GROUPS: &[(&str, &[&str])] = &[
    ("Run", &["run", "sandbox", "task"]),
    ("Running sandboxes", &["list", "status", "mount", "umount", "stop"]),
    ("Setup", &["install", "devices", "check", "completions"]),
];

/// A sandbox and, optionally, one of its named instances.
#[derive(clap::Args)]
struct Target {
    #[arg(add = ArgValueCandidates::new(sandbox_names))]
    sandbox: String,
    /// A named instance (shares the sandbox's home).
    #[arg(short, long)]
    instance: Option<String>,
}

impl Target {
    fn identity(&self) -> plan::Identity {
        plan::Identity::Instance {
            sandbox: self.sandbox.clone(),
            instance: self.instance.clone(),
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a command in a sandbox, starting it if needed.
    Run {
        #[command(flatten)]
        target: Target,
        /// Command defined in the sandbox config.
        #[arg(short, long)]
        command: Option<String>,
        /// Treat the arguments as the whole command line (the command only
        /// supplies environment and app id).
        #[arg(long)]
        exec: bool,
        /// Also create missing directories marked with a trailing `/`
        /// outside the sandbox root.
        #[arg(long)]
        create_dirs: bool,
        /// Arguments appended to the command.
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Run a command in a new unnamed sandbox that ends when it exits.
    #[command(after_help = "\x1b[1;32mExamples:\x1b[0m
  psbox sandbox --home-tmpfs --network none -- ./untrusted-tool
  psbox sandbox --home ~/work/client --bind ~/Downloads -- bash")]
    Sandbox {
        #[command(flatten)]
        options: unnamed::Options,
        /// Also create missing directories marked with a trailing `/`
        /// outside the sandbox root.
        #[arg(long)]
        create_dirs: bool,
        /// The command and its arguments.
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Run one of a sandbox's tasks in its own instance and wait for it.
    Task {
        #[arg(add = ArgValueCandidates::new(sandbox_names))]
        sandbox: String,
        task: String,
        /// Also create missing directories marked with a trailing `/`
        /// outside the sandbox root.
        #[arg(long)]
        create_dirs: bool,
        /// Arguments appended to the task's command.
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Configured sandboxes and what is running.
    List {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Processes, mounts, and devices of a running sandbox.
    Status {
        #[command(flatten)]
        target: Target,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Attach a host path into a running sandbox.
    Mount {
        #[command(flatten)]
        target: Target,
        #[arg(value_hint = clap::ValueHint::AnyPath)]
        host: String,
        /// Destination in the sandbox (defaults to the host path).
        #[arg(long)]
        at: Option<String>,
        /// Read-only.
        #[arg(long)]
        ro: bool,
    },
    /// Detach a mount added with `mount`.
    Umount {
        #[command(flatten)]
        target: Target,
        at: String,
    },
    /// Stop a running sandbox.
    Stop {
        #[arg(
            add = ArgValueCandidates::new(sandbox_names),
            required_unless_present = "all",
            conflicts_with = "all"
        )]
        sandbox: Option<String>,
        /// A named instance (shares the sandbox's home).
        #[arg(short, long, conflicts_with = "all")]
        instance: Option<String>,
        /// Stop every running sandbox: instances, tasks, and unnamed ones.
        #[arg(long)]
        all: bool,
    },
    /// Write desktop and autostart entries for every sandbox.
    Install {
        /// Only report what would change.
        #[arg(long)]
        dry_run: bool,
        /// Also list entries that are already up to date.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Host devices that device classes grant.
    Devices {
        /// List every device, not only those a class matches.
        #[arg(long)]
        all: bool,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Check that this system provides what psbox needs, or that a sandbox
    /// can start.
    Check {
        /// Check this sandbox and its tasks instead of the system.
        #[arg(add = ArgValueCandidates::new(sandbox_names))]
        sandbox: Option<String>,
    },
    /// Print the shell completion script.
    #[command(after_help = "\x1b[1;32mSetup:\x1b[0m
  bash: echo 'source <(psbox completions bash)' >> ~/.bashrc
  zsh:  echo 'source <(psbox completions zsh)' >> ~/.zshrc")]
    Completions { shell: Shell },
    /// Run a supervisor (started by its systemd socket unit).
    #[command(hide = true)]
    Supervise {
        /// The supervisor key: `<sandbox>`, `<sandbox>.instance-<name>`, or
        /// `<sandbox>.task-<task>`.
        key: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Shell {
    Bash,
    Zsh,
}

/// The command line, with the commands of the top-level help in groups.
fn command() -> clap::Command {
    let cmd = Cli::command();
    let width = GROUPS
        .iter()
        .flat_map(|(_, names)| names.iter())
        .map(|n| n.len())
        .max()
        .unwrap_or(0);
    let mut groups = String::new();
    for (heading, names) in GROUPS {
        groups.push_str(&format!("{}\n", ui::paint(ui::GREEN.bold(), format!("{heading}:"))));
        for name in *names {
            let about = cmd
                .find_subcommand(name)
                .and_then(|c| c.get_about())
                .map(|a| a.to_string())
                .unwrap_or_default();
            let about = about.trim_end_matches('.');
            groups.push_str(&format!(
                "  {}  {about}\n",
                ui::paint(ui::CYAN.bold(), format!("{name:<width$}"))
            ));
        }
        groups.push('\n');
    }
    cmd.help_template(format!(
        "{{about-with-newline}}\n{{usage-heading}} {{usage}}\n\n{groups}{}\n{{options}}{{after-help}}",
        ui::paint(ui::GREEN.bold(), "Options:")
    ))
    .color(color_arg())
}

/// `--color` from the command line, read ahead of parsing so help and usage
/// errors follow it too.
fn color_arg() -> clap::ColorChoice {
    let args: Vec<String> = std::env::args().skip(1).take_while(|a| a != "--").collect();
    let value = args
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, a)| match a.strip_prefix("--color") {
            Some(v) if v.starts_with('=') => Some(v[1..].to_string()),
            Some("") => args.get(i + 1).cloned(),
            _ => None,
        });
    match value.as_deref() {
        Some("always") => clap::ColorChoice::Always,
        Some("never") => clap::ColorChoice::Never,
        _ => clap::ColorChoice::Auto,
    }
}

/// Configured sandbox names, for completion.
fn sandbox_names() -> Vec<CompletionCandidate> {
    let Ok(host) = plan::HostEnv::from_process() else {
        return Vec::new();
    };
    spec::Spec::names(&host.config_dir)
        .unwrap_or_default()
        .into_iter()
        .map(CompletionCandidate::new)
        .collect()
}

/// Runs the command line and returns the exit status.
pub fn main() -> ExitCode {
    clap_complete::CompleteEnv::with_factory(command).complete();
    let cli = match Cli::from_arg_matches(&command().get_matches()) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };
    let choice = match cli.color {
        clap::ColorChoice::Auto => anstream::ColorChoice::Auto,
        clap::ColorChoice::Always => anstream::ColorChoice::Always,
        clap::ColorChoice::Never => anstream::ColorChoice::Never,
    };
    choice.write_global();
    match dispatch(cli.command) {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(e) => {
            ui::error(&e);
            ExitCode::from(1)
        }
    }
}

fn dispatch(cmd: Cmd) -> Result<i32> {
    match cmd {
        Cmd::Run {
            target,
            command,
            exec,
            create_dirs,
            args,
        } => client::run(&target.identity(), command, args, exec, create_dirs),
        Cmd::Task {
            sandbox,
            task,
            create_dirs,
            args,
        } => cli::task(&sandbox, &task, args, create_dirs),
        Cmd::Sandbox {
            options,
            create_dirs,
            command,
        } => {
            let host = plan::HostEnv::from_process()?;
            let settings = options.settings(&host, &std::env::current_dir()?)?;
            client::sandbox(&settings, command, create_dirs)
        }
        Cmd::List { json } => cli::list(json),
        Cmd::Status { target, json } => cli::status(&target.identity(), json),
        Cmd::Mount { target, host, at, ro } => cli::mount(&target.identity(), &host, at, ro),
        Cmd::Umount { target, at } => cli::umount(&target.identity(), at),
        Cmd::Stop { all: true, .. } => cli::stop_all(),
        Cmd::Stop { sandbox, instance, .. } => cli::stop(&plan::Identity::Instance {
            sandbox: sandbox.expect("required without --all"),
            instance,
        }),
        Cmd::Install { dry_run, verbose } => cli::install(dry_run, verbose),
        Cmd::Devices { all, json } => cli::devices(all, json),
        Cmd::Check { sandbox: None } => cli::check(),
        Cmd::Check { sandbox: Some(name) } => cli::check_sandbox(&name),
        Cmd::Completions { shell } => cli::completions(match shell {
            Shell::Bash => &clap_complete::env::Bash,
            Shell::Zsh => &clap_complete::env::Zsh,
        }),
        Cmd::Supervise { key } => {
            supervisor::run(&plan::Identity::from_key(&key)?)?;
            Ok(0)
        }
    }
}
