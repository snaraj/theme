# theme

[![ci](https://github.com/snaraj/theme/actions/workflows/ci.yml/badge.svg)](https://github.com/snaraj/theme/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/snaraj/theme?include_prereleases)](https://github.com/snaraj/theme/releases)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

![theme in a kitty terminal: help and status on the left; list, preview and colorscheme swatches on the right](docs/showcase.png)

One command that owns the desktop wallpaper and the terminal palette
together, so they always agree.

This is the Rust port of the shell `theme` from
[snaraj/dotfiles](https://github.com/snaraj/dotfiles): one static binary,
palette derivation in-process via the workspace `pigment` crate, and no
Python, pywal, or ImageMagick anywhere in the chain.

## Build and test

```sh
cargo build --release            # target/release/theme
cargo test                       # unit + trust-boundary tests
tests/boundary.sh                # the ported acceptance fixture (headless)
```

The fixture drives the compiled binary through a PATH-stubbed network and
scratch directories; it never touches the desktop, a live terminal, or the
real cache.

## Usage

`theme help` is the reference. In brief: `theme random | set <name> |
unsplash [query|page-url] | url <link> | list | preview [-w] <name> |
status | rename | rm` — with `--rotate left|right`, `--extend [hex]`, and
`--desktop-only` (wallpaper without recoloring the terminal) accepted
anywhere.

## Environment

- `THEME_WALLPAPER_DIR` — the wallpaper library. May be a **colon-separated
  list**: every directory is searched (recursively), downloads land in the
  first extant one. Falls back to `WALLPAPER_DIR`, then
  `~/.config/wallpapers`.
- `THEME_CACHE_DIR` — palette cache root (default `~/.cache/theme`; the old
  `WAL_CACHE` name is still honored as a fallback).
- `THEME_FORMATS` / `THEME_EXCLUDE_FORMATS` — the image extensions listed
  and picked from.
- `THEME_CONTRAST` — the minimum text-to-background contrast ratio floor.
- `THEME_NO_APPLY` — dry-run: announce what would happen, touch nothing.
- `UNSPLASH_ACCESS_KEY` / `UNSPLASH_SECRET_KEY` / `UNSPLASH_USER_TOKEN` —
  Unsplash credentials; the macOS Keychain is consulted when unset, and no
  credential ever appears on an argv (curl reads them over stdin config).

## Terminals

Every emitter sits behind one trait; adding a terminal is one more impl.

- **kitty** — recolored live over its remote-control socket
  (`set-colors --all --configured`); the `current-theme.conf` include is
  rewritten for future windows. kitty is never signaled.
- **alacritty** — a managed `theme-colors.toml` is written under
  `~/.config/alacritty/` (when that directory exists); import it once from
  `alacritty.toml` and alacritty live-reloads on every theme change.
- **anything else** — OSC 4/10/11/12 to the calling terminal's own tty.

## Divergences from the shell version

- pywal is gone; palettes derive from `pigment` and cache under
  `THEME_CACHE_DIR/schemes`, keyed by content identity — the scheme shown
  for a wallpaper is always its own.
- ImageMagick and `sips` are gone; rotation, extension, and measurement run
  in-process (`image` crate). WebP transforms re-encode as PNG.
- `theme status` labels the cache line `THEME_CACHE_DIR`.

## Dependency policy

Runtime crates are held to the minimum and each carries its justification
in `Cargo.toml`: `pigment` (workspace), `image` (same pin the engine uses),
and `rustix` (descriptor-anchored `openat`/`O_NOFOLLOW` saving that `std`
cannot express — the download trust chain). External tools used at runtime:
`curl`, `file`, and the platform's wallpaper setter.
