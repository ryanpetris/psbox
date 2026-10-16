# Dependencies

Every crate in the `psbox` build for the release targets, with its license.
`cargo deny check` (`make check/deny`, part of `make check`) enforces the
policy in [`deny.toml`](../deny.toml): permissive licenses and MPL-2.0 only.
See `AGENTS.md`, "Dependencies and licenses", before adding or updating one.

## psbox

| Crate | Version | License | Direct |
| --- | --- | --- | --- |
| `anstream` | 1.0.0 | MIT OR Apache-2.0 | yes |
| `anstyle` | 1.0.14 | MIT OR Apache-2.0 | yes |
| `anstyle-parse` | 1.0.0 | MIT OR Apache-2.0 |  |
| `anstyle-query` | 1.1.5 | MIT OR Apache-2.0 |  |
| `anyhow` | 1.0.104 | MIT OR Apache-2.0 | yes |
| `clap` | 4.6.7 | MIT OR Apache-2.0 | yes |
| `clap_builder` | 4.6.7 | MIT OR Apache-2.0 |  |
| `clap_complete` | 4.6.11 | MIT OR Apache-2.0 | yes |
| `clap_derive` | 4.6.7 | MIT OR Apache-2.0 |  |
| `clap_lex` | 1.1.1 | MIT OR Apache-2.0 |  |
| `colorchoice` | 1.0.5 | MIT OR Apache-2.0 |  |
| `equivalent` | 1.0.2 | Apache-2.0 OR MIT |  |
| `hashbrown` | 0.17.1 | MIT OR Apache-2.0 |  |
| `heck` | 0.5.0 | MIT OR Apache-2.0 |  |
| `indexmap` | 2.14.2 | Apache-2.0 OR MIT |  |
| `is_executable` | 1.0.6 | MIT OR Apache-2.0 |  |
| `is_terminal_polyfill` | 1.70.2 | MIT OR Apache-2.0 |  |
| `itoa` | 1.0.18 | MIT OR Apache-2.0 |  |
| `libc` | 0.2.190 | MIT OR Apache-2.0 | yes |
| `memchr` | 2.8.3 | Unlicense OR MIT |  |
| `proc-macro2` | 1.0.107 | MIT OR Apache-2.0 |  |
| `quote` | 1.0.47 | MIT OR Apache-2.0 |  |
| `serde` | 1.0.229 | MIT OR Apache-2.0 | yes |
| `serde_core` | 1.0.229 | MIT OR Apache-2.0 |  |
| `serde_derive` | 1.0.229 | MIT OR Apache-2.0 |  |
| `serde_json` | 1.0.154 | MIT OR Apache-2.0 | yes |
| `serde_spanned` | 1.1.2 | MIT OR Apache-2.0 |  |
| `shlex` | 2.0.1 | MIT OR Apache-2.0 |  |
| `strsim` | 0.11.1 | MIT |  |
| `syn` | 3.0.9 | MIT OR Apache-2.0 |  |
| `toml` | 1.1.8+spec-1.1.0 | MIT OR Apache-2.0 | yes |
| `toml_datetime` | 1.1.2+spec-1.1.0 | MIT OR Apache-2.0 |  |
| `toml_parser` | 1.1.5+spec-1.1.0 | MIT OR Apache-2.0 |  |
| `toml_writer` | 1.1.3+spec-1.1.0 | MIT OR Apache-2.0 |  |
| `unicode-ident` | 1.0.27 | (MIT OR Apache-2.0) AND Unicode-3.0 |  |
| `utf8parse` | 0.2.2 | Apache-2.0 OR MIT |  |
| `winnow` | 1.0.4 | MIT |  |
| `zmij` | 1.0.23 | MIT |  |

## Fuzz targets only

`fuzz/` is a separate workspace that is never part of a release build. Beyond
the crates above, it uses:

| Crate | Version | License |
| --- | --- | --- |
| `anstyle-wincon` | 3.0.11 | MIT OR Apache-2.0 |
| `arbitrary` | 1.5.0 | MIT OR Apache-2.0 |
| `cc` | 1.8.0 | MIT OR Apache-2.0 |
| `cfg-if` | 1.0.5 | MIT OR Apache-2.0 |
| `find-msvc-tools` | 0.1.14 | MIT OR Apache-2.0 |
| `getrandom` | 0.4.3 | MIT OR Apache-2.0 |
| `jobserver` | 0.1.35 | MIT OR Apache-2.0 |
| `libfuzzer-sys` | 0.4.13 | (MIT OR Apache-2.0) AND NCSA |
| `once_cell_polyfill` | 1.70.2 | MIT OR Apache-2.0 |
| `r-efi` | 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later |
| `windows-link` | 0.2.1 | MIT OR Apache-2.0 |
| `windows-sys` | 0.61.2 | MIT OR Apache-2.0 |

`libfuzzer-sys` bundles LLVM libFuzzer (NCSA). `r-efi` is used under MIT or
Apache-2.0. The Windows crates are not built on Linux.
