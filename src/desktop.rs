//! Desktop entry files: parsing, `Exec` quoting, and the rewrite that makes
//! an entry launch through psbox.

use std::path::Path;

use anyhow::{Result, bail};

/// The first line of every file psbox writes. Files without it are never
/// changed or removed.
pub const GENERATOR_MARKER: &str = "# @generator psbox";

/// The prefix of the second line, which names the sandbox whose config the
/// file was generated from.
pub const SANDBOX_TAG: &str = "# @sandbox ";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    /// Comments and blank lines, kept verbatim.
    Other(String),
    Key(String, String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Section {
    name: String,
    lines: Vec<Line>,
}

/// A parsed desktop entry that keeps comments and ordering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    preamble: Vec<String>,
    sections: Vec<Section>,
}

pub const MAIN: &str = "Desktop Entry";

impl Entry {
    pub fn parse(text: &str) -> Result<Self> {
        let mut entry = Entry {
            preamble: Vec::new(),
            sections: Vec::new(),
        };
        for raw in text.lines() {
            let line = raw.trim();
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                entry.sections.push(Section {
                    name: name.to_string(),
                    lines: Vec::new(),
                });
                continue;
            }
            let Some(section) = entry.sections.last_mut() else {
                if !line.is_empty() && !line.starts_with('#') {
                    bail!("key outside any section: {line:?}");
                }
                if line != GENERATOR_MARKER && !line.starts_with(SANDBOX_TAG) {
                    entry.preamble.push(line.to_string());
                }
                continue;
            };
            if line.is_empty() || line.starts_with('#') {
                section.lines.push(Line::Other(line.to_string()));
            } else if let Some((k, v)) = line.split_once('=') {
                section
                    .lines
                    .push(Line::Key(k.trim().to_string(), v.trim_start().to_string()));
            } else {
                bail!("invalid line in [{}]: {line:?}", section.name);
            }
        }
        if !entry.sections.iter().any(|s| s.name == MAIN) {
            bail!("no [{MAIN}] section");
        }
        Ok(entry)
    }

    /// Renders the entry with the psbox generator marker first.
    /// The file psbox writes for `sandbox`: the marker lines, then the entry.
    pub fn render(&self, sandbox: &str) -> String {
        let mut out = format!("{GENERATOR_MARKER}\n{SANDBOX_TAG}{sandbox}\n");
        for l in &self.preamble {
            out.push_str(l);
            out.push('\n');
        }
        for s in &self.sections {
            out.push_str(&format!("[{}]\n", s.name));
            for l in &s.lines {
                match l {
                    Line::Other(t) => out.push_str(t),
                    Line::Key(k, v) => out.push_str(&format!("{k}={v}")),
                }
                out.push('\n');
            }
        }
        out
    }

    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.sections
            .iter()
            .find(|s| s.name == section)?
            .lines
            .iter()
            .find_map(|l| match l {
                Line::Key(k, v) if k == key => Some(v.as_str()),
                _ => None,
            })
    }

    /// Sets `key` in `section` (which must exist), replacing every value.
    pub fn set(&mut self, section: &str, key: &str, value: &str) {
        let Some(s) = self.sections.iter_mut().find(|s| s.name == section) else {
            return;
        };
        let mut done = false;
        s.lines.retain_mut(|l| match l {
            Line::Key(k, v) if k == key => {
                if done {
                    return false;
                }
                *v = value.to_string();
                done = true;
                true
            }
            _ => true,
        });
        if !done {
            // Insert after the last key so trailing blank lines stay last.
            let at = s
                .lines
                .iter()
                .rposition(|l| matches!(l, Line::Key(..)))
                .map_or(0, |i| i + 1);
            s.lines.insert(at, Line::Key(key.to_string(), value.to_string()));
        }
    }

    pub fn remove(&mut self, section: &str, key: &str) {
        if let Some(s) = self.sections.iter_mut().find(|s| s.name == section) {
            s.lines.retain(|l| !matches!(l, Line::Key(k, _) if k == key));
        }
    }

    /// Names of the main section and every `[Desktop Action ...]` section.
    pub fn exec_sections(&self) -> Vec<String> {
        self.sections
            .iter()
            .filter(|s| s.name == MAIN || s.name.starts_with("Desktop Action "))
            .map(|s| s.name.clone())
            .collect()
    }
}

