//! Evaluates sandbox configs, which are Jsonnet.
//!
//! A config is the sum of layers: psbox's `base`, then profiles, the config
//! file, and `psbox sandbox -c` settings, composed by the psbox library
//! (`assets/psbox.libsonnet`) that `import 'psbox'` returns. Each task's
//! config is the composed config with the task's `overrides` added, also
//! inside Jsonnet, so `+:` in overrides merges as written. The results are
//! manifested as JSON for [`crate::spec`] to validate.
//!
//! Imports resolve relative to the importing file. Every file read is
//! opened once, checked to be a regular file outside the places the sandbox
//! can write (see [`Input::forbidden`]), and read through that descriptor.

use std::collections::{BTreeMap, HashMap};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use rsjsonnet_lang::arena::Arena;
use rsjsonnet_lang::interner::InternedStr;
use rsjsonnet_lang::program::{
    AnalyzeError, Callbacks, EvalError, EvalErrorKind, EvalStackTraceItem, ImportError, LoadError, NativeError,
    Program, Thunk, Value,
};
use rsjsonnet_lang::span::{SpanContextId, SpanId};

use crate::spec::ConfigProblem;

/// The library `import 'psbox'` returns, a function of the context.
const LIBRARY: &str = include_str!("assets/psbox.libsonnet");

/// The prefix of the import names psbox's own sources use for the library
/// function, the layers, and the composed config. Only psbox's own glue
/// sources can import them.
const INTERNAL: &str = "<psbox internal>/";
const LIBRARY_FN: &str = "<psbox internal>/library";
const COMPOSED: &str = "<psbox internal>/composed";

/// Built-in profiles, by name: `psbox.profiles.<name>` in Jsonnet.
pub const BUILTIN_PROFILES: [(&str, &str); 1] = [("browser", include_str!("assets/profiles/browser.libsonnet"))];

/// The import path of a built-in profile.
fn builtin_path(name: &str) -> String {
    format!("psbox/profiles/{name}.libsonnet")
}

/// One layer of a config, added in order on top of `base`.
#[derive(Debug, Clone)]
pub enum Layer {
    /// A Jsonnet file.
    File(PathBuf),
    /// A built-in profile, by name.
    Builtin(String),
    /// Jsonnet source psbox wrote, such as the object a `-c` setting makes.
    Snippet { name: String, text: String },
}

/// What to evaluate.
pub struct Input<'a> {
    pub layers: Vec<Layer>,
    /// The sandbox name; `None` for an unnamed sandbox.
    pub name: Option<&'a str>,
    pub home: &'a Path,
    pub root: &'a Path,
    /// Directories (and files) no loaded file may be inside: the places a
    /// sandbox can write, known before evaluating.
    pub forbidden: Vec<PathBuf>,
    /// Told of each file before it is read, so a watcher can watch it first.
    pub before_read: Option<&'a (dyn Fn(&Path) + Sync)>,
}

/// A file evaluation read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedFile {
    /// As imported, made absolute.
    pub path: PathBuf,
    /// With every symlink resolved.
    pub resolved: PathBuf,
}

/// A successful evaluation.
#[derive(Debug, Clone)]
pub struct Evaluation {
    /// The composed config.
    pub config: serde_json::Value,
    /// Each task's config, by task name.
    pub tasks: BTreeMap<String, serde_json::Value>,
    /// Every file read.
    pub files: Vec<LoadedFile>,
    /// Imported paths that did not exist.
    pub missing: Vec<PathBuf>,
}

/// A failed evaluation: what went wrong, and the files involved so far, so
/// that a watcher can notice when they are fixed.
#[derive(Debug)]
pub struct Failure {
    pub problem: ConfigProblem,
    pub files: Vec<LoadedFile>,
    pub missing: Vec<PathBuf>,
}

/// Source text the program loaded, for locating errors.
struct Source {
    /// The file path, or a name such as `<psbox>`.
    name: String,
    /// The directory relative imports resolve against.
    dir: Option<PathBuf>,
    text: Vec<u8>,
    /// psbox's own code (the library and the glue that composes layers),
    /// which may import [`INTERNAL`] names. Errors in it are located at the
    /// config code that led to them.
    own: bool,
}

