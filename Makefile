PREFIX ?= /usr
DESTDIR ?=
BINDIR = $(DESTDIR)$(PREFIX)/bin
APPDIR = $(DESTDIR)$(PREFIX)/share/applications
UNITDIR = $(DESTDIR)$(PREFIX)/lib/systemd/user

BIN = rnetapplet
POPUP_BIN = rnetapplet-popup
VER = $(shell grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
ARCH ?= x86_64
DISTDIR = $(BIN)-$(VER)-$(ARCH)-linux
DISTBALL = $(DISTDIR).tar.gz

.PHONY: all build build-release install uninstall check test clean dist

all: build

build:
	cargo build

build-release:
	cargo build --release --locked --bin rnetapplet
	cargo build --release --locked --features popup --bin rnetapplet-popup

install: build-release
	install -Dm755 target/release/$(BIN) $(BINDIR)/$(BIN)
	install -Dm755 target/release/$(POPUP_BIN) $(BINDIR)/$(POPUP_BIN)
	install -Dm644 packaging/rnetapplet.desktop $(APPDIR)/rnetapplet.desktop
	install -Dm644 packaging/rnetapplet.service $(UNITDIR)/rnetapplet.service

uninstall:
	rm -f $(BINDIR)/$(BIN)
	rm -f $(BINDIR)/$(POPUP_BIN)
	rm -f $(APPDIR)/rnetapplet.desktop
	rm -f $(UNITDIR)/rnetapplet.service

check:
	cargo check --locked --features popup --all-targets
	cargo check --locked --bin rnetapplet

test:
	cargo test --locked --features popup
	cargo test --locked --bin rnetapplet

dist: build-release
	rm -rf $(DISTDIR) $(DISTBALL) $(DISTBALL).sha256
	mkdir -p $(DISTDIR)
	install -m755 target/release/$(BIN) $(DISTDIR)/$(BIN)
	install -m755 target/release/$(POPUP_BIN) $(DISTDIR)/$(POPUP_BIN)
	install -m644 packaging/rnetapplet.desktop $(DISTDIR)/
	install -m644 packaging/rnetapplet.service $(DISTDIR)/
	install -m644 README.md LICENSE-MIT $(DISTDIR)/
	tar czf $(DISTBALL) $(DISTDIR)
	rm -rf $(DISTDIR)
	sha256sum $(DISTBALL) | tee $(DISTBALL).sha256

clean:
	cargo clean
