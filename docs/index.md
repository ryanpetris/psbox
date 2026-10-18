# Documentation

User and developer documentation for Petris Sandbox (`psbox`). Definitions
in the [glossary](glossary.md) are canonical: if code and the glossary
disagree, change the code.

Start with the [README](../README.md) for a short pitch and a minimal
setup. Then:

| Page | Concern |
| --- | --- |
| [Install](install.md) | Requirements, release archives, building from source, units, completions |
| [Configuration](configuration.md) | Sandbox configs in Jsonnet, the psbox library, mounts, devices, network, profiles, apps and desktop entries, portal identity, tasks |
| [CLI](cli.md) | Commands, output, the startup check, unnamed sandboxes |
| [Isolation](isolation.md) | What a sandbox can and cannot reach |
| [Architecture](architecture.md) | Supervisors, units, cgroups, plans, the sandbox root, live changes, devices, D-Bus, portal identity, Xwayland, network |
| [Development](development.md) | Checks, fuzzing, headless GUI tests, verification status |
| [Release](release.md) | Static builds, archives, workflows, packaging |
| [Dependencies](dependencies.md) | Crates and their licenses |
| [Glossary](glossary.md) | Canonical terms |

Linux only. The release archives hold static `amd64` and `arm64` binaries;
the Arch and Debian packages are native builds for their distribution.
