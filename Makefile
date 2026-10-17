# psbox build, check, and release targets. Run from the repository root.

CARGO ?= cargo
PREFIX ?= /usr
DESTDIR ?=
USER_UNIT_DIR ?= $(or $(XDG_CONFIG_HOME),$(HOME)/.config)/systemd/user
FUZZ_TIME ?= 60

TARGETS := x86_64-unknown-linux-musl aarch64-unknown-linux-musl
FUZZ_TARGETS := config protocol dbus_message desktop_entry wayland_message
# The version psbox reports, MAJOR.MINOR.PATCH without a `v`: 0.0.0 for
# development builds, set from the tag for releases.
export PSBOX_VERSION ?= 0.0.0
# The release of the distribution packages for one PSBOX_VERSION.
export PKGREL ?= 1
# The Debian changelog's distribution for package-deb.
DEB_DISTRIBUTION ?= unstable
PACKAGE_DIR := target/package

.PHONY: all build test fmt check check/fmt check/clippy check/deny check/version \
	fuzz release dist checksums source package-arch package-deb install dev/install clean

all: build

build:
	$(CARGO) build

test:
	$(CARGO) test

fmt:
	$(CARGO) fmt

# Everything a change must pass.
check: check/fmt check/clippy test check/deny

check/fmt:
	$(CARGO) fmt --check

# Clippy for the native build (what the distribution packages ship) and both
# release targets, so C library and architecture differences are checked.
check/clippy:
	$(CARGO) clippy --all-targets -- -D warnings
	$(CARGO) clippy --all-targets --target x86_64-unknown-linux-musl -- -D warnings
	$(CARGO) clippy --all-targets --target aarch64-unknown-linux-musl -- -D warnings

check/deny:
	$(CARGO) deny check

# Runs each fuzz target for FUZZ_TIME seconds (needs nightly and cargo-fuzz).
fuzz:
	cd fuzz && for t in $(FUZZ_TARGETS); do \
		$(CARGO) +nightly fuzz run $$t -- -max_total_time=$(FUZZ_TIME) || exit 1; \
	done

check/version:
	@echo "$(PSBOX_VERSION)" | grep -Eqx '(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)' || \
		{ echo "PSBOX_VERSION must be MAJOR.MINOR.PATCH without a v prefix" >&2; exit 1; }

# Static release binaries for amd64 and arm64.
release: check/version
	for t in $(TARGETS); do $(CARGO) build --release --target $$t || exit 1; done

# Release archives: the binary, the units, the license, and the README.
dist: release
	mkdir -p dist && rm -f dist/psbox_*_linux_*.tar.gz
	for pair in x86_64-unknown-linux-musl:amd64 aarch64-unknown-linux-musl:arm64; do \
		target=$${pair%%:*}; arch=$${pair##*:}; \
		name=psbox_$(PSBOX_VERSION)_linux_$$arch; \
		mkdir -p dist/$$name/units; \
		cp target/$$target/release/psbox LICENSE README.md dist/$$name/; \
		cp units/psbox@.service units/psbox@.socket dist/$$name/units/; \
		tar -C dist -czf dist/$$name.tar.gz $$name || exit 1; \
		rm -rf dist/$$name; \
	done
	$(MAKE) checksums

# SHA-256 of everything in dist/.
checksums:
	cd dist && find . -maxdepth 1 -type f ! -name checksums.txt -printf '%P\n' | sort | \
		xargs sha256sum > checksums.txt

# The source archive of the committed HEAD, which the distribution packages
# build from: dist/psbox-<version>.tar.gz with one psbox-<version>/ directory.
source: check/version
	mkdir -p dist
	git archive --format=tar.gz --prefix=psbox-$(PSBOX_VERSION)/ \
		-o dist/psbox-$(PSBOX_VERSION).tar.gz HEAD

# An Arch package for this machine's architecture (needs makepkg and cargo).
package-arch: source
	rm -rf $(PACKAGE_DIR)/arch && mkdir -p $(PACKAGE_DIR)/arch
	cp packaging/arch/PKGBUILD dist/psbox-$(PSBOX_VERSION).tar.gz $(PACKAGE_DIR)/arch/
	cd $(PACKAGE_DIR)/arch && \
		PSBOX_SOURCE_B2SUM=$$(b2sum psbox-$(PSBOX_VERSION).tar.gz | cut -d' ' -f1) \
		makepkg --cleanbuild --nosign --noconfirm
	cp $(PACKAGE_DIR)/arch/psbox-$(PSBOX_VERSION)-$(PKGREL)-*.pkg.tar.zst dist/
	$(MAKE) checksums

# A Debian package for this machine's architecture (needs dpkg-buildpackage
# and the build dependencies in packaging/debian/control).
package-deb: source
	rm -rf $(PACKAGE_DIR)/deb && mkdir -p $(PACKAGE_DIR)/deb
	tar -C $(PACKAGE_DIR)/deb -xzf dist/psbox-$(PSBOX_VERSION).tar.gz
	cp -R packaging/debian $(PACKAGE_DIR)/deb/psbox-$(PSBOX_VERSION)/debian
	printf '%s\n' \
		"psbox ($(PSBOX_VERSION)-$(PKGREL)) $(DEB_DISTRIBUTION); urgency=medium" '' \
		'  * psbox $(PSBOX_VERSION).' '' \
		" -- Ryan Petris <ryan@petris.net>  $$(date -R -d @$$(git log -1 --format=%ct))" \
		> $(PACKAGE_DIR)/deb/psbox-$(PSBOX_VERSION)/debian/changelog
	cd $(PACKAGE_DIR)/deb/psbox-$(PSBOX_VERSION) && dpkg-buildpackage --build=binary --no-sign
	cp $(PACKAGE_DIR)/deb/psbox_$(PSBOX_VERSION)-$(PKGREL)_*.deb dist/
	$(MAKE) checksums

# Installs the release binary (from `cargo build --release`) and the user
# units, for example `sudo make install` or `make install DESTDIR=pkg`.
install:
	@test -x target/release/psbox || { echo "run cargo build --release first" >&2; exit 1; }
	install -Dm 0755 target/release/psbox $(DESTDIR)$(PREFIX)/bin/psbox
	install -Dm 0644 units/psbox@.socket $(DESTDIR)$(PREFIX)/lib/systemd/user/psbox@.socket
	sed 's|/usr/bin/psbox|$(PREFIX)/bin/psbox|' units/psbox@.service > psbox@.service.tmp
	install -Dm 0644 psbox@.service.tmp $(DESTDIR)$(PREFIX)/lib/systemd/user/psbox@.service
	rm -f psbox@.service.tmp

# Installs the units for the debug build into the user unit directory, where
# they override packaged units.
dev/install:
	$(CARGO) build
	mkdir -p $(USER_UNIT_DIR)
	install -m 0644 units/psbox@.socket $(USER_UNIT_DIR)/psbox@.socket
	sed 's|/usr/bin/psbox|$(CURDIR)/target/debug/psbox|' units/psbox@.service > $(USER_UNIT_DIR)/psbox@.service
	systemctl --user daemon-reload

clean:
	$(CARGO) clean
	rm -rf dist fuzz/target