struct State<'p, 'a> {
    before_read: Option<&'a (dyn Fn(&Path) + Sync)>,
    sources: HashMap<SpanContextId, Source>,
    library: Option<Thunk<'p>>,
    /// Thunks psbox's own sources import by an [`INTERNAL`] name.
    internal: HashMap<String, Thunk<'p>>,
    cache: HashMap<PathBuf, Thunk<'p>>,
    files: Vec<LoadedFile>,
    missing: Vec<PathBuf>,
    forbidden: Vec<PathBuf>,
    /// Why the last import failed; `ImportError` carries nothing.
    import_errors: Vec<(String, String)>,
}

/// Opens `path` (following symlinks), requires a regular file outside every
/// `forbidden` path, and reads it through the descriptor that was checked.
fn read_checked(path: &Path, forbidden: &[PathBuf]) -> std::io::Result<Result<(PathBuf, Vec<u8>), String>> {
    let file = std::fs::File::options()
        .read(true)
        .custom_flags(libc::O_PATH)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.file_type().is_file() {
        return Ok(Err(format!("{} isn't a regular file", path.display())));
    }
    use std::os::fd::AsRawFd;
    let fd_path = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
    let resolved = std::fs::read_link(&fd_path)?;
    if let Some(dir) = forbidden.iter().find(|f| resolved.starts_with(f)) {
        return Ok(Err(format!(
            "{} is inside {}, which the sandbox can write",
            crate::ui::tilde(&resolved),
            crate::ui::tilde(dir)
        )));
    }
    let text = std::fs::read(&fd_path)?;
    Ok(Ok((resolved, text)))
}

impl<'p> State<'p, '_> {
    fn load(
        &mut self,
        program: &mut Program<'p>,
        name: String,
        dir: Option<PathBuf>,
        text: Vec<u8>,
        own: bool,
    ) -> Result<Thunk<'p>, LoadError> {
        let (ctx, _) = program.span_manager_mut().insert_source_context(text.len());
        let thunk = program.load_source(ctx, &text, true, &name);
        self.sources.insert(ctx, Source { name, dir, text, own });
        thunk
    }

    /// Reads a file for an import, recording it.
    fn read(&mut self, path: &Path) -> Result<(PathBuf, Vec<u8>), String> {
        if let Some(f) = self.before_read {
            f(path);
        }
        match read_checked(path, &self.forbidden) {
            Ok(Ok((resolved, text))) => {
                let file = LoadedFile {
                    path: path.to_path_buf(),
                    resolved: resolved.clone(),
                };
                if !self.files.contains(&file) {
                    self.files.push(file);
                }
                Ok((resolved, text))
            }
            Ok(Err(why)) => {
                self.files.push(LoadedFile {
                    path: path.to_path_buf(),
                    resolved: path.to_path_buf(),
                });
                Err(why)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.missing.push(path.to_path_buf());
                Err(format!("{} doesn't exist", crate::ui::tilde(path)))
            }
            Err(e) => Err(format!("can't read {}: {e}", crate::ui::tilde(path))),
        }
    }

    /// The absolute path an import from the source at `from` names.
    fn resolve(&self, program: &Program<'p>, from: SpanId, path: &str) -> Result<PathBuf, String> {
        let p = Path::new(path);
        if p.is_absolute() {
            return Ok(p.to_path_buf());
        }
        let (ctx, _, _) = program.span_manager().get_span(from);
        match self.sources.get(&ctx).and_then(|s| s.dir.as_ref()) {
            Some(dir) => Ok(dir.join(p)),
            None => Err(format!(
                "{path:?} is relative, and psbox's own sources import only by absolute path"
            )),
        }
    }

    fn fail<T>(&mut self, path: &str, why: String) -> Result<T, ImportError> {
        self.import_errors.push((path.to_string(), why));
        Err(ImportError)
    }
}

