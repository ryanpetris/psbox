//! Terminal output: styles, tables, and the `✗` error, `!` warning, and
//! `→` fix lines.
//!
//! Everything goes through `anstream`, which drops the styles when the
//! stream is not a terminal, `NO_COLOR` is set, or `--color never` was given.

use std::fmt;
use std::io::IsTerminal;
use std::path::Path;

use anstyle::{AnsiColor, Style};

pub const BOLD: Style = Style::new().bold();
pub const DIM: Style = Style::new().dimmed();
pub const HEADER: Style = Style::new().bold().dimmed();
pub const GREEN: Style = AnsiColor::Green.on_default();
pub const YELLOW: Style = AnsiColor::Yellow.on_default();
pub const RED: Style = AnsiColor::Red.on_default();
pub const CYAN: Style = AnsiColor::Cyan.on_default();

/// Text in a style.
pub fn paint(style: Style, text: impl fmt::Display) -> String {
    format!("{style}{text}{style:#}")
}

/// `path` with the home directory written as `~`.
pub fn tilde(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty())
        && let Ok(rest) = path.strip_prefix(&home)
    {
        if rest.as_os_str().is_empty() {
            return "~".into();
        }
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

/// An error with what to do about it, shown on a `→` line after the error.
/// Only for fixes that the message does not already make obvious.
#[derive(Debug)]
pub struct Fixed {
    pub error: anyhow::Error,
    pub fix: String,
}

impl fmt::Display for Fixed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#}", self.error)
    }
}

impl std::error::Error for Fixed {}

/// Attaches what to do about an error.
pub fn with_fix(error: impl Into<anyhow::Error>, fix: impl Into<String>) -> anyhow::Error {
    Fixed {
        error: error.into(),
        fix: fix.into(),
    }
    .into()
}

/// Prints an error: `✗` and the message, then any fixes attached to it.
/// Config errors and startup reports print their own sections.
pub fn error(e: &anyhow::Error) {
    if let Some(r) = e.downcast_ref::<crate::plan::Report>() {
        anstream::eprint!("{}", r.render());
        return;
    }
    if let Some(c) = e.downcast_ref::<crate::spec::ConfigError>() {
        anstream::eprint!("{}", c.render());
        return;
    }
    anstream::eprintln!("{}", failure(format!("{e:#}")));
    for c in e.chain() {
        if let Some(f) = c.downcast_ref::<Fixed>() {
            fix(&f.fix);
        }
    }
}

/// Prints a warning: something worth fixing that did not stop anything.
pub fn warning(message: impl fmt::Display) {
    anstream::eprintln!("{} {message}", paint(YELLOW, "!"));
}

/// Prints what to do about the error or warning above it.
pub fn fix(text: impl fmt::Display) {
    anstream::eprintln!("{}", fix_line(text));
}

/// A `→` line, dimmed except for the literals in it (see [`code`]).
pub fn fix_line(text: impl fmt::Display) -> String {
    let reset = anstyle::Reset.render().to_string();
    let text = text.to_string().replace(&reset, &format!("{reset}{}", DIM.render()));
    format!("  {}", paint(DIM, format!("→ {text}")))
}

/// `✗` and a message: the first line of an error.
pub fn failure(message: impl fmt::Display) -> String {
    format!("{} {message}", paint(RED, "✗"))
}

/// `text` without terminal styles, for logs and other plain output.
pub fn plain(text: &str) -> String {
    anstream::adapter::strip_str(text).to_string()
}

/// Confirms that something was done.
pub fn done(message: impl fmt::Display) {
    anstream::eprintln!("{} {message}", paint(GREEN, "✓"));
}

/// A command, path, or other literal in a message.
pub fn code(text: impl fmt::Display) -> String {
    paint(CYAN, text)
}

/// One table cell: text in one or more styles.
#[derive(Debug, Clone, Default)]
pub struct Cell {
    spans: Vec<(String, Style)>,
}

impl Cell {
    pub fn new(text: impl Into<String>) -> Self {
        Self::styled(Style::new(), text)
    }

    pub fn styled(style: Style, text: impl Into<String>) -> Self {
        Self {
            spans: vec![(text.into(), style)],
        }
    }

    /// Appends more text in another style.
    pub fn then(mut self, style: Style, text: impl Into<String>) -> Self {
        self.spans.push((text.into(), style));
        self
    }

    fn width(&self) -> usize {
        self.spans.iter().map(|(t, _)| t.chars().count()).sum()
    }

    /// The cell cut to `width` characters (the last one `…`), styled.
    fn render(&self, width: usize) -> String {
        let cut = self.width() > width;
        let mut room = if cut { width.saturating_sub(1) } else { width };
        let mut out = String::new();
        for (text, style) in &self.spans {
            let part: String = text.chars().take(room).collect();
            room -= part.chars().count();
            if !part.is_empty() {
                out.push_str(&paint(*style, part));
            }
        }
        if cut {
            out.push('…');
        }
        out
    }
}

impl From<&str> for Cell {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl From<String> for Cell {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

/// A borderless table: a dim header and columns separated by spaces. Rows
/// may start with a one-character marker (such as a state dot) in the
/// indent. On a terminal, the `flex` column is shortened (ending in `…`) so
/// rows fit the window.
pub struct Table {
    header: Option<Vec<String>>,
    columns: usize,
    rows: Vec<(Option<Cell>, Vec<Cell>)>,
    flex: Option<usize>,
}

const INDENT: usize = 2;
const GAP: usize = 3;
const MIN_FLEX: usize = 12;

impl Table {
    pub fn new(header: &[&str]) -> Self {
        Self {
            header: Some(header.iter().map(|h| h.to_string()).collect()),
            columns: header.len(),
            rows: Vec::new(),
            flex: None,
        }
    }

