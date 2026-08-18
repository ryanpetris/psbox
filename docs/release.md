# Release

See [index.md](index.md) for the rest of the documentation.

psbox releases are Linux `amd64` and `arm64` binaries with `CGO_ENABLED=0`.
GitHub Actions runs CI on pull requests and pushes to `master` / `main`.
Pushing an annotated `v*` tag publishes a GitHub Release with GoReleaser.

The in-tree `systemd/` templates are the units shipped in each archive.
Install them to `/usr/lib/systemd/user/`. Distro recipes consume these
files. Do not keep a second copy in this repository.

## Version

`internal/version.Current` is `dev` in an unstamped tree. Make and
GoReleaser override it with:

```text
-X petris.dev/psbox/internal/version.Current=<version>
```

Release tags use semantic versions with a `v` prefix, for example
`v0.1.0`. A prerelease tag such as `v0.1.0-rc.1` is marked as a GitHub
prerelease automatically.

## Cut a release

1. Ensure `make check/all` passes on the commit you intend to ship.
2. Create an annotated tag and push it:

   ```sh
   git tag -a v0.1.0 -m v0.1.0
   git push origin v0.1.0
   ```

3. The `Release` workflow runs GoReleaser and uploads artifacts to the
   GitHub Release for that tag.

The GitHub Actions `GITHUB_TOKEN` is enough to create the release in the
same repository. No extra secrets are required.

## Artifacts

Each platform archive is named `psbox_<version>_linux_<arch>.tar.gz` and
contains:

- `psbox`, `psboxd`, and `psboxa`
- `systemd/psboxd@.socket` and `systemd/psboxd@.service`
- `completions/psbox.bash` and `completions/_psbox`

`checksums.txt` covers every uploaded file.

Install from an archive:

```sh
sudo install -m755 psbox psboxd psboxa /usr/bin/
sudo install -m644 systemd/psboxd@.socket systemd/psboxd@.service /usr/lib/systemd/user/
sudo install -m644 completions/psbox.bash /usr/share/bash-completion/completions/psbox
sudo install -m644 completions/_psbox /usr/share/zsh/site-functions/_psbox
```

Then reload the user manager:

```sh
systemctl --user daemon-reload
```

## Local snapshot

From the repository root:

```sh
make release/check
make release/snapshot
```

`make release/snapshot` writes archives under `dist/` and does not
publish. `dist/` and generated `completions/` are gitignored.

CI also runs `goreleaser check` and a snapshot build so the release
config stays valid without publishing.
