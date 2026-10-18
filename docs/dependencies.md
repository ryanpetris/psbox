# Dependencies

Every crate in the `psbox` build for the release targets, with its license.
`cargo deny check` (`make check/deny`, part of `make check`) enforces the
policy in [`deny.toml`](../deny.toml): permissive licenses and MPL-2.0 only.
See `AGENTS.md`, "Dependencies and licenses", before adding or updating one.

## psbox

| Crate | Version | License | Direct |
| --- | --- | --- | --- |
| `allocator-api2` | 0.2.21 | MIT OR Apache-2.0 |  |
| `anstream` | 1.0.0 | MIT OR Apache-2.0 | yes |
| `anstyle` | 1.0.14 | MIT OR Apache-2.0 | yes |
| `anstyle-parse` | 1.0.0 | MIT OR Apache-2.0 |  |
| `anstyle-query` | 1.1.5 | MIT OR Apache-2.0 |  |
| `anyhow` | 1.0.104 | MIT OR Apache-2.0 | yes |
| `arraydeque` | 0.5.1 | MIT OR Apache-2.0 |  |
| `arrayref` | 0.3.9 | BSD-2-Clause |  |
| `arrayvec` | 0.7.8 | MIT OR Apache-2.0 |  |
| `bitflags` | 2.13.2 | MIT OR Apache-2.0 |  |
| `block-buffer` | 0.10.4 | MIT OR Apache-2.0 |  |
| `bumpalo` | 3.20.3 | MIT OR Apache-2.0 |  |
| `bytemuck` | 1.25.2 | Zlib OR Apache-2.0 OR MIT |  |
| `bytemuck_derive` | 1.12.1 | Zlib OR Apache-2.0 OR MIT |  |
| `cc` | 1.8.0 | MIT OR Apache-2.0 |  |
| `cfg-if` | 1.0.5 | MIT OR Apache-2.0 |  |
| `clap` | 4.6.7 | MIT OR Apache-2.0 | yes |
| `clap_builder` | 4.6.7 | MIT OR Apache-2.0 |  |
| `clap_complete` | 4.6.11 | MIT OR Apache-2.0 | yes |
| `clap_derive` | 4.6.7 | MIT OR Apache-2.0 |  |
| `clap_lex` | 1.1.1 | MIT OR Apache-2.0 |  |
| `colorchoice` | 1.0.5 | MIT OR Apache-2.0 |  |
| `cpufeatures` | 0.2.17 | MIT OR Apache-2.0 |  |
| `crypto-common` | 0.1.7 | MIT OR Apache-2.0 |  |
| `cursor-icon` | 1.2.0 | MIT OR Apache-2.0 OR Zlib |  |
| `digest` | 0.10.7 | MIT OR Apache-2.0 |  |
| `downcast-rs` | 1.2.1 | MIT/Apache-2.0 |  |
| `equivalent` | 1.0.2 | Apache-2.0 OR MIT |  |
| `errno` | 0.3.14 | MIT OR Apache-2.0 |  |
| `find-msvc-tools` | 0.1.14 | MIT OR Apache-2.0 |  |
| `foldhash` | 0.1.5 | Zlib |  |
| `foldhash` | 0.2.0 | Zlib |  |
| `font-types` | 0.12.7 | MIT OR Apache-2.0 |  |
| `generic-array` | 0.14.7 | MIT |  |
| `hashbrown` | 0.15.5 | MIT OR Apache-2.0 |  |
| `hashbrown` | 0.16.1 | MIT OR Apache-2.0 |  |
| `hashlink` | 0.10.0 | MIT OR Apache-2.0 |  |
| `heck` | 0.5.0 | MIT OR Apache-2.0 |  |
| `is_executable` | 1.0.6 | MIT OR Apache-2.0 |  |
| `is_terminal_polyfill` | 1.70.2 | MIT OR Apache-2.0 |  |
| `itoa` | 1.0.18 | MIT OR Apache-2.0 |  |
| `keccak` | 0.1.6 | Apache-2.0 OR MIT |  |
| `libc` | 0.2.190 | MIT OR Apache-2.0 | yes |
| `linux-raw-sys` | 0.12.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |  |
| `log` | 0.4.34 | MIT OR Apache-2.0 |  |
| `md-5` | 0.10.6 | MIT OR Apache-2.0 |  |
| `memchr` | 2.8.3 | Unlicense OR MIT |  |
| `memmap2` | 0.9.11 | MIT OR Apache-2.0 |  |
| `once_cell` | 1.21.4 | MIT OR Apache-2.0 |  |
| `pkg-config` | 0.3.34 | MIT OR Apache-2.0 |  |
| `proc-macro2` | 1.0.107 | MIT OR Apache-2.0 |  |
| `quick-xml` | 0.41.0 | MIT |  |
| `quote` | 1.0.47 | MIT OR Apache-2.0 |  |
| `read-fonts` | 0.41.0 | MIT OR Apache-2.0 |  |
| `rsjsonnet-lang` | 0.5.0 | MIT OR Apache-2.0 | yes |
| `rustix` | 1.1.5 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |  |
| `saphyr-parser` | 0.0.6 | MIT OR Apache-2.0 |  |
| `sctk-adwaita` | 0.12.0 | MIT | yes |
| `serde` | 1.0.229 | MIT OR Apache-2.0 | yes |
| `serde_core` | 1.0.229 | MIT OR Apache-2.0 |  |
| `serde_derive` | 1.0.229 | MIT OR Apache-2.0 |  |
| `serde_json` | 1.0.154 | MIT OR Apache-2.0 | yes |
| `sha1` | 0.10.7 | MIT OR Apache-2.0 |  |
| `sha2` | 0.10.9 | MIT OR Apache-2.0 |  |
| `sha3` | 0.10.9 | MIT OR Apache-2.0 |  |
| `shlex` | 2.0.1 | MIT OR Apache-2.0 |  |
| `skrifa` | 0.44.0 | MIT OR Apache-2.0 |  |
| `smallvec` | 1.16.3 | MIT OR Apache-2.0 |  |
| `smithay-client-toolkit` | 0.21.1 | MIT | yes |
| `strict-num` | 0.1.1 | MIT |  |
| `strsim` | 0.11.1 | MIT |  |
| `syn` | 3.0.9 | MIT OR Apache-2.0 |  |
| `thiserror` | 2.0.21 | MIT OR Apache-2.0 |  |
| `thiserror-impl` | 2.0.21 | MIT OR Apache-2.0 |  |
| `tiny-skia` | 0.12.0 | BSD-3-Clause |  |
| `tiny-skia-path` | 0.12.0 | BSD-3-Clause |  |
| `typenum` | 1.20.1 | MIT OR Apache-2.0 |  |
| `unicode-ident` | 1.0.27 | (MIT OR Apache-2.0) AND Unicode-3.0 |  |
| `utf8parse` | 0.2.2 | Apache-2.0 OR MIT |  |
| `version_check` | 0.9.5 | MIT OR Apache-2.0 |  |
| `wayland-backend` | 0.3.17 | MIT | yes |
| `wayland-client` | 0.31.15 | MIT | yes |
| `wayland-csd-frame` | 0.3.0 | MIT |  |
| `wayland-cursor` | 0.31.14 | MIT |  |
| `wayland-protocols` | 0.32.13 | MIT | yes |
| `wayland-protocols-experimental` | 20251230.0.3 | MIT |  |
| `wayland-protocols-misc` | 0.3.12 | MIT |  |
| `wayland-protocols-wlr` | 0.3.12 | MIT |  |
| `wayland-scanner` | 0.31.11 | MIT |  |
| `wayland-sys` | 0.31.11 | MIT |  |
| `xcursor` | 0.3.11 | MIT |  |
| `xkeysym` | 0.2.1 | MIT OR Apache-2.0 OR Zlib |  |
| `zmij` | 1.0.23 | MIT |  |

