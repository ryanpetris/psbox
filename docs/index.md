# Documentation

User and operator documentation for Petris Sandbox (`psbox`). Definitions
in the [glossary](glossary.md) are canonical: if code and the glossary
disagree, change the code.

Start with the [README](../README.md) for a short pitch and a minimal
setup. Then:

| Page | Concern |
| --- | --- |
| [Install](install.md) | Binaries, systemd user units, first objects |
| [Configuration](configuration.md) | Object YAML, paths, environment |
| [CLI](cli.md) | `psbox launch`, `instance`, `objects`, `sandbox` |
| [Isolation](isolation.md) | `instance` and `oneshot` |
| [Architecture](architecture.md) | `psbox`, `psboxd`, `psboxa`, compose |
| [Instances](instances.md) | Daemon, sockets, hashes, idle teardown |
| [Host URLs](urls.md) | `xdg-open`, GIO, `http`/`https` allowlist |
| [Release](release.md) | Tags, GoReleaser, archive layout |
| [Dependencies](dependencies.md) | Go module licenses |
| [Glossary](glossary.md) | Canonical terms |

Linux only. Release binaries are `amd64` and `arm64` with `CGO_ENABLED=0`.
