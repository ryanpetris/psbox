# Agent Guide

Orientation for agents and humans working on Petris Sandbox (`psbox`).

## What psbox is

Petris Sandbox (`psbox`) is one Linux-only Rust binary that runs
applications in private-home sandboxes, built directly on Linux namespaces,
the new mount API, Landlock, seccomp, and cgroups. The same binary is the
client and, as `psbox supervise <key>`, the socket-activated supervisor of
one sandbox. Configuration, the CLI, isolation, and the process model are in
[docs/](docs/index.md). The [glossary](docs/glossary.md) is canonical in
code, config, CLI output, and documentation. If code and the glossary
disagree, update the code rather than inventing a second meaning.

psbox targets the newest kernel interfaces (Linux 7.2) without fallbacks.
User-visible names, paths, units, and environment variables are `psbox`.

## Host modifications

Every change psbox makes to the host, outside the sandbox root it builds,
requires explicit approval from the user. This covers files, directories,
sockets (including abstract sockets in the host network namespace), units,
cgroups, and state held by host services. Do not add a new host
modification, or widen an approved one, without asking first. Prefer a
design that keeps the change inside the sandbox root.

Approved host modifications (paths shown for uid 1000 with home
`/home/user` and no `PSBOX_ROOT`, `XDG_DATA_HOME`, or
`XDG_CONFIG_HOME` set):

- Sandbox storage: `home = "private"` creates
  `/home/user/Sandbox/<name>` (for example
  `/home/user/Sandbox/firefox`), and `home = "<path>"` or
  `psbox sandbox --home <path>` creates that path if it is missing (for
  example `/home/user/work/client`).
- Runtime directory `/run/user/1000/psbox/`, containing:
  - socket-unit sockets such as `/run/user/1000/psbox/firefox.sock` and
    `/run/user/1000/psbox/retroarch.task-update.sock`;
  - the shared empty mount point `/run/user/1000/psbox/.root`;
  - the short-lived unnamed-sandbox settings file
    `/run/user/1000/psbox/<key>.toml`, written by `psbox sandbox` and
    removed by the supervisor after reading it.
- systemd user units: `make dev/install` writes
  `/home/user/.config/systemd/user/psbox@.service` and
  `/home/user/.config/systemd/user/psbox@.socket` and runs
  `systemctl --user daemon-reload`; psbox starts and stops units such as
  `psbox@firefox.socket` and `psbox@firefox.service` through the user
  manager.
- cgroups: `supervisor/`, `infra/`, and `workload/` inside each unit's
  delegated cgroup, for example
  `/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/app-psbox.slice/psbox@firefox.service/workload`.
- `psbox install`, only when run: tagged entries in
  `/home/user/.local/share/applications/` (for example
  `/home/user/.local/share/applications/com.libretro.RetroArch.desktop`)
  and `/home/user/.config/autostart/` (for example
  `/home/user/.config/autostart/steam-autostart.desktop`).
- Host portal registration: one `org.freedesktop.host.portal.Registry`
  `Register` call to the host `xdg-desktop-portal` on
  `/run/user/1000/bus` per sandbox client connection, held only for that
  connection.
- Mount points inside shares: a missing mount point beneath a host share
  is created on the host (for example `rw = ["~/Code"]` with a mount at
  `~/Code/sub` creates `/home/user/Code/sub`).
- Save dialogs: when an app saves through the file chooser portal to a
  new file the sandbox cannot already write, the supervisor creates that
  empty file at the path the user chose (for example
  `/home/user/Documents/report.pdf`) so it can be mounted into the
  sandbox. Existing files are never replaced or truncated.
- The config watcher creates `/home/user/.config/psbox/sandboxes` when
  it is missing.
