# theme

[![ci](https://github.com/snaraj/theme/actions/workflows/ci.yml/badge.svg)](https://github.com/snaraj/theme/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/snaraj/theme?include_prereleases)](https://github.com/snaraj/theme/releases)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Manage your wallpapers and the aesthetics of your Desktop and Terminal using
the background image as the driver.

![theme in a kitty terminal: help and status on the left; list, preview and colorscheme swatches on the right](docs/showcase.png)

## Install

Every release ships prebuilt binaries for macOS and Linux (arm64 and x86_64)
with a `SHA256SUMS` to verify against — everything below resolves to those
same four tarballs. The Linux builds need glibc 2.34 or newer, and are
verified on Ubuntu 22.04 and 24.04, Debian 12, Fedora 44 and Arch.

**Homebrew** — macOS and Linux. Homebrew 6 requires a third-party tap to be
trusted; the middle line grants that to this one formula and nothing else:

```sh
brew tap snaraj/theme https://github.com/snaraj/theme
brew trust --formula snaraj/theme/theme
brew install snaraj/theme/theme
```

The formula is bumped by a pull request after each release, so `brew` can
be one release behind until that merges; the tarballs below never are.

**Debian, Ubuntu, Fedora, RHEL** — take the `.deb` or `.rpm` for your
architecture from the
[releases page](https://github.com/snaraj/theme/releases), then
`sudo apt install ./theme_*.deb` or `sudo dnf install ./theme-*.rpm`. Each
declares that glibc floor, so a distro below it refuses the install rather
than leaving you a command that cannot run.

**Anywhere else** — the tarball, one file, no installer:

```sh
curl -fsSLO https://github.com/snaraj/theme/releases/latest/download/theme-aarch64-apple-darwin.tar.gz
tar -xzf theme-aarch64-apple-darwin.tar.gz
mkdir -p ~/.local/bin && mv theme ~/.local/bin/
```

- Other targets: `theme-x86_64-apple-darwin.tar.gz`,
  `theme-x86_64-unknown-linux-gnu.tar.gz`,
  `theme-aarch64-unknown-linux-gnu.tar.gz`.
- `~/.local/bin` must be on your `PATH`.
- From source:
  `cargo install --git https://github.com/snaraj/theme --locked`.
- No Snap or Flatpak build: their sandboxes cut off the kitty socket, the
  wallpaper store and `/dev/tty`, which is the entire job. No Windows build.

**Update** — `theme update` replaces the binary it runs from with the
latest release, verified against `SHA256SUMS` before a byte of it is
written; `theme version` and the bare `theme` screen say when one is out.
Homebrew and package installs update through their own manager
(`brew upgrade snaraj/theme/theme`, the next `.deb` or `.rpm`): those files
are the manager's to replace, and `theme update` never elevates.

### Compatibility

| Platform | glibc | Prebuilt binary | From source |
| --- | --- | --- | --- |
| macOS (Apple Silicon, Intel) | — | yes | yes |
| Ubuntu 24.04 | 2.39 | yes | yes |
| Fedora 44 | 2.43 | yes | yes |
| Arch | 2.44 | yes | yes |
| Debian 12 | 2.36 | yes | yes |
| Ubuntu 22.04 | 2.35 | yes | yes |
| Alpine 3 (musl) | — | no | yes |

The prebuilt Linux binaries inherit the glibc floor of the runner that
builds them, which is why they are built on the oldest image GitHub still
offers: the floor is 2.34, below every glibc row above, so only musl
systems build from source. (Releases before v0.3.0 were built on 24.04 and
still want 2.39.) Every Linux row was checked on 2026-09-03 in a
container on x86_64 and arm64 (Arch on x86_64, the only architecture it
publishes an image for): the release tarball for the prebuilt column, a
source build and the whole `tests/boundary.sh` fixture for the other.
`tests/linux-matrix.sh` re-runs the prebuilt half against the current
release.

## Use

`theme help` is the reference. In brief:

```
theme random | set <name|link> | unsplash [query|page-url] | get <link>
theme list | search <terms> | preview [-w] <name> | status | update | version | rename | rm
```

`--rotate left|right`, `--extend[=hex]`, and `--desktop-only` (wallpaper
without recoloring the terminal) are accepted anywhere; `--mkdir <folder>`
files a `get` download under a library subfolder of your own.

macOS 14 and later keep a wallpaper per Mission Control Space, and the system
tools change only the Space you are looking at. `theme` applies the image to
every Space on every display and seeds the all-Spaces fallback, so Spaces you
create later inherit it too. Your screensaver choices are left alone.

## Terminals

- **kitty** — recolored live over its remote-control socket; future windows
  pick the palette up from `current-theme.conf`.
- **alacritty** — a managed `theme-colors.toml` is written under
  `~/.config/alacritty/`; import it once and alacritty live-reloads on every
  theme change.
- **anything else** — standard OSC 4/10/11/12 color sequences to the calling
  terminal.

## Environment

- `THEME_WALLPAPER_DIR` — the wallpaper library; a colon-separated list is
  allowed (every directory searched, downloads land in the first).
- `THEME_CACHE_DIR` — palette cache root (default `~/.cache/theme`).
- `THEME_CONTRAST` — minimum text-to-background contrast floor.
- `THEME_NO_APPLY` — dry-run: announce what would happen, touch nothing.
- `THEME_NO_UPDATE_CHECK` — disable the update-available note on the bare
  `theme` screen. The check asks GitHub for the latest release tag at most
  once every 24 hours (2-second cap, silent on failure, nothing sent but
  the request itself), cached under `THEME_CACHE_DIR`; the explicit
  `theme update` installs it and is never disabled by this. The cache is
  only used while every directory on its path is owned by you and free of
  write-granting ACLs — a cache you have deliberately ACL'd open is out of
  contract and the check silently stands down.
- `UNSPLASH_ACCESS_KEY` / `UNSPLASH_SECRET_KEY` / `UNSPLASH_USER_TOKEN` —
  Unsplash credentials (the macOS Keychain is consulted when unset; no
  credential ever appears on an argv).

## Build and test

```sh
cargo build --release            # target/release/theme
cargo test                       # unit + trust-boundary tests
tests/boundary.sh                # the acceptance fixture (headless)
```
