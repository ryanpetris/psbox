# Agent Guide

Orientation for agents and humans working on Petris Sandbox (`psbox`).

## What psbox is

Petris Sandbox (`psbox`) is three Linux-only Go binaries that run
applications in private-home Bubblewrap sandboxes. Use the Go version
in `go.mod`. Process model, isolation, and CLI verbs are in
[docs/](docs/index.md). The [glossary](docs/glossary.md) is canonical
in code, config, CLI output, and documentation. If code and the
glossary disagree, update the code rather than inventing a second
meaning.

User-visible names, paths, units, and environment variables are
`psbox`. Use `bwrap` only for the Bubblewrap binary, the compose argv
that execs it, the YAML key `bwrap-args`, and comments beside those
calls.

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
- Keep the instance protocol version at `1`; pre-1.0 protocol changes
  replace version-1 behavior directly instead of incrementing the version.

## Project structure

Packages are split by import boundary.

- Executable entry points belong beneath `cmd/psbox`, `cmd/psboxd`, and
  `cmd/psboxa`.
- Binary assembly and implementation details belong under `internal/`.
- New packages belong under `internal/` unless the project deliberately
  introduces and supports an external Go API.
- Keep exported surfaces minimal. An identifier may still need to be
  exported from an `internal` package for another internal package to use
  it.
- Each `cmd/<binary>/main.go` imports only its matching composition root
  beneath `internal/app`.

Package map:

| Boundary | Area | Packages |
| --- | --- | --- |
| Commands | Entry points | `cmd/psbox`, `cmd/psboxd`, `cmd/psboxa` |
| Internal | Entry and CLI | `internal/app/{client,daemon,agent}`, `internal/cli`, `internal/version` |
| Internal | Configuration | `internal/config` |
| Internal | `bwrap` compose | `internal/bwrap` |
| Internal | Client verbs | `internal/launch`, `internal/objects`, `internal/sandbox`, `internal/units` |
| Internal | Instance runtime | `internal/instance` (protocol, daemon, agent, xdg-open) |
| Internal | systemd user manager | `internal/systemd` |
| Internal | Completions | `internal/complete` |
| Internal | Shared support | `log/slog` to stderr |

Do not replace explicit constructors with globals or ad hoc service
location.

Wire binaries with uber-go/fx in `internal/app/*/module.go`.
`fx.Lifecycle` is process-wide only; do not represent launches, instances,
or child processes as graph nodes.

`psbox launch` must not exec `psbox sandbox` (or any other psbox binary)
to compose or run a sandbox. See
[docs/architecture.md](docs/architecture.md).

## Build, test, and release

Run commands from the repository root:

```sh
make build
make go/install
make test
make vet
make check/fmt
```

`make check/fmt` must print nothing. Use `make fmt` on changed Go files.

During development, run focused tests first when useful:

```sh
go test ./internal/config -run TestName -v
```

Before completing a code change, run the full build, test, vet, and
formatting checks above unless the environment makes a check impossible.
Report any check that could not run and why. Documentation-only changes
may use direct documentation and diff validation instead of the full Go
suite.

Before creating any commit, run these mandatory analysis gates from the
repository root:

```sh
make check/staticcheck
make check/deadcode
```

`staticcheck` and `deadcode` must exit successfully. The Makefile
installs either Go tool when unavailable. The documentation-only
validation exception above does not waive these gates when a commit is
requested.

Release binaries are Linux `amd64` and `arm64` builds with `CGO_ENABLED=0`.
Do not add a required CGO code path or dependency unless the user
explicitly approves changing that property. Use the Make targets as the
canonical project checks. Tags, GoReleaser, archives, and unit install
paths are in [docs/release.md](docs/release.md).

## Dependencies and licenses

[docs/dependencies.md](docs/dependencies.md) is the canonical inventory of
every direct and indirect module in `go.mod` and its license.

psbox accepts only Go module licenses allowed by project policy:
permissive licenses such as MIT, BSD, ISC, Apache-2.0, and similarly
approved licenses, plus MPL-2.0. Do not add GPL, LGPL, AGPL, SSPL, BUSL,
Commons Clause, PolyForm, proprietary, custom, unclear, or comparably
reciprocal/source-disclosure Go module dependencies.

Before adding or updating a module:

1. Select the version deliberately; do not blindly upgrade the whole
   module graph.
2. Inspect the candidate module's `LICENSE` or `COPYING` file in the Go
   module cache.
3. Inspect every newly introduced or changed indirect module as well.
4. Run `go mod tidy` after an accepted direct dependency change.
5. Review every `go.mod` and `go.sum` change.
6. Update the matching rows in `docs/dependencies.md`, including
   direct/indirect classification, in the same change.

If a license is missing or ambiguous, stop before accepting the
dependency. This Go-module policy does not apply to external programs
psbox invokes (`bwrap`, `dbus-daemon`, `xdg-open`). The host CLI talks
to the systemd user manager over its private D-Bus socket.
Artifacts psbox redistributes still require review under their
distribution terms.

Do not vendor the module graph. Builds use the module cache and `go.mod` / `go.sum`.

## Go design and style

- Prefer simple, readable code and explicit control flow.
- Use the standard library before adding a dependency.
- Pass dependencies explicitly through constructors and established
  wiring.
- Keep interfaces small and defined around consumer needs.
- Avoid global mutable state and package initialization side effects
  unless the package contract genuinely requires them.
