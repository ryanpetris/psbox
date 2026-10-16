# Release

## Builds

Release binaries are static musl builds for `x86_64-unknown-linux-musl`
(amd64) and `aarch64-unknown-linux-musl` (arm64), with symbols stripped.
psbox has no C dependencies, so both link with Rust's own `rust-lld`
(`.cargo/config.toml`) and need no cross C toolchain:

```sh
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
make release        # target/<triple>/release/psbox
make dist           # dist/psbox_<version>_linux_<arch>.tar.gz and dist/checksums.txt
```

The version comes from `PSBOX_VERSION` (`MAJOR.MINOR.PATCH`, no `v`),
which defaults to `0.0.0` for development builds; `Cargo.toml` stays at
`0.0.0`. `psbox --version` prints it. To build a release locally:

```sh
make dist PSBOX_VERSION=1.2.3
```

## Archives

Each archive is `psbox_<version>_linux_<arch>.tar.gz` (`arch` is `amd64` or
`arm64`), with one top-level directory of the same name:

| File | Install to |
| --- | --- |
| `psbox` | `/usr/bin/psbox` |
| `units/psbox@.socket`, `units/psbox@.service` | `/usr/lib/systemd/user/` |
| `LICENSE`, `README.md` | |

`dist/checksums.txt` has the SHA-256 of every file in `dist/` (`make
checksums`). The archives do not include completions, which `psbox
completions` generates; the distribution packages install them.

## Packaging

`packaging/` holds the distribution recipes: `arch/PKGBUILD` and `debian/`.
Both build from the source archive, run the tests, and install the binary,
the user units, the license, and the bash and zsh completions. The version
comes from `PSBOX_VERSION` and the package release from `PKGREL` (default
`1`):

```sh
make source           # dist/psbox-<version>.tar.gz: `git archive` of HEAD
make package-arch     # dist/psbox-<version>-<rel>-<arch>.pkg.tar.zst (needs makepkg and cargo)
make package-deb      # dist/psbox_<version>-<rel>_<arch>.deb (needs dpkg-buildpackage)
```

Each package is for the architecture it is built on, and its debug package
stays in `target/package/`. `package-deb` writes `debian/changelog` with the
version, `DEB_DISTRIBUTION` (default `unstable`), and the commit's date.
Debian's Rust is older than psbox needs (`rust-version` in `Cargo.toml`),
so the Debian package build-depends on `rustup`, and `debian/rules` installs
the stable toolchain inside the build tree. Both builds fetch crates, so
they need network access. On a machine without `dpkg-buildpackage`, build
in a container:

```sh
docker run --rm -v "$PWD":/src -w /src -e PSBOX_VERSION=1.2.3 debian:13 bash -c '
    apt-get update && apt-get install -y --no-install-recommends \
        build-essential debhelper rustup ca-certificates git
    git config --global --add safe.directory /src
    make package-deb'
```

`make install` installs an existing `cargo build --release` binary and the
units under `DESTDIR` and `PREFIX` (default `/usr`), with the service's
`ExecStart` rewritten to `$(PREFIX)/bin/psbox`:

```sh
cargo build --release
make install DESTDIR="$pkgdir" PREFIX=/usr
```

## Tags and workflows

- `.github/workflows/ci.yml` runs `make check` and `make release` on pull
  requests and pushes to `master` or `main`.
- `.github/workflows/release.yml` runs on a `v*` tag: it checks that the tag
  is `v` plus `MAJOR.MINOR.PATCH` and sets `PSBOX_VERSION` from it, then
  builds the archives and the source archive (`make dist`, `make source`),
  the Arch package for x86_64 in an `archlinux` container, and the Debian
  packages for amd64 and arm64 in `debian:13` containers. It publishes all
  of them with `checksums.txt` as a GitHub release with generated notes.

To release, push a tag `v<version>`, such as `v1.2.3`. Nothing in the
repository changes.