/// Splits an `Exec` value into arguments, following the Desktop Entry
/// specification's quoting rules.
pub fn split_exec(value: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut have = false;
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                have = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(e @ ('"' | '`' | '$' | '\\')) => cur.push(e),
                            Some(other) => {
                                cur.push('\\');
                                cur.push(other);
                            }
                            None => bail!("unterminated escape in Exec"),
                        },
                        Some(other) => cur.push(other),
                        None => bail!("unterminated quote in Exec"),
                    }
                }
            }
            ' ' | '\t' => {
                if have {
                    args.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            other => {
                cur.push(other);
                have = true;
            }
        }
    }
    if have {
        args.push(cur);
    }
    Ok(args)
}

/// Joins arguments into an `Exec` value, quoting where needed.
pub fn join_exec(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            let needs = a.is_empty() || a.chars().any(|c| " \t\n\"'\\><~|&;$*?#()`".contains(c));
            if !needs {
                return a.clone();
            }
            let mut q = String::from("\"");
            for c in a.chars() {
                if matches!(c, '"' | '`' | '$' | '\\') {
                    q.push('\\');
                }
                q.push(c);
            }
            q.push('"');
            q
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Field codes like `%u`; never removed by `remove-args`.
fn is_field_code(arg: &str) -> bool {
    arg.len() == 2 && arg.starts_with('%')
}

/// The command line an `Exec` value runs inside the sandbox.
#[derive(Debug, PartialEq, Eq)]
pub enum Launch {
    /// A bare program name: run the sandbox command with these arguments.
    Command(Vec<String>),
    /// A path or an `env` prefix: run this exact command line.
    Exec(Vec<String>),
}

/// Classifies and filters an `Exec` value.
pub fn launch_for(exec: &str, remove_args: &[String]) -> Result<Launch> {
    let args = split_exec(exec)?;
    let Some(program) = args.first() else {
        bail!("empty Exec")
    };
    let keep = |a: &&String| is_field_code(a) || !remove_args.contains(a);
    if program == "env" || program.contains('/') {
        Ok(Launch::Exec(args.iter().filter(keep).cloned().collect()))
    } else {
        Ok(Launch::Command(args[1..].iter().filter(keep).cloned().collect()))
    }
}

/// How an entry starts its sandbox command.
pub struct Target<'a> {
    pub psbox: &'a Path,
    pub sandbox: &'a str,
    pub command: &'a str,
}

/// Rewrites `entry` so every `Exec` launches through psbox, and applies the
/// rules every installed entry needs: no `TryExec` (the program may only
/// exist inside the sandbox) and no D-Bus activation (which would start the
/// app on the host, outside the sandbox).
pub fn rewrite(entry: &mut Entry, target: &Target<'_>, remove_args: &[String]) -> Result<()> {
    for section in entry.exec_sections() {
        let Some(exec) = entry.get(&section, "Exec").map(str::to_string) else {
            continue;
        };
        let mut cmd = vec![
            target.psbox.to_string_lossy().into_owned(),
            "run".into(),
            target.sandbox.into(),
            "--command".into(),
            target.command.into(),
        ];
        match launch_for(&exec, remove_args)? {
            Launch::Command(args) => {
                cmd.push("--".into());
                cmd.extend(args);
            }
            Launch::Exec(args) => {
                cmd.push("--exec".into());
                cmd.push("--".into());
                cmd.extend(args);
            }
        }
        entry.set(&section, "Exec", &join_exec(&cmd));
        entry.remove(&section, "TryExec");
    }
    entry.set(MAIN, "DBusActivatable", "false");
    Ok(())
}

