# Petris Sandbox build, test, and analysis targets.
export CGO_ENABLED := 0

GO       ?= go
GOFMT    ?= gofmt
BINDIR   ?= bin

GO_TOOL_BINDIR ?= $(shell d="$$($(GO) env GOBIN)"; if [ -n "$$d" ]; then printf '%s' "$$d"; else printf '%s/bin' "$$($(GO) env GOPATH)"; fi)
STATICCHECK ?= $(GO_TOOL_BINDIR)/staticcheck
DEADCODE ?= $(GO_TOOL_BINDIR)/deadcode
GORELEASER ?= $(GO_TOOL_BINDIR)/goreleaser
COMPLETIONS_DIR ?= completions

PSBOX_VERSION ?= $(shell v="$$(git describe --tags --dirty --always --match 'v*' 2>/dev/null || printf dev)"; printf '%s' "$$v")
GO_LDFLAGS ?= -X petris.dev/psbox/internal/version.Current=$(PSBOX_VERSION)

CMDS := ./cmd/psbox ./cmd/psboxd ./cmd/psboxa

.PHONY: all build
.PHONY: test vet fmt coverage
.PHONY: go/install go/test go/vet go/fmt go/coverage
.PHONY: check check/all check/fmt check/staticcheck check/deadcode
.PHONY: dev/tools dev/tools/staticcheck dev/tools/deadcode dev/tools/goreleaser
.PHONY: release/completions release/check release/snapshot
.PHONY: clean clean/bin clean/coverage clean/completions

all: build

build:
	$(GO) build ./...
	mkdir -p $(BINDIR)
	$(GO) build -trimpath -ldflags "$(GO_LDFLAGS)" -o $(BINDIR)/psbox ./cmd/psbox
	$(GO) build -trimpath -ldflags "$(GO_LDFLAGS)" -o $(BINDIR)/psboxd ./cmd/psboxd
	$(GO) build -trimpath -ldflags "$(GO_LDFLAGS)" -o $(BINDIR)/psboxa ./cmd/psboxa

go/install:
	$(GO) install -trimpath -ldflags "$(GO_LDFLAGS)" $(CMDS)

test: go/test

vet: go/vet

fmt: go/fmt

coverage: go/coverage

go/test:
	$(GO) test ./...

go/vet:
	$(GO) vet ./...

go/fmt:
	find . -name '*.go' -type f -print0 | xargs -0 -r $(GOFMT) -w

go/coverage:
	mkdir -p dist
	$(GO) test -covermode=atomic -coverprofile=dist/coverage.out ./...
	$(GO) tool cover -func=dist/coverage.out > dist/coverage.txt
	$(GO) tool cover -html=dist/coverage.out -o dist/coverage.html

check: go/vet go/test

check/all: check/fmt build go/vet go/test check/staticcheck check/deadcode

check/fmt:
	files="$$(find . -name '*.go' -type f -print0 | xargs -0 -r $(GOFMT) -l)"; \
	if [ -n "$$files" ]; then \
		printf '%s\n' "$$files"; \
		exit 1; \
	fi

check/staticcheck: dev/tools/staticcheck
	$(STATICCHECK) ./...

check/deadcode: dev/tools/deadcode
	$(DEADCODE) ./...

dev/tools: dev/tools/staticcheck dev/tools/deadcode dev/tools/goreleaser

dev/tools/staticcheck:
	@if [ ! -x "$(STATICCHECK)" ]; then \
		GOBIN="$(GO_TOOL_BINDIR)" $(GO) install honnef.co/go/tools/cmd/staticcheck@latest; \
	fi

dev/tools/deadcode:
	@if [ ! -x "$(DEADCODE)" ]; then \
		GOBIN="$(GO_TOOL_BINDIR)" $(GO) install golang.org/x/tools/cmd/deadcode@latest; \
	fi

dev/tools/goreleaser:
	@if [ ! -x "$(GORELEASER)" ]; then \
		GOBIN="$(GO_TOOL_BINDIR)" $(GO) install github.com/goreleaser/goreleaser/v2@latest; \
	fi

release/completions:
	mkdir -p $(COMPLETIONS_DIR)
	$(GO) run ./cmd/psbox completion bash >$(COMPLETIONS_DIR)/psbox.bash
	$(GO) run ./cmd/psbox completion zsh >$(COMPLETIONS_DIR)/_psbox

release/check: dev/tools/goreleaser
	$(GORELEASER) check

release/snapshot: dev/tools/goreleaser release/completions
	$(GORELEASER) release --snapshot --clean --skip=publish

clean: clean/bin clean/coverage clean/completions

clean/bin:
	rm -f $(BINDIR)/psbox $(BINDIR)/psboxd $(BINDIR)/psboxa
	-rmdir $(BINDIR)

clean/coverage:
	rm -f dist/coverage.out dist/coverage.txt dist/coverage.html
	-rmdir dist

clean/completions:
	rm -f $(COMPLETIONS_DIR)/psbox.bash $(COMPLETIONS_DIR)/_psbox
	-rmdir $(COMPLETIONS_DIR)
