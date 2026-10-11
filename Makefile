CARGO ?= cargo
INSTALL ?= install
PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
XDG_CONFIG_HOME ?= $(HOME)/.config
SYSTEMD_USER_UNIT_DIR ?= $(if $(filter $(HOME)/.local,$(PREFIX)),$(XDG_CONFIG_HOME)/systemd/user,$(PREFIX)/lib/systemd/user)

.PHONY: all build install

all: build

build:
	$(CARGO) build --release --locked

install: build
	$(INSTALL) -Dm755 target/release/blatt "$(DESTDIR)$(BINDIR)/blatt"
	$(INSTALL) -Dm644 systemd/blatt.service "$(DESTDIR)$(SYSTEMD_USER_UNIT_DIR)/blatt.service"
