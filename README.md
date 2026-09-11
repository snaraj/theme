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
trusted; grant trust to this one formula before adding the tap:

```sh
brew trust --formula snaraj/theme/theme
brew tap snaraj/theme https://github.com/snaraj/theme
brew install snaraj/theme/theme
```

The release workflow verifies published downloads and tests the Homebrew
installation. Delivery stays incomplete until the matching formula PR merges.

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

**Update** — `theme update` (alias `theme upgrade`) replaces a standalone
copy with the latest release after verifying `SHA256SUMS`. Managed installs
print their update options; Homebrew reports the local tap's available version.
To install the latest verified binary separately:

```sh
mkdir -p ~/.local/bin
theme upgrade --binary ~/.local/bin/theme
```

The destination must be new; existing files and symlinks are refused. Run that
path directly or place its directory first in `PATH`. For subsequent updates,
run `~/.local/bin/theme update`. `--version vX.Y.Z` selects a specific release.
Updates never elevate, and package-managed files remain owned by their manager.

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
theme list | search <terms> | browse [terms] | index | preview [-w] <name>
theme status | update | version | rename | rm
```

`--rotate left|right`, `--extend[=hex]`, and `--desktop-only` (wallpaper
without recoloring the terminal) are accepted anywhere; `--mkdir <folder>`
files a `get` download under a library subfolder of your own.

macOS 14 and later keep a wallpaper per Mission Control Space, and the system
tools change only the Space you are looking at. `theme` applies the image to
every Space on every display and seeds the all-Spaces fallback, so Spaces you
create later inherit it too. Your screensaver choices are left alone.

## Browse wallpapers

```sh
theme browse                         # resume the saved query
theme surf ocean --color blue         # a palette filter in addition to text search
theme browse --all --coverage 0.9 --min-width 2560 --aspect 16:9
theme index                          # prepare palettes and cache searchable metadata
```

The browser shows six larger thumbnails per page in Kitty, and the keys move
it: Right/Left step through the results and preview each one, Down/Up,
PageDown/PageUp and Space turn the page, Home/End jump to the first or last.
They act on an empty line and need no Return. Type `select 3` for a picture's
final terminal palette and a text specimen; `n`, `p`, `page 2`, `shuffle`,
`?` and `quit` are lines followed by Return, as before. Only `apply` changes
the wallpaper and terminal colors. Backspace, Ctrl-U and Ctrl-W edit the
typed line and Escape clears it; with text typed the movement keys are
ignored, so a paste can never navigate. Ctrl-C or Ctrl-D on an empty line
leaves, and the terminal is given back the mode it was found in on every exit
— including an error or a panic. Existing shell and terminal shortcuts keep
their meaning.

`favorite` saves the selection; `favorites` shows that collection. `history`
shows recently previewed IDs, and `query mountains` changes the search.
`calmer` ranks images by sampled texture; `similar` and `different` rank by
mean Oklab color distance from the selection. These are measured image
properties, not subject or style recognition. `--all` clears the saved query.
Non-interactive use prints one deterministic page and never consumes commands
or applies a wallpaper: no keys are read and the terminal mode is untouched.

That key path is tested in a terminal that draws, not only down a pipe. Both
hosted CI runners download Kitty 0.48.2 — pinned by SHA-256 and verified
before extraction — run `theme browse` inside it (Linux headless under Xvfb
with software GL, macOS in the runner's own GUI session), press the keys
above through `kitten @ send-text`, and assert on what the screen says
afterwards. The test decodes its own screenshot of the contact sheet, so a
row of blank thumbnails fails it. Screenshots, screen dumps and the kitty log
are published as `kitty-e2e-<OS>-<architecture>` artifacts, pass or fail. It
proves the terminal side only: applying a wallpaper, the macOS Spaces store
and the desktop itself stay outside it.

```sh
python3 -I -B tests/kitty_e2e.py --kitty "$(command -v kitty)" \
  --theme target/release/theme --output target/kitty-e2e