impl<'p> Callbacks<'p> for State<'p, '_> {
    fn import(&mut self, program: &mut Program<'p>, from: SpanId, path: &str) -> Result<Thunk<'p>, ImportError> {
        if path == "psbox" {
            return Ok(self.library.clone().expect("the library is loaded before evaluating"));
        }
        if path.starts_with(INTERNAL) {
            let (ctx, _, _) = program.span_manager().get_span(from);
            let own = self.sources.get(&ctx).is_some_and(|s| s.own);
            return match self.internal.get(path) {
                Some(thunk) if own => Ok(thunk.clone()),
                _ => self.fail(path, "is psbox's own".into()),
            };
        }
        if let Some((name, text)) = BUILTIN_PROFILES.iter().find(|(n, _)| builtin_path(n) == path) {
            return match self.load(
                program,
                format!("<psbox profile {name}>"),
                None,
                text.as_bytes().to_vec(),
                false,
            ) {
                Ok(thunk) => Ok(thunk),
                Err(e) => {
                    let why = format!("{e:?}");
                    self.fail(path, why)
                }
            };
        }
        let file = match self.resolve(program, from, path) {
            Ok(file) => file,
            Err(why) => return self.fail(path, why),
        };
        let (resolved, text) = match self.read(&file) {
            Ok(r) => r,
            Err(why) => return self.fail(path, why),
        };
        if let Some(thunk) = self.cache.get(&resolved) {
            return Ok(thunk.clone());
        }
        let dir = file.parent().map(Path::to_path_buf);
        match self.load(program, file.display().to_string(), dir, text, false) {
            Ok(thunk) => {
                self.cache.insert(resolved, thunk.clone());
                Ok(thunk)
            }
            Err(e) => {
                let why = describe_load(program, &self.sources, &e);
                self.fail(path, why)
            }
        }
    }

    fn import_str(&mut self, program: &mut Program<'p>, from: SpanId, path: &str) -> Result<String, ImportError> {
        let bytes = self.import_bin(program, from, path)?;
        match String::from_utf8(bytes) {
            Ok(s) => Ok(s),
            Err(_) => self.fail(path, "isn't UTF-8 text".into()),
        }
    }

    fn import_bin(&mut self, program: &mut Program<'p>, from: SpanId, path: &str) -> Result<Vec<u8>, ImportError> {
        let file = match self.resolve(program, from, path) {
            Ok(file) => file,
            Err(why) => return self.fail(path, why),
        };
        match self.read(&file) {
            Ok((_, bytes)) => Ok(bytes),
            Err(why) => self.fail(path, why),
        }
    }

    fn trace(&mut self, _: &mut Program<'p>, message: &str, _: &[EvalStackTraceItem]) {
        crate::ui::warning(format!("std.trace: {message}"));
    }

    fn native_call(
        &mut self,
        _: &mut Program<'p>,
        _: InternedStr<'p>,
        _: &[Value<'p>],
    ) -> Result<Value<'p>, NativeError> {
        Err(NativeError)
    }
}

/// Where `span` is, as `file:line:column`, with the source line and a caret
/// under the span's start.
fn locate(program: &Program<'_>, sources: &HashMap<SpanContextId, Source>, span: SpanId) -> (String, Vec<String>) {
    let (ctx, start, _) = program.span_manager().get_span(span);
    let Some(source) = sources.get(&ctx) else {
        return ("<std>".into(), Vec::new());
    };
    let before = &source.text[..start.min(source.text.len())];
    let line = before.iter().filter(|b| **b == b'\n').count() + 1;
    let line_start = before.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let column = String::from_utf8_lossy(&before[line_start..]).chars().count() + 1;
    let line_end = source.text[line_start..]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(source.text.len(), |i| line_start + i);
    let code = String::from_utf8_lossy(&source.text[line_start..line_end]).into_owned();
    let gutter = format!("{line} ");
    let excerpt = vec![
        format!("{gutter}|{code}"),
        format!("{}|{}^", " ".repeat(gutter.len()), " ".repeat(column - 1)),
    ];
    (format!("{}:{line}:{column}", crate::ui::tilde(&source.name)), excerpt)
}

fn describe_load(program: &Program<'_>, sources: &HashMap<SpanContextId, Source>, e: &LoadError) -> String {
    let (span, message) = match e {
        LoadError::Lex(e) => (None, format!("{e:?}")),
        LoadError::Parse(e) => (None, format!("{e:?}")),
        LoadError::Analyze(AnalyzeError::UnknownVariable { span, name }) => {
            (Some(*span), format!("unknown variable {name}"))
        }
        LoadError::Analyze(e) => (None, format!("{e:?}")),
    };
    match span {
        Some(span) => format!("{}: {message}", locate(program, sources, span).0),
        None => message,
    }
}

/// The span and message of an evaluation error.
fn describe_eval(kind: &EvalErrorKind) -> (Option<SpanId>, String) {
    use EvalErrorKind as K;
    match kind {
        K::ExplicitError { span, message } => (Some(*span), message.clone()),
        K::AssertFailed { span, message } => (
            Some(*span),
            message.clone().unwrap_or_else(|| "assertion failed".into()),
        ),
        K::UnknownObjectField { span, field_name } => (Some(*span), format!("field {field_name:?} doesn't exist")),
        K::ImportFailed { span, path } => (Some(*span), format!("can't import {path:?}")),
        K::StackOverflow | K::InfiniteRecursion => (None, "the config recurses too deeply".into()),
        K::Other { span, message } => (*span, message.clone()),
        other => (None, format!("{other:?}")),
    }
}

/// An evaluation error as a config problem, located at the innermost span.
fn eval_problem(program: &Program<'_>, state: &State<'_, '_>, e: &EvalError) -> ConfigProblem {
    let (span, mut message) = describe_eval(&e.kind);
    if let EvalErrorKind::ImportFailed { path, .. } = &e.kind
        && let Some((_, why)) = state.import_errors.iter().rev().find(|(p, _)| p == path)
    {
        message = format!("can't import {path:?}: {why}");
    }
    // The innermost span in config code: an error raised in psbox's own
    // code is shown where the config called it, or without a location.
    let traced = e.stack_trace.iter().filter_map(|item| match item {
        EvalStackTraceItem::Expr { span } | EvalStackTraceItem::Variable { span, .. } => Some(*span),
        EvalStackTraceItem::Call { span, .. }
        | EvalStackTraceItem::ArrayItem { span, .. }
        | EvalStackTraceItem::ObjectField { span, .. } => *span,
        _ => None,
    });
    let span = span.into_iter().chain(traced).find(|span| {
        let (ctx, _, _) = program.span_manager().get_span(*span);
        state.sources.get(&ctx).is_some_and(|s| !s.own)
    });
    match span {
        Some(span) => {
            let (location, excerpt) = locate(program, &state.sources, span);
            ConfigProblem::Parse {
                location: Some(location),
                message,
                excerpt,
            }
        }
        None => ConfigProblem::Parse {
            location: None,
            message,
            excerpt: Vec::new(),
        },
    }
}

fn load_problem(program: &Program<'_>, state: &State<'_, '_>, e: &LoadError) -> ConfigProblem {
    let message = describe_load(program, &state.sources, e);
    match message.split_once(": ") {
        Some((location, rest)) if location.contains(':') => ConfigProblem::Parse {
            location: Some(location.into()),
            message: rest.into(),
            excerpt: Vec::new(),
        },
        _ => ConfigProblem::Parse {
            location: None,
            message,
            excerpt: Vec::new(),
        },
    }
}

/// The stack evaluation runs on. The parser recurses once per level of
/// nesting with no limit of its own, so it gets far more than a thread's
/// default; the memory is only reserved, not used.
const STACK: usize = 1 << 30;

/// Evaluates `input`, on a thread with a [`STACK`] of its own.
pub fn evaluate(input: &Input<'_>) -> Result<Evaluation, Failure> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("jsonnet".into())
            .stack_size(STACK)
            .spawn_scoped(scope, || evaluate_here(input))
            .map_err(|e| Failure {
                problem: ConfigProblem::Parse {
                    location: None,
                    message: format!("can't start evaluating: {e}"),
                    excerpt: Vec::new(),
                },
                files: Vec::new(),
                missing: Vec::new(),
            })?
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

fn evaluate_here(input: &Input<'_>) -> Result<Evaluation, Failure> {
    let arena = Arena::new();
    let mut program = Program::new(&arena);
    let mut state = State {
        before_read: input.before_read,
        sources: HashMap::new(),
        library: None,
        internal: HashMap::new(),
        cache: HashMap::new(),
        files: Vec::new(),
        missing: Vec::new(),
        forbidden: input.forbidden.clone(),
        import_errors: Vec::new(),
    };
    let result = run(&mut program, &mut state, input);
    result.map_err(|problem| Failure {
        problem,
        files: state.files.clone(),
        missing: state.missing.clone(),
    })
}

fn run<'p>(
    program: &mut Program<'p>,
    state: &mut State<'p, '_>,
    input: &Input<'_>,
) -> Result<Evaluation, ConfigProblem> {
    macro_rules! load {
        ($name:expr, $dir:expr, $text:expr) => {
            load!($name, $dir, $text, false)
        };
        ($name:expr, $dir:expr, $text:expr, $own:expr) => {
            match state.load(program, $name, $dir, $text, $own) {
                Ok(thunk) => thunk,
                Err(e) => return Err(load_problem(program, state, &e)),
            }
        };
    }
    macro_rules! eval {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(e) => return Err(eval_problem(program, state, &e)),
            }
        };
    }

    // Rust gets values from rsjsonnet only fully evaluated, so the library
    // and the layers stay unevaluated thunks that one Jsonnet program
    // composes: forcing a layer on its own would fail on its `super`
    // references, and forcing the library on its fields that are errors.
    let context = serde_json::json!({
        "name": input.name,
        "home": input.home.to_string_lossy(),
        "root": input.root.to_string_lossy(),
    });
    let library_fn = load!("<psbox>".into(), None, LIBRARY.as_bytes().to_vec(), true);
    state.internal.insert(LIBRARY_FN.into(), library_fn);
    let library = load!(
        "<psbox context>".into(),
        None,
        format!("(import '{LIBRARY_FN}')({context})").into_bytes(),
        true
    );
    state.library = Some(library);

    let mut names = Vec::new();
    for (i, layer) in input.layers.iter().enumerate() {
        let thunk = match layer {
            Layer::File(path) => {
                let (resolved, text) = match state.read(path) {
                    Ok(r) => r,
                    Err(why) => {
                        return Err(ConfigProblem::Parse {
                            location: None,
                            message: why,
                            excerpt: Vec::new(),
                        });
                    }
                };
                let thunk = load!(path.display().to_string(), path.parent().map(Path::to_path_buf), text);
                state.cache.insert(resolved, thunk.clone());
                thunk
            }
            Layer::Builtin(name) => {
                let Some((_, text)) = BUILTIN_PROFILES.iter().find(|(n, _)| n == name) else {
                    return Err(ConfigProblem::Parse {
                        location: None,
                        message: format!("no profile named {name}"),
                        excerpt: Vec::new(),
                    });
                };
                load!(format!("<psbox profile {name}>"), None, text.as_bytes().to_vec())
            }
            Layer::Snippet { name, text } => load!(name.clone(), None, text.as_bytes().to_vec()),
        };
        let name = format!("{INTERNAL}layer-{i}");
        state.internal.insert(name.clone(), thunk);
        names.push(format!("import '{name}'"));
    }

    let composed = load!(
        "<psbox compose>".into(),
        None,
        format!("(import 'psbox').compose([{}])", names.join(", ")).into_bytes(),
        true
    );
    let value = eval!(program.eval_value(&composed, state));
    let config = manifest(program, state, &value)?;
    state.internal.insert(COMPOSED.into(), composed);

    let mut tasks = BTreeMap::new();
    let names: Vec<String> = match config.get("task") {
        Some(serde_json::Value::Object(t)) => t
            .iter()
            .filter(|(_, v)| v.is_object())
            .map(|(k, _)| k.clone())
            .collect(),
        _ => Vec::new(),
    };
    for name in names {
        let source = format!(
            "(import 'psbox').forTask(import '{COMPOSED}', {})",
            serde_json::Value::String(name.clone())
        );
        let task = load!(format!("<psbox task {name}>"), None, source.into_bytes(), true);
        let value = eval!(program.eval_value(&task, state));
        tasks.insert(name, manifest(program, state, &value)?);
    }

    Ok(Evaluation {
        config,
        tasks,
        files: state.files.clone(),
        missing: state.missing.clone(),
    })
}