- Directories marked with a trailing `/` in a sandbox config (`rw`, `ro`,
  a mount's `host`, `--bind`, `--ro-bind`), for mounts that are not
  optional: created when missing inside the sandbox root (for example
  `host = "@self.bin/"` creates `/home/user/Sandbox/retroarch.bin`),
  and elsewhere only when `psbox run`, `task`, or `sandbox` is given
  `--create-dirs` (for example `/home/user/Games/saves/`).

## Temporary pre-1.0 rules

These rules apply only while psbox remains below `v1.0.0`. If psbox has
reached `v1.0.0` and this section still exists, stop and tell the user before
making a change that relies on it.

- When replacing or removing behavior, delete the old implementation
  completely as if it never existed.
- Do not add compatibility shims, deprecated aliases, dual config paths,
  fallbacks, or regression tests that merely verify removed behavior is
  absent.
- Remove obsolete code, tests, config, documentation, and dependencies
  together.
- On-disk formats, CLI shapes, and configuration schemas may change
  directly when that produces the correct clean design.
- The client protocol has no version: change it directly. A sandbox still
  running an older psbox is reported as such, and `psbox stop` restarts it.

## Project structure

One crate: the library (`src/lib.rs`) is the whole program, and
`src/main.rs` only calls `psbox::main()`. The library exists so that the
fuzz targets in `fuzz/` (a separate crate with its own workspace) can reach
the parsers through `src/fuzz.rs`, which is compiled only with
`--cfg fuzzing`. Keep modules private (`mod`, not `pub mod`) so unused code
is reported.

| Area | Modules |
| --- | --- |
| Command line and output | `lib.rs`, `cli.rs`, `ui.rs` |
| Client | `client.rs`, `unnamed.rs`, `protocol.rs` |
| Configuration and plans | `spec.rs`, `plan.rs`, `assets/profiles/` |
| Supervisor | `supervisor.rs` |
| Sandbox root, spawning, and namespace joins | `root.rs`, `spawn.rs`, `forker.rs`, `inject.rs` |
| Kernel interfaces | `sys/` (namespaces, mount API, Landlock, seccomp, cgroups, inotify, fds, processes) |
| D-Bus | `bus.rs`, `gateway.rs`, `portal.rs`, `dbus/` (wire format and connections) |
| Devices, Xwayland, desktop entries, systemd | `devices.rs`, `x11.rs`, `desktop.rs`, `install.rs`, `systemd.rs` |
| Fuzz entry points | `fuzz.rs`, `fuzz/fuzz_targets/` |

Other top-level directories: `units/` (the `psbox@` user units),
`examples/` (sandbox configs), `scripts/` (headless GNOME test helpers),
`docs/`.

A new module belongs where its concern already lives; add one only for a
new concern, and do not create catch-all modules such as `utils`.

## Build, test, and release

Run commands from the repository root:

```sh
make build
make test
make check
make fmt
```

`make check` runs `cargo fmt --check`, clippy with `-D warnings` for both
release targets, the tests, and `cargo deny check`; it must pass before a
code change is complete. During development, run focused tests first:

```sh
cargo test plan::tests::network_defaults_to_isolated_when_pasta_is_installed
```

Report any check that could not run and why. Documentation-only changes may
use direct documentation and diff validation instead of the full suite.

Release binaries are static musl builds for `x86_64-unknown-linux-musl` and
`aarch64-unknown-linux-musl` (`make release`, `make dist`), linked with
Rust's `rust-lld`. Do not add a C dependency or a build step that needs a C
toolchain unless the user explicitly approves changing that property.
Archives, workflows, and packaging are in [docs/release.md](docs/release.md).

Changes to a parser of input psbox does not control (config, client
protocol, D-Bus messages, desktop entries) should be fuzzed: `make fuzz`
(nightly and `cargo-fuzz`; see [docs/development.md](docs/development.md)).
A crash becomes a regression test beside the code it broke.

For local testing with real sandboxes, `make dev/install` points the user
units at the debug build. After moving the checkout, run it again and
`target/debug/psbox install`, since both embed the binary path.

## Dependencies and licenses

[docs/dependencies.md](docs/dependencies.md) is the inventory of every crate
in the build and its license, and [deny.toml](deny.toml) is the policy
`cargo deny check` enforces.

psbox accepts only permissive licenses (MIT, Apache-2.0, BSD, ISC, Zlib,
Unicode-3.0, and similarly approved licenses) plus MPL-2.0. Do not add GPL,
LGPL, AGPL, SSPL, BUSL, Commons Clause, PolyForm, proprietary, custom,
unclear, or comparably reciprocal/source-disclosure dependencies.

Before adding or updating a crate:

1. Prefer the standard library, or a crate already in the graph.
2. Select the version deliberately; do not blindly upgrade the whole graph
   (`cargo update -p <crate>` rather than `cargo update`).
3. Check the license of the crate and of every crate it newly brings in.
4. Run `cargo deny check` and review every `Cargo.toml` and `Cargo.lock`
   change.
5. Update [docs/dependencies.md](docs/dependencies.md) in the same change.

If a license is missing or ambiguous, stop before accepting the dependency.
This policy does not apply to the external programs psbox runs
(`dbus-daemon`, `pasta`, `xwayland-satellite`). Do not vendor crates.

## Rust design and style

- Prefer simple, readable code and explicit control flow.
- Pass state explicitly; avoid global mutable state. The supervisor's
  shared state lives in `Supervisor` behind its locks.
- Keep `pub` items to what other modules use.
- Preserve strict validation and fail-closed security boundaries. Partial
  results are appropriate only where items are explicitly independent (such
  as one sandbox per config in `list` and `install`).
- Never panic on input psbox does not control. `unwrap` and `expect` are for
  invariants the code itself guarantees, with an `expect` message that says
  which.
- Every `unsafe` block has a `// SAFETY:` comment saying why it is sound.
  Code that runs in a child after `clone3` or `fork` in a multithreaded
  process may only make async-signal-safe calls (raw syscalls, no
  allocation or locks).
- Give every thread a clear owner and a way to end.

### Errors and output

- Use `anyhow::Result` and add context that says what failed
  (`with_context(|| format!("mount {}", path.display()))`).
- Errors meant for the user read as a sentence about what is wrong; attach
  a fix with `ui::with_fix` only when it is not obvious from the message.
  Problems that come in groups are reported together (see `plan::Report`
  and `spec::ConfigError`) rather than one at a time.
- Do not discard errors silently. Cleanup and optional progress may ignore
  failures (`let _ =`) when the primary result does not depend on them.
- stdout carries a command's result (tables, lists, `--json`); everything
  else (`✓`, `✗`, `!`, and `→` lines) goes to stderr through `ui`. Styles
  go through `ui` and `anstream`, so `NO_COLOR`, `--color`, and pipes are
  respected.
- The supervisor logs to stderr (the journal) as `psbox[<key>]: <message>`.
  Do not write log files.

### Naming and modules

- Follow standard Rust naming. Accessors do not use a `get_` prefix.
- Do not add a function whose only purpose is to forward to another under a
  second name.
- Every source file opens with a `//!` comment describing its concern.
  Public items get `///` doc comments when their purpose is not obvious
  from the name.
- Within functions, group statements by concern with blank lines.

### Static assets

Do not put substantial scripts, templates, schemas, or configuration in
string literals. Keep them in `src/assets/` and use `include_str!`. Small
constants and short messages may stay inline.

## Testing

- Keep unit tests in a `#[cfg(test)] mod tests` beside the code they cover.
- Add tests for new behavior and bug fixes.
- Prefer focused, in-process tests that need no root, no real sandbox, and
  no session: temporary directories, sample `/proc` and udev data, and
  plans built from a fake `HostEnv`.
- Behavior that needs a real session (sandboxes, GUI apps, devices) is
  checked by hand and recorded in the verification table in
  [docs/development.md](docs/development.md).

## Comments and source text

Comments describe current behavior in present tense. Code comments must
stand alone for repository readers:

- Explain what the code does and why.
- Do not refer to private planning documents, review findings, or temporary
  discussion context.
- Do not narrate removed behavior or mention old identifiers and flags.
- Avoid noisy comments that merely restate obvious code.

Prefer plain ASCII for new source text unless Unicode is required for
correctness or materially improves a user-facing terminal UI (such as the
`✓`, `✗`, `!`, `→`, and `·` in CLI output). Avoid invisible or confusable
characters, and emojis in code, comments, logs, tests, and documentation.

In Markdown, leave a blank line before lists and after headings. Put CLI
commands, paths, environment variables, and configuration keys in
backticks.

## Documentation sync

Documentation is part of the implementation change. Update the page listed
in [docs/index.md](docs/index.md) that covers what changed, and the
[glossary](docs/glossary.md) when a term's meaning is involved. Verify
documentation against the implemented code, not only against the intent
that motivated a change.

## Git and self-review

- Inspect `git status` and the relevant diffs before editing.
- Preserve unrelated user changes in a dirty worktree.
- Do not create or switch branches unless the user asks.
- Do not stage or commit unless the user asks.
- Never commit changes authored by someone else without explicit
  direction.
- Do not add AI attribution, co-author lines, session IDs, or tool
  signatures to commit messages.

When asked to commit, use a concise summary that describes the behavior
changed. Add a body when future readers need constraints, tradeoffs, or
important edge cases.

Before completing a non-trivial task:

1. Review the full diff for correctness, accidental scope, stale
   comments, and documentation impact.
2. Run `git diff --check`.
3. Run the checks appropriate to the changed files.
4. Use an independent review when it is available and materially useful;
   it is not a substitute for your own review.
