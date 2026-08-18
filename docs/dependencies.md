# Dependencies

psbox itself is MIT. See [LICENSE](../LICENSE).

Canonical inventory of Go modules in `go.mod` and their licenses.

Reviewed against each module's `LICENSE` or `COPYING` file in the module
cache before accept.

## Direct

| Module | Version | License | Use |
| --- | --- | --- | --- |
| `github.com/godbus/dbus/v5` | v5.2.2 | BSD-2-Clause | systemd user-manager D-Bus |
| `github.com/spf13/cobra` | v1.9.1 | Apache-2.0 | CLI |
| `go.uber.org/fx` | v1.24.0 | MIT | Process wiring |
| `golang.org/x/crypto` | v0.41.0 | BSD-3-Clause | blake2b-512 hashes |
| `golang.org/x/sys` | v0.35.0 | BSD-3-Clause | SEQPACKET and SCM_RIGHTS |
| `gopkg.in/yaml.v3` | v3.0.1 | MIT and Apache-2.0 | Object YAML |

`gopkg.in/yaml.v3` is dual-licensed: libyaml-derived files remain MIT, and
the rest of the module is Apache-2.0.

## Indirect

| Module | Version | License | Introduced by |
| --- | --- | --- | --- |
| `github.com/inconshreveable/mousetrap` | v1.1.0 | Apache-2.0 | cobra |
| `github.com/spf13/pflag` | v1.0.6 | BSD-3-Clause | cobra |
| `go.uber.org/dig` | v1.19.0 | MIT | fx |
| `go.uber.org/multierr` | v1.10.0 | MIT | fx |
| `go.uber.org/zap` | v1.26.0 | MIT | fx |