fn manifest<'p>(
    program: &mut Program<'p>,
    state: &mut State<'p, '_>,
    value: &Value<'p>,
) -> Result<serde_json::Value, ConfigProblem> {
    let text = match program.manifest_json(value, false) {
        Ok(text) => text,
        Err(e) => return Err(eval_problem(program, state, &e)),
    };
    serde_json::from_str(&text).map_err(|e| ConfigProblem::Parse {
        location: None,
        message: format!("the config isn't valid JSON: {e}"),
        excerpt: Vec::new(),
    })
}

/// Fails when a loaded file is inside any of `writable`, the places the
/// evaluated config lets the sandbox (or its tasks) write.
pub fn check_files(files: &[LoadedFile], writable: &[PathBuf]) -> Result<(), String> {
    for f in files {
        if let Some(dir) = writable.iter().find(|w| f.resolved.starts_with(w)) {
            return Err(format!(
                "{} is inside {}, which the sandbox can write",
                crate::ui::tilde(&f.resolved),
                crate::ui::tilde(dir)
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(layers: Vec<Layer>) -> Input<'static> {
        Input {
            layers,
            name: Some("app"),
            home: Path::new("/home/u"),
            root: Path::new("/home/u/Sandbox"),
            forbidden: Vec::new(),
            before_read: None,
        }
    }

    fn snippet(text: &str) -> Layer {
        Layer::Snippet {
            name: "<test>".into(),
            text: text.into(),
        }
    }

    fn eval(text: &str) -> Result<serde_json::Value, String> {
        evaluate(&input(vec![snippet(text)]))
            .map(|e| e.config)
            .map_err(|f| format!("{:?}", f.problem))
    }

    #[test]
    fn composes_base_and_layers() {
        let c = eval("{ dbus+: { mode: 'host' }, mount+: { a: { source: '/a' } } }").unwrap();
        assert_eq!(c["dbus"]["mode"], "host");
        assert_eq!(c["mount"]["a"]["source"], "/a");
        assert_eq!(c["display"], serde_json::json!({}));
        let c = eval("{ dbus: null, display: null }").unwrap();
        assert!(c["dbus"].is_null() && c["display"].is_null());
    }

    #[test]
    fn library_paths_and_helpers() {
        let c = eval(
            "local psbox = import 'psbox';
             { environment+: {
                 A: psbox.path([psbox.home, '.bin', 'AppRun']),
                 B: psbox.path(['/foo/', '/.bin/']),
                 C: psbox.path(['/foo//bar', '', 'baz/', '.']),
                 D: psbox.path(['/']),
                 E: psbox.storageFor('bin'),
                 F: psbox.storage,
               } }",
        )
        .unwrap();
        let env = &c["environment"];
        assert_eq!(env["A"], "/home/u/.bin/AppRun");
        assert_eq!(env["B"], "/foo/.bin");
        assert_eq!(env["C"], "/foo/bar/baz");
        assert_eq!(env["D"], "/");
        assert_eq!(env["E"], "/home/u/Sandbox/app.bin");
        assert_eq!(env["F"], "/home/u/Sandbox/app");
        assert!(eval("local psbox = import 'psbox'; { environment+: { A: psbox.path(['/a', '..']) } }").is_err());
        assert!(eval("local psbox = import 'psbox'; { environment+: { A: psbox.storageFor('a/b') } }").is_err());
        let c = eval(
            "local psbox = import 'psbox';
             { mount+: { g: psbox.mount.readOnly('/g', create=true), e: psbox.mount.empty('/e') },
               devices: psbox.devices.none + { camera: true } }",
        )
        .unwrap();
        assert_eq!(c["mount"]["g"]["mode"], "read-only");
        assert_eq!(c["mount"]["g"]["create"], true);
        assert_eq!(
            c["mount"]["e"],
            serde_json::json!({ "destination": "/e", "mode": "empty" })
        );
        assert_eq!(c["devices"]["camera"], true);
        assert_eq!(c["devices"]["gpu"], false);
    }

    #[test]
    fn hidden_fields_are_errors() {
        let e = eval("{ mount+:: { a: { source: '/a' } } }").unwrap_err();
        assert!(e.contains("hidden fields never reach psbox: mount"), "{e}");
        let e = eval("{ mount+: { a: { source: '/a', mode:: 'read-only' } } }").unwrap_err();
        assert!(e.contains("mount.a.mode"), "{e}");
    }

    #[test]
    fn tasks_apply_overrides_inside_jsonnet() {
        let e = evaluate(&input(vec![snippet(
            "{ mount+: { bin: { source: '/b', mode: 'overlay' }, keep: { source: '/k' } },
               task+: { update: { run: ['u'], overrides: { mount+: { bin+: { mode: 'read-write' } }, display: null } } } }",
        )]))
        .unwrap();
        let t = &e.tasks["update"];
        assert_eq!(t["mount"]["bin"]["mode"], "read-write");
        assert_eq!(t["mount"]["bin"]["source"], "/b");
        assert_eq!(t["mount"]["keep"]["source"], "/k");
        assert!(t["display"].is_null());
        assert_eq!(e.config["mount"]["bin"]["mode"], "overlay");
    }

    #[test]
    fn layers_see_what_came_before_them() {
        let e = evaluate(&input(vec![
            snippet("{ dbus+: { talk: ['a'] } }"),
            snippet("{ dbus+: { talk: super.talk + ['b'] } }"),
        ]))
        .unwrap();
        assert_eq!(e.config["dbus"]["talk"], serde_json::json!(["a", "b"]));
        let mut unnamed = input(vec![snippet("{ home: (import 'psbox').defaults.home }")]);
        unnamed.name = None;
        assert_eq!(evaluate(&unnamed).unwrap().config["home"]["mode"], "temporary");
        assert!(eval("import '<psbox internal>/library'").is_err());
    }

    #[test]
    fn errors_point_at_the_config() {
        let at = |text: &str| match evaluate(&input(vec![snippet(text)])).unwrap_err().problem {
            ConfigProblem::Parse { location, message, .. } => (location, message),
            other => panic!("{other:?}"),
        };
        let (location, message) = at("local psbox = import 'psbox'; { environment+: { A: psbox.storageFor('a/b') } }");
        assert_eq!(location.as_deref(), Some("<test>:1:52"), "{message}");
        let (location, message) = at("{ x:: 1 }");
        assert_eq!(
            (location, message.as_str()),
            (None, "hidden fields never reach psbox: x")
        );
    }

    #[test]
    fn deep_nesting_is_an_error() {
        let depth = 5000;
        let text = format!("{{ y: {}1{} }}", "{a:".repeat(depth), "}".repeat(depth));
        let problem = evaluate(&input(vec![snippet(&text)])).unwrap_err().problem;
        assert!(format!("{problem:?}").contains("recurses too deeply"), "{problem:?}");
    }

    #[test]
    fn builtin_profile() {
        let e = evaluate(&input(vec![Layer::Builtin("browser".into())])).unwrap();
        assert_eq!(e.config["devices"]["camera"], true);
        assert_eq!(e.config["links"]["http"], "sandbox");
        assert_eq!(e.config["mount"]["downloads"]["source"], "/home/u/Downloads");
        let c = eval("(import 'psbox').profiles.browser + { network: null }").unwrap();
        assert_eq!(c["devices"]["security-key"], true);
    }

    #[test]
    fn imports_resolve_relative_and_are_checked() {
        let dir = std::env::temp_dir().join(format!("psbox-jsonnet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("profiles")).unwrap();
        std::fs::create_dir_all(dir.join("writable")).unwrap();
        std::fs::write(
            dir.join("profiles/p.libsonnet"),
            "{ environment+: { P: importstr 'note.txt' } }",
        )
        .unwrap();
        std::fs::write(dir.join("profiles/note.txt"), "hi").unwrap();
        std::fs::write(dir.join("writable/w.libsonnet"), "{}").unwrap();
        let main = dir.join("main.jsonnet");
        std::fs::write(&main, "(import 'profiles/p.libsonnet') + { network: null }").unwrap();

        let e = evaluate(&input(vec![Layer::File(main.clone())])).unwrap();
        assert_eq!(e.config["environment"]["P"], "hi");
        assert_eq!(e.files.len(), 3);
        assert!(check_files(&e.files, &[dir.join("writable")]).is_ok());
        assert!(check_files(&e.files, &[dir.join("profiles")]).is_err());

        std::fs::write(&main, "import 'writable/w.libsonnet'").unwrap();
        let mut i = input(vec![Layer::File(main.clone())]);
        i.forbidden = vec![dir.join("writable")];
        let f = evaluate(&i).unwrap_err();
        assert!(format!("{:?}", f.problem).contains("which the sandbox can write"));

        std::fs::write(&main, "import 'missing/x.libsonnet'").unwrap();
        let f = evaluate(&input(vec![Layer::File(main.clone())])).unwrap_err();
        assert_eq!(f.missing, vec![dir.join("missing/x.libsonnet")]);

        std::fs::write(&main, "{ a: }").unwrap();
        let f = evaluate(&input(vec![Layer::File(main)])).unwrap_err();
        assert!(matches!(f.problem, ConfigProblem::Parse { .. }));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn errors_are_located() {
        let ConfigProblem::Parse {
            location,
            message,
            excerpt,
        } = evaluate(&input(vec![snippet("{\n  a: error 'boom',\n}")]))
            .unwrap_err()
            .problem
        else {
            panic!("a parse problem")
        };
        assert_eq!(message, "boom");
        assert_eq!(location.as_deref(), Some("<test>:2:6"));
        assert_eq!(excerpt, ["2 |  a: error 'boom',", "  |     ^"]);
    }
}