/// A minimal entry that hides a host autostart entry with the same id.
pub fn hidden(id: &str) -> Entry {
    let mut e = Entry {
        preamble: Vec::new(),
        sections: vec![Section {
            name: MAIN.into(),
            lines: Vec::new(),
        }],
    };
    e.set(MAIN, "Type", "Application");
    e.set(MAIN, "Name", id);
    e.set(MAIN, "Hidden", "true");
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIREFOX: &str = "# vendor comment\n[Desktop Entry]\nName=Firefox\nExec=firefox %u\nTryExec=firefox\nIcon=firefox\nDBusActivatable=true\n\n[Desktop Action new-window]\nName=New Window\nExec=firefox --new-window %u\n";

    fn target() -> Target<'static> {
        Target {
            psbox: Path::new("/usr/bin/psbox"),
            sandbox: "firefox",
            command: "firefox",
        }
    }

    #[test]
    fn parses_and_renders_with_marker() {
        let e = Entry::parse(FIREFOX).unwrap();
        assert_eq!(e.get(MAIN, "Name"), Some("Firefox"));
        let out = e.render("firefox");
        assert!(out.starts_with(&format!("{GENERATOR_MARKER}\n{SANDBOX_TAG}firefox\n")));
        assert!(out.contains("# vendor comment"));
        assert_eq!(Entry::parse(&out).unwrap(), e, "re-parsing drops the marker lines");
    }

    #[test]
    fn rewrites_exec_lines_and_rules() {
        let mut e = Entry::parse(FIREFOX).unwrap();
        rewrite(&mut e, &target(), &[]).unwrap();
        assert_eq!(
            e.get(MAIN, "Exec"),
            Some("/usr/bin/psbox run firefox --command firefox -- %u")
        );
        assert_eq!(
            e.get("Desktop Action new-window", "Exec"),
            Some("/usr/bin/psbox run firefox --command firefox -- --new-window %u")
        );
        assert_eq!(e.get(MAIN, "TryExec"), None);
        assert_eq!(e.get(MAIN, "DBusActivatable"), Some("false"));
    }

    #[test]
    fn paths_and_env_prefixes_run_exactly() {
        let l = launch_for("env FOO=\"a b\" /opt/app/run --detach %F", &["--detach".into()]).unwrap();
        assert_eq!(
            l,
            Launch::Exec(vec!["env".into(), "FOO=a b".into(), "/opt/app/run".into(), "%F".into()])
        );
        assert_eq!(
            launch_for("code --new-window %F", &["--new-window".into()]).unwrap(),
            Launch::Command(vec!["%F".into()])
        );
        assert_eq!(
            launch_for("app %u", &["%u".into()]).unwrap(),
            Launch::Command(vec!["%u".into()])
        );
    }

    #[test]
    fn exec_quoting_round_trips() {
        let args: Vec<String> = ["/usr/bin/psbox", "a b", "say \"hi\"", "$HOME", "plain", ""]
            .map(String::from)
            .to_vec();
        assert_eq!(split_exec(&join_exec(&args)).unwrap(), args);
        assert!(split_exec("\"open").is_err());
    }

    #[test]
    fn set_replaces_duplicates_and_appends() {
        let mut e = Entry::parse("[Desktop Entry]\nName=A\nName=B\n\n").unwrap();
        e.set(MAIN, "Name", "C");
        e.set(MAIN, "StartupWMClass", "c");
        assert_eq!(
            e.render("a"),
            format!(
                "{GENERATOR_MARKER}
{SANDBOX_TAG}a
[Desktop Entry]\nName=C\nStartupWMClass=c\n\n"
            )
        );
    }

    #[test]
    fn rejects_invalid_files() {
        assert!(Entry::parse("Name=x\n").is_err());
        assert!(Entry::parse("[Other]\nName=x\n").is_err());
        assert!(Entry::parse("[Desktop Entry]\nnot a key\n").is_err());
    }

    #[test]
    fn hidden_entries() {
        let h = hidden("steam-autostart");
        assert_eq!(h.get(MAIN, "Hidden"), Some("true"));
    }
}