`sctk-adwaita` embeds the Cantarell Regular font (SIL Open Font License
1.1), which it draws title bars with when the system's title bar font cannot
be loaded. `cc` and `pkg-config` are build dependencies of `wayland-backend`
and `wayland-sys` that compile or link C only for features psbox does not
enable (`log`, and the system libwayland backend), so the build stays free
of C.

## Fuzz targets only

`fuzz/` is a separate workspace that is never part of a release build. Beyond
the crates above, it uses:

| Crate | Version | License |
| --- | --- | --- |
| `anstyle-wincon` | 3.0.11 | MIT OR Apache-2.0 |
| `arbitrary` | 1.5.0 | MIT OR Apache-2.0 |
| `getrandom` | 0.4.3 | MIT OR Apache-2.0 |
| `jobserver` | 0.1.35 | MIT OR Apache-2.0 |
| `libfuzzer-sys` | 0.4.13 | (MIT OR Apache-2.0) AND NCSA |
| `once_cell_polyfill` | 1.70.2 | MIT OR Apache-2.0 |
| `r-efi` | 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later |
| `windows-link` | 0.2.1 | MIT OR Apache-2.0 |
| `windows-sys` | 0.61.2 | MIT OR Apache-2.0 |

`libfuzzer-sys` bundles LLVM libFuzzer (NCSA). `r-efi` is used under MIT or
Apache-2.0. The Windows crates are not built on Linux.
