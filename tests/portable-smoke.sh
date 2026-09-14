#!/bin/sh
# Exercise a built binary in a minimal OS, without a desktop or network.
set -eu
umask 077
theme_bin=${1:?usage: portable-smoke.sh /absolute/path/to/theme}
fixture=$(mktemp -d "${TMPDIR:-/tmp}/theme-portable.XXXXXX")
trap 'rm -rf "$fixture"' EXIT HUP INT TERM
mkdir -p "$fixture/library" "$fixture/config" "$fixture/cache"
printf '%s' 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNgYGAAAAAEAAH2FzhVAAAAAElFTkSuQmCC' | base64 -d > "$fixture/library/blue.png"
export THEME_NO_APPLY=1 THEME_NO_UPDATE_CHECK=1 THEME_OPACITY=1 THEME_CONTRAST=4.5
export THEME_WALLPAPER_DIR="$fixture/library" THEME_CACHE_DIR="$fixture/cache"
export CONFIG_DIR="$fixture/config" KITTY_CONFIG_DIRECTORY="$fixture/config"
export KITTY_WINDOW_ID='' TERM=dumb COLUMNS=80 THEME_FORMATS=png THEME_EXCLUDE_FORMATS=''
"$theme_bin" --version > "$fixture/version"
grep -q '^version: v' "$fixture/version"
"$theme_bin" help > "$fixture/help"
grep -q 'Library Commands:' "$fixture/help"
"$theme_bin" list > "$fixture/list"
grep -q 'blue' "$fixture/list"
"$theme_bin" preview blue > "$fixture/preview"
grep -q 'TITLE.*blue' "$fixture/preview"
grep -q 'COLORSCHEME' "$fixture/preview"
"$theme_bin" browse --all > "$fixture/browse"
grep -q '1 matches' "$fixture/browse"
"$theme_bin" browse --all --min-contrast 1 > "$fixture/filter"
grep -q '1 matches' "$fixture/filter"
"$theme_bin" search blue > "$fixture/search"
grep -q 'blue' "$fixture/search"
"$theme_bin" index > "$fixture/index"
grep -q 'inspected 1 wallpaper' "$fixture/index"
"$theme_bin" set blue > "$fixture/set" 2>&1
grep -q '\[no-apply\]' "$fixture/set"
"$theme_bin" status > "$fixture/status"
printf 'portable CLI: PASS (10 commands, image decode, palette, browse, dry-run apply)\n'