- Keep public APIs small; do not export an identifier speculatively.
- Write incidental output to stderr. Reserve stdout as
  [docs/cli.md](docs/cli.md) specifies.
- Cleanup, optional progress, and optional log-persistence failures do
  not replace a successful result or a primary operation error. Required
  finalization that determines whether the requested operation completed
  may still return an error.
- Preserve strict validation and fail-closed security boundaries.
  Partial results are appropriate only where the behavior explicitly
  treats items as independent.

Use `log/slog` for all logs. Do not write log files. Handler choice is
in [docs/architecture.md](docs/architecture.md).

### Errors, contexts, and concurrency

- Handle errors explicitly and add useful operation context with `%w`.
- Use `errors.Is` and `errors.As`; do not compare error strings.
- Do not discard errors silently.
- Avoid `panic` in library code.
- Pass `context.Context` as the first parameter for request-scoped work.
- Do not store a caller-owned request context in a struct.
- A lifecycle owner may store a context it creates or explicitly owns,
  together with its cancel function, when that context represents
  component lifetime.
- Give every goroutine a clear owner, cancellation path, error path, and
  join or shutdown behavior.
- Do not use channels merely for style; use them when they make
  coordination clearer.

### Naming and APIs

Accessor methods do not use a `Get` prefix. Use `Mounts()` rather than
`GetMounts()`. Setters retain `Set`, and a getter/setter pair uses names
such as `Environment()` and `SetEnvironment()`.

The `Get` name remains valid when GET is the operation (`GetJSON`) or for
a map-style lookup matching standard-library conventions. Constructors,
side-effecting actions, computations, predicates, and `String()` /
`Error()` are not accessors. Generated code follows its generator's
conventions.

Do not add a function or method whose only purpose is to forward to
another under a second name. Keep one canonical name and update callers.

When a type is intended to implement an interface from another package,
add a compile-time assertion beside the type:

```go
var _ instance.Agent = (*Service)(nil)
```

### Packages and files

- Keep one high-level concern per package and one concern per file.
- Put process wiring in `module.go`.
- Give each stateful behavior type its own appropriately named file,
  such as `service.go` or `registry.go`.
- Keep closely related data-only structs and enums in `types.go`; split
  the file when it stops representing one concern.
- Do not create new vague catch-all packages. Do not create a general
  `utils` package.
- Keep methods for one type together when practical.

Every Go file opens with a purpose comment. Exactly one file per package
carries the package documentation comment (`// Package <name> ...`).
Other files put a short concern comment immediately after the `package`
clause. Generated files are exempt from manual comment and organization
rules.

Structural type suffixes:

- `Service` is the coordinator for one package concern.
- `Registry` is an in-memory collection of like items.
- `Router` or `Dispatcher` maps methods to handlers.
- `Handler` implements a related group of requests.

Do not introduce `*Manager` types.

Do not namespace fx value-group names with `psbox.` or package paths.
The element type already disambiguates a group.

Within functions, group statements by concern with blank lines:

- Keep an assignment, its error check, and the uses of that value
  together.
- Keep a mutex lock and its paired deferred unlock together.
- Separate the next unrelated variable or operation with a blank line.
- Do not split every statement mechanically; prefer a few meaningful
  groups.

### Static assets

Do not put substantial scripts, templates, schemas, generated
configuration fixtures, or other static payloads in Go string literals.
Keep them in dedicated files and use `//go:embed` where embedding is
appropriate. Small protocol constants and short messages may remain
inline when that is clearer.

## Testing

- Keep tests beside the code they cover.
- Add tests for new behavior and bug fixes.
- Prefer focused, in-process tests that do not require root privileges or
  a working nested `bwrap`. Nested `bwrap` does not run in some
  development environments; keep those checks as a documented host
  manual checklist.
- Prefer a real lightweight implementation, temporary directory, fake
  clock, or small fake over a large mock framework.
- In new or modified tests, prefer `t.Context()` when a test-scoped
  context is appropriate. Do not churn unrelated tests solely to replace
  `context.Background()`.
- Use `testing/synctest` when it materially makes goroutine or timer
  behavior deterministic; it is not mandatory for every concurrent test.
- Re-run focused tests while developing, then run the full validation
  commands before completing a code change.

## Comments and source text

Comments describe current behavior in present tense. Exported package,
identifier, and API comments follow normal Go conventions and begin with
the name they document where applicable.

Code comments must stand alone for repository readers:

- Explain what the code does and why.
- Do not refer to private planning documents, review findings, gates, or
  temporary discussion context.
- Do not narrate removed behavior or mention old identifiers and flags.
- Avoid noisy comments that merely restate obvious code.

Prefer plain ASCII for new source text unless Unicode is required for
correctness or materially improves a user-facing terminal UI. Avoid
invisible or confusable characters and do not churn existing files solely
to replace intentional Unicode. Avoid emojis in code, comments, logs,
tests, and project documentation.

In Markdown, leave a blank line before lists and after headings. Put CLI
commands, paths, environment variables, and configuration keys in
backticks.

## Documentation sync

Documentation is part of the implementation change. Update the page
listed in [docs/index.md](docs/index.md) that covers what changed, and
the [glossary](docs/glossary.md) when a term's meaning is involved.
Verify documentation against implemented code, not only against the
intent that motivated a change.

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