```

Previews use the same contrast-adjusted colors as apply, including all 16 ANSI
colors and Kitty's selection, border, and tab accents. Readability samples a
16-by-16 image grid at the configured opacity; it reports worst text contrast
and the fraction meeting `THEME_CONTRAST`. `--min-contrast 4.5` and
`--coverage 0.9` filter those measurements. Sampling models the wallpaper behind
the terminal; cropping, another window behind it, live opacity changes, and
unsampled detail can affect the real result. It is not an every-pixel guarantee.

Search facts are cached by configured roots, timezone files, image identity,
and palette. Changed images or timezone files refresh automatically. Opening
an unfiltered browser does not prepare the entire metadata index. The first index pass still
reads images and metadata; warm searches reuse those records. The metadata
index is capped at 16 MiB and retains a useful subset for larger libraries.
On macOS, missing source metadata is queried in bounded batches; incomplete
answers fall back to individual lookups. Warm index entries skip those queries.
`theme index` reports inspected images, available palettes, and persisted
metadata separately. Favorites, query, and history stay local in the cache.

For a repeatable editor/shell/output layout, copy
[`examples/kitty-workspace.conf`](examples/kitty-workspace.conf) into your
project and run `kitty --session ./kitty-workspace.conf`. It uses directional
splits without adding or replacing key mappings.

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
- `THEME_CONTRAST` — text contrast target from 1 to 21 (default 4.5).
- `THEME_OPACITY` — explicit preview/apply opacity from 0 to 1. Otherwise,
  literal local Kitty includes are read in order. Dynamic or expanded includes
  require this override; no generated configuration is executed. Live window
  opacity is not queried.
- `THEME_NO_APPLY` — dry-run: announce what would happen, touch nothing.
- `THEME_NO_UPDATE_CHECK` — hide the cached update-available note. Ordinary
  `theme`/`theme help` never make release-network requests: their footer uses
  only a trusted cache entry less than 24 hours old. `theme version` checks
  live (2-second cap), and an explicit latest-release `theme update` refreshes
  the cache too. `-V` and `--version` print the build alone. Cache integrity
  requires trusted directory ownership and no foreign write grants.
- `UNSPLASH_ACCESS_KEY` / `UNSPLASH_SECRET_KEY` / `UNSPLASH_USER_TOKEN` —
  Unsplash credentials (the macOS Keychain is consulted when unset; no
  credential ever appears on an argv).

## Build and test

```sh
cargo build --release            # target/release/theme
cargo test                       # unit + trust-boundary tests
tests/boundary.sh                # the acceptance fixture (headless)
```

CI builds the PR base and candidate with the same Rust toolchain and release
profile on Linux and macOS. It checks bare/help/version, list/verbose list,
preview, metadata/color search, browse/filter, and index. Each existing command
gets 63 alternating warm pairs and nine pairs with fresh application caches.
Cold here does not mean a flushed OS filesystem cache. New commands have no
historical speed comparison until the base supports them; generated-fixture
stall budgets are 500 ms warm and 3 s cold.

The gate checks median and p95 shifts with 99.9% paired bootstrap confidence and
repetition across interleaved sample blocks. A shift counts only above a floor
of 2% of the baseline value, applied to the bootstrap lower bound; a shift with
no faster pair must clear the same 2%. Byte-identical binaries drift by up to
0.9% on this apparatus, and below the floor the harness cannot tell code from
apparatus. The cold phase gates the median alone, because with nine cold samples
p95 is the maximum observation: cold p95 is reported, never gated. Up to three
equal batches vote per metric — the second runs when the first votes or shows
evidence without repetition, the third only when the first two disagree — and
one metric with two votes blocks CI for changed binaries. CI explicitly
selects `--built-artifacts` after comparable direct Cargo builds. With its
controlled synthetic fixture, native executable headers and identical SHA-256
values for both binaries before and after the run, the timing gate reports
`IDENTICAL_BINARY`: there is no code-performance
comparison. All measurements and statistical verdicts remain visible;
executable identity is not a measured speedup. Execution,
semantic and rendering failures still block CI. These checks cannot prove
identical performance on every machine or workload. The harness tests consistent
slowdowns above the floor, missing output and changes to either executable.
Standalone comparisons remain strict by default, including identical wrappers.
`--built-artifacts` rejects `--library`; headers alone do not prove provenance.

Every measured capture must still contain the expected rows, metadata, and
colors. Rendering checks compare 25-, 80-, and 120-column output, while separate
PTY tests cover navigation and wide Unicode filenames. CI publishes timing
summaries and downloadable `command-performance-<OS>-<architecture>` artifacts,
including `results.json`, all stdout/stderr captures, and `rendering.html` with
the actual ANSI colors. These are headless renderings; native Kitty graphics,
font shaping, desktop application, and network latency are outside this gate.
The existing release workflow requires both exact-source CI jobs to pass.

Reproduce from a feature branch (the output directory must be empty):

```sh
THEME_PERF_BASE="$(git rev-parse origin/main)" tests/performance-ci.sh
# Or compare already-built binaries, optionally using a read-only real library:
python3 -I -B tests/performance.py --before /path/to/theme-before \
  --after target/release/theme --output target/performance-local
```