    /// A table without a header row.
    pub fn headless(columns: usize) -> Self {
        Self {
            header: None,
            columns,
            rows: Vec::new(),
            flex: None,
        }
    }

    /// The column to shorten when rows do not fit.
    pub fn flex(mut self, column: usize) -> Self {
        self.flex = Some(column);
        self
    }

    pub fn row(&mut self, cells: Vec<Cell>) {
        self.rows.push((None, cells));
    }

    /// A row with a marker before its first column.
    pub fn marked_row(&mut self, marker: Cell, cells: Vec<Cell>) {
        self.rows.push((Some(marker), cells));
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The table as lines, fitted to `width` when given.
    pub fn render(&self, width: Option<usize>) -> Vec<String> {
        let header: Option<Vec<Cell>> = self
            .header
            .as_ref()
            .map(|h| h.iter().map(|t| Cell::styled(HEADER, t.clone())).collect());
        let mut widths = vec![0; self.columns];
        for cells in header.iter().chain(self.rows.iter().map(|(_, c)| c)) {
            for (i, c) in cells.iter().enumerate().take(self.columns) {
                widths[i] = widths[i].max(c.width());
            }
        }
        if let (Some(width), Some(flex)) = (width, self.flex) {
            let total = INDENT + widths.iter().sum::<usize>() + GAP * (self.columns - 1);
            if total > width {
                widths[flex] = widths[flex].saturating_sub(total - width).max(MIN_FLEX);
            }
        }
        let line = |marker: Option<&Cell>, cells: &[Cell]| {
            let mut out = match marker {
                Some(m) => format!("{} ", m.render(1)),
                None => " ".repeat(INDENT),
            };
            for (i, w) in widths.iter().enumerate() {
                let empty = Cell::default();
                let cell = cells.get(i).unwrap_or(&empty);
                out.push_str(&cell.render(*w));
                if i + 1 < self.columns {
                    out.push_str(&" ".repeat(w.saturating_sub(cell.width().min(*w)) + GAP));
                }
            }
            out.trim_end().to_string()
        };
        let mut lines: Vec<String> = header.iter().map(|h| line(None, h)).collect();
        lines.extend(self.rows.iter().map(|(m, c)| line(m.as_ref(), c)));
        lines
    }

    /// Prints the table to stdout.
    pub fn print(&self) {
        for l in self.render(stdout_width()) {
            anstream::println!("{l}");
        }
    }
}

/// The terminal width when stdout is a terminal: `COLUMNS`, else the
/// window size.
pub fn stdout_width() -> Option<usize> {
    if !std::io::stdout().is_terminal() {
        return None;
    }
    if let Some(n) = std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()) {
        return Some(n);
    }
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: TIOCGWINSZ fills the local winsize.
    let r = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
    (r == 0 && ws.ws_col > 0).then_some(ws.ws_col as usize)
}

/// `n` of something, with the plural `s` when needed.
pub fn count(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

/// A duration as `2h 14m`, `5m 3s`, or `12s`.
pub fn duration(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, _) => format!("{m}m {s}s"),
        (0, _, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: Vec<String>) -> Vec<String> {
        lines
            .into_iter()
            .map(|l| {
                let mut out = String::new();
                let mut esc = false;
                for c in l.chars() {
                    match (esc, c) {
                        (false, '\x1b') => esc = true,
                        (true, 'm') => esc = false,
                        (true, _) => {}
                        (false, c) => out.push(c),
                    }
                }
                out
            })
            .collect()
    }

    #[test]
    fn tables_align_and_fit() {
        let mut t = Table::new(&["NAME", "STATE", "COMMANDS"]).flex(2);
        t.row(vec!["firefox".into(), Cell::styled(GREEN, "running"), "firefox".into()]);
        t.row(vec![
            "code".into(),
            "stopped".into(),
            "code, code-insiders, code-oss".into(),
        ]);
        assert_eq!(
            plain(t.render(None)),
            [
                "  NAME      STATE     COMMANDS",
                "  firefox   running   firefox",
                "  code      stopped   code, code-insiders, code-oss",
            ]
        );
        let fitted = plain(t.render(Some(40)));
        assert_eq!(fitted[2], "  code      stopped   code, code-inside…");
        assert!(fitted.iter().all(|l| l.chars().count() <= 40));
    }

    #[test]
    fn markers_and_mixed_styles() {
        let mut t = Table::headless(2);
        t.marked_row(
            Cell::styled(GREEN, "●"),
            vec![Cell::new("code"), Cell::styled(GREEN, "running").then(DIM, " · work")],
        );
        assert_eq!(plain(t.render(None)), ["● code   running · work"]);
        let mut cut = Table::headless(2).flex(1);
        cut.row(vec![
            Cell::new("code"),
            Cell::styled(GREEN, "running").then(DIM, " · default, work"),
        ]);
        assert_eq!(plain(cut.render(Some(21))), ["  code   running · d…"]);
    }

    #[test]
    fn short_rows_are_padded() {
        let mut t = Table::new(&["A", "B"]);
        t.row(vec!["x".into()]);
        assert_eq!(plain(t.render(None)), ["  A   B", "  x"]);
    }

    #[test]
    fn durations_and_counts() {
        assert_eq!(duration(12), "12s");
        assert_eq!(duration(303), "5m 3s");
        assert_eq!(duration(2 * 3600 + 14 * 60 + 5), "2h 14m");
        assert_eq!(duration(3 * 86400 + 3600), "3d 1h");
        assert_eq!(count(1, "entry"), "1 entry");
        assert_eq!(count(2, "client"), "2 clients");
    }
}
