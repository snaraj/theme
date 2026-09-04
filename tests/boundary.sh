#!/usr/bin/env bash
# Boundary fixture for the Rust `theme` binary — the port of the dotfiles
# theme-boundary-tests.sh acceptance suite. Same doctrine: destructive verbs
# act on library NAMES only; positives assert the MUTATION, not exit 0;
# refusals leave victims untouched; the network boundary runs against
# deterministic curl stubs (PATH-resolved for the wallpaper/credential
# lanes per the reviewed parity design; via the debug-only THEME_CURL seam
# for the self-update lane, whose transport never consults PATH);
# credentials never reach argv and a hostile credential produces ZERO
# transfers; every command surface is swept with OSC-52-poisoned inputs
# and may emit no terminal protocol.
#
# Two sections of the shell fixture extracted python from the script under
# test and drove it directly (the descriptor-bound saver's check/use windows,
# the ACL interrogator branches, and the contrast floor). Those trust
# decisions now live in Rust and are driven natively — src/save_tests.rs
# (FIFO-opened swap windows, the in-process POSIX ACL xattr parser, darwin
# ACL allow/deny/writesecurity) and the floor regression in src/apply.rs — so
# this file carries their END-TO-END shapes (world-writable ancestor, ACL'd
# library, symlinked provider) through the real binary instead.
#
# The pywal scheme-cache ownership section is retired WITH ITS PROBLEM: the
# pigment cache keys on the canonical path + mtime + size (no lossy name
# mangling), so name-extension and collision attacks have no surface. What
# remains testable is tested: own scheme after backfill, dash for underived,
# corrupt entry degrades silently, multi-dir attribution by construction.
#
# Run: THEME_BIN=target/release/theme tests/boundary.sh   (exits 0 on pass)
set -u
# The fixture builds the library, the cache and every planted directory
# itself, and the custody audits it pins REFUSE a group-writable one — so
# the world it builds must not depend on the caller's umask. macOS defaults
# to 022 and always satisfied this by accident; Ubuntu defaults to 002 and
# turned every custody positive into a refusal. Stated, not inherited.
umask 022
root="$(cd "$(dirname "$0")/.." && pwd)"
THEME="${THEME_BIN:-$root/target/release/theme}"
[ -x "$THEME" ] || { echo "FAIL  no binary at $THEME (cargo build --release first)"; exit 1; }
fails=0
pass() { echo "PASS  $1"; }
fail() { echo "FAIL  $1"; fails=$((fails + 1)); }

check() { # $1 description, $2 expected exit (0=ok nonzero=refused), then cmd…
    local desc="$1" want="$2" got
    shift 2
    if "$@" >/dev/null 2>&1; then got=0; else got=1; fi
    if { [ "$want" = 0 ] && [ "$got" = 0 ]; } || { [ "$want" != 0 ] && [ "$got" != 0 ]; }; then
        pass "$desc"
    else
        fail "$desc (exit $got, wanted $want)"
    fi
}

exists() { # $1 description, $2 yes|no, $3 path
    if [ "$2" = yes ] && [ -e "$3" ]; then pass "$1"
    elif [ "$2" = no ] && [ ! -e "$3" ]; then pass "$1"
    else fail "$1"; fi
}

# The update-available check is disabled for the WHOLE fixture — no case may
# ever touch the real network; the footer-note section re-enables it per run
# against a stubbed/failing curl.
export THEME_NO_UPDATE_CHECK=1

# The self-update/footer transport never consults PATH (round 8): those
# sections drive a DEBUG build, whose THEME_CURL test seam (compiled OUT of
# release) aims the trusted-transport lane at the deterministic stubs. One
# release-binary pin proves the seam and PATH are both ignored there.
THEME_DBG="${THEME_DEBUG_BIN:-$root/target/debug/theme}"
[ -x "$THEME_DBG" ] || { echo "FAIL  no debug binary at $THEME_DBG (cargo build first — the update sections need its THEME_CURL seam)"; exit 1; }

# The AMBIENT credential environment is neutralised once, here, for the whole
# fixture; every case that needs a credential sets it explicitly.
outer_token="${UNSPLASH_USER_TOKEN-}"
export UNSPLASH_USER_TOKEN="" UNSPLASH_ACCESS_KEY="" UNSPLASH_SECRET_KEY=""
if grep -q '^export UNSPLASH_USER_TOKEN="" UNSPLASH_ACCESS_KEY="" UNSPLASH_SECRET_KEY=""$' "$0" &&
    [ "$(grep -c '^export UNSPLASH_USER_TOKEN' "$0")" = 1 ]; then
    pass "the fixture neutralises the ambient credential environment exactly once"
else fail "the fixture no longer neutralises the ambient credential environment"; fi
child_env=$(bash -c 'printf "%s|%s|%s" "${UNSPLASH_USER_TOKEN-unset}" "${UNSPLASH_ACCESS_KEY-unset}" "${UNSPLASH_SECRET_KEY-unset}"')
if [ "$child_env" = "||" ]; then
    pass "no ambient credential reaches an unpinned child (outer token was ${#outer_token} bytes)"
else fail "an ambient credential reached an unpinned child: $child_env"; fi

# Spelled-out template, and NOT under TMPDIR: `mktemp -t` means opposite
# things to BSD and GNU (the GNU form also demands the Xs), and on Linux
# TMPDIR is the world-writable /tmp — a chain the saver's ancestor audit
# refuses outright, which is why the unit tests' scratch already lives here
# (src/save_tests.rs). target/ sits on a user-owned chain wherever the repo
# is sanely checked out.
mkdir -p "$root/target/test-tmp" || exit 1
fixture=$(mktemp -d "$root/target/test-tmp/theme-boundary.XXXXXX") || exit 1
trap 'rm -rf "$fixture"' EXIT
lib="$fixture/library"
out="$fixture/outside"
mkdir -p "$lib" "$out" "$fixture/tmpdir" "$fixture/cache"
printf 'x' >"$lib/in-lib.jpg"
printf 'x' >"$lib/renameme.jpg"
printf 'x' >"$lib/keepme.jpg"
printf 'x' >"$out/delete-victim.jpg"
printf 'x' >"$out/rename-victim.jpg"
ln -s "$out/delete-victim.jpg" "$lib/escape.jpg"
png1x1() { # $1 dest, $2 r, $3 g, $4 b  (a real 1x1 PNG, any solid color)
    python3 - "$1" "$2" "$3" "$4" <<'PY'
import struct, sys, zlib
dest, r, g, b = sys.argv[1], *(int(x) for x in sys.argv[2:5])
def chunk(t, d):
    return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d))
raw = zlib.compress(b"\x00" + bytes([r, g, b]))
png = (b"\x89PNG\r\n\x1a\n"
       + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
       + chunk(b"IDAT", raw) + chunk(b"IEND", b""))
open(dest, "wb").write(png)
PY
}
png1x1 "$lib/tiny.png" 0 0 0
printf 'not an image' >"$lib/broken.jpg"

run() { THEME_WALLPAPER_DIR="$1" THEME_NO_APPLY=1 THEME_CACHE_DIR="$fixture/cache" \
    TMPDIR="$fixture/tmpdir" "$THEME" "${@:2}"; }
run_nokitty() { THEME_WALLPAPER_DIR="$1" THEME_NO_APPLY=1 THEME_CACHE_DIR="$fixture/cache" \
    TMPDIR="$fixture/tmpdir" KITTY_WINDOW_ID='' "$THEME" "${@:2}"; }

# theme writes and reads a wallpaper's provenance through the macOS `xattr`
# tool; Linux ships no counterpart, so a planted theme.* attribute is
# invisible there and the sections that assert on one have nothing to drive.
# What those cases pin — the display sanitizing and the parsed-host label —
# is platform-independent code, exercised on the platform that can carry it.
xattr_meta() { [ "$(uname -s)" = Darwin ]; }

# --- positive destructive ops must MUTATE, not merely exit 0 ---------------
check  "in-library rm succeeds"                0 run "$lib" rm in-lib.jpg
exists "in-library rm really deleted"          no "$lib/in-lib.jpg"
check  "in-library rename succeeds"            0 run "$lib" rename renameme renamed-fine
exists "rename source gone"                    no "$lib/renameme.jpg"
exists "rename destination exists"             yes "$lib/renamed-fine.jpg"

# --- boundary refusals, victims untouched ----------------------------------
check  "rm refuses ..-traversal"               1 run "$lib" rm ../outside/delete-victim.jpg
check  "rename refuses ..-traversal"           1 run "$lib" rename ../outside/rename-victim.jpg moved
check  "rm refuses absolute path"              1 run "$lib" rm "$out/delete-victim.jpg"
check  "rm refuses nested path"                1 run "$lib" rm outside/delete-victim.jpg
check  "rm of an in-library symlink succeeds"  0 run "$lib" rm escape.jpg
exists "symlink target beyond boundary intact" yes "$out/delete-victim.jpg"
exists "outside rename-victim untouched"       yes "$out/rename-victim.jpg"

# A row count past usize used to be all digits, so the shape check passed and
# the number that did not fit became the default ten rows: exit 0, ten rows,
# nobody told. It has to REFUSE.
check  "-n takes the largest count there is"   0 run "$lib" list -n 18446744073709551615
check  "-n refuses a count past usize"         1 run "$lib" list -n 18446744073709551616
check  "-n refuses a wildly overflowing count" 1 run "$lib" list -n 184467440737095516160

# --- truncated/stem resolution: exactly ONE candidate or refuse ------------
printf 'x' >"$lib/same-title.jpg"
printf 'x' >"$lib/same-title.png"
printf 'x' >"$lib/uniq-stem-one.jpg"
printf 'x' >"$lib/uniq-prefix-only-here.jpg"
check  "rm refuses same-title.jpg/.png stem"    1 run "$lib" rm same-title
exists "same-title.jpg intact after refusal"    yes "$lib/same-title.jpg"
exists "same-title.png intact after refusal"    yes "$lib/same-title.png"
check  "rename refuses same-title stem"         1 run "$lib" rename same-title other-name
exists "same-title.jpg intact after rename ref" yes "$lib/same-title.jpg"
exists "same-title.png intact after rename ref" yes "$lib/same-title.png"
check  "rm refuses ambiguous truncated prefix"  1 run "$lib" rm same-tit
exists "no same-title victim of the prefix"     yes "$lib/same-title.jpg"
check  "unique stem still resolves for rm"      0 run "$lib" rm uniq-stem-one
exists "unique stem really deleted"             no "$lib/uniq-stem-one.jpg"
check  "unique truncated … resolves for rm"     0 run "$lib" rm "uniq-prefix-onl…"
exists "unique truncated target really deleted" no "$lib/uniq-prefix-only-here.jpg"

# --- unknown trailing option: refused BEFORE any side effect ---------------
check  "rm with trailing unknown flag refused" 1 run "$lib" rm keepme.jpg --bogus
exists "no partial delete before flag error"   yes "$lib/keepme.jpg"
check  "set with trailing unknown flag refused" 1 run "$lib" set keepme.jpg --bogus

# --- `wal` stays removed ----------------------------------------------------
walout=$(run "$lib" wal 2>&1 || true)
if printf '%s' "$walout" | grep -qF "unknown command 'wal'"; then
    pass "the removed 'wal' command is rejected as unknown"
else fail "'theme wal' still dispatches instead of being unknown"; fi

# --- OWNER FIX: help lists the full command first, then aliases -------------
helpout=$(run_nokitty "$lib" help 2>&1)
if printf '%s' "$helpout" | grep -q '^  remove, rm' &&
    ! printf '%s' "$helpout" | grep -q '^  rm, remove' &&
    printf '%s' "$helpout" | grep -q '^  list, ls'; then
    pass "help lists full command names first (remove, rm / list, ls)"
else fail "help alias ordering drifted"; fi
# `theme help <command>` asks what `theme <command> --help` asks, so it
# must print that answer and not the whole bare screen (which it did).
for c in update version; do
    hc=$(run_nokitty "$lib" help "$c" 2>&1)
    if [ "$hc" = "$(run_nokitty "$lib" "$c" --help 2>&1)" ] && [ "$hc" != "$helpout" ]; then
        pass "theme help $c is theme $c --help, not the bare screen"
    else fail "theme help $c did not route to the per-command help"; fi
done

# --- OWNER FIX: --desktop-only skips the terminal, never by default ---------
donly=$(run "$lib" set tiny.png --desktop-only 2>&1)
if printf '%s' "$donly" | grep -q 'would set the desktop wallpaper' &&
    ! printf '%s' "$donly" | grep -q 'would derive a palette'; then
    pass "--desktop-only sets the desktop and leaves the palette alone"
else fail "--desktop-only did not skip the palette (or skipped the desktop)"; fi
dfull=$(run "$lib" set tiny.png 2>&1)
if printf '%s' "$dfull" | grep -q 'would set the desktop wallpaper' &&
    printf '%s' "$dfull" | grep -q 'would derive a palette'; then
    pass "without the flag, both desktop and palette apply (not the default)"
else fail "default set no longer applies both desktop and palette"; fi

# --- OWNER FIX: preview -w/--wallpaper equals positional; banner is gone ----
pv_pos=$(COLUMNS=100 run_nokitty "$lib" preview tiny 2>&1)
pv_flag=$(COLUMNS=100 run_nokitty "$lib" preview -w tiny 2>&1)
pv_flag2=$(COLUMNS=100 run_nokitty "$lib" preview --wallpaper=tiny 2>&1)
if [ "$pv_pos" = "$pv_flag" ] && [ "$pv_pos" = "$pv_flag2" ]; then
    pass "preview -w/--wallpaper matches the positional form"
else fail "preview flag and positional forms diverge"; fi
if printf '%s' "$pv_pos" | grep -qi 'wallpaper preview'; then
    fail "the 'wallpaper preview' banner is back"
else pass "the preview banner is gone"; fi
if printf '%s' "$pv_pos" | grep -q 'TITLE        tiny'; then
    pass "preview still shows the labeled block"
else fail "preview block lost its labels"; fi

# --- OWNER FIX: THEME_WALLPAPER_DIR is a colon-separated list ---------------
lib2="$fixture/library2"
mkdir -p "$lib2"
printf 'x' >"$lib2/second-dir-only.jpg"
png1x1 "$lib2/dupname.png" 10 20 30
png1x1 "$lib/dupname.png" 40 50 60
multi="$lib:$lib2"
check  "a name in the SECOND dir resolves"      0 run "$multi" preview second-dir-only
check  "rm resolves into the second dir too"    0 run "$multi" rm second-dir-only
exists "second-dir file really deleted"         no "$lib2/second-dir-only.jpg"
# COLUMNS wide enough that the list stays on one line — narrow terminals
# wrap it with a hanging indent now (issue #19), pinned in its own section.
mstat=$(COLUMNS=500 run_nokitty "$multi" status 2>&1)
if printf '%s' "$mstat" | grep -qF "$multi"; then
    pass "status shows the whole directory list"
else fail "status hides the extra library dirs"; fi
# Same basename in both dirs: a bare stem is ambiguous across the list.
check  "same stem across dirs refuses rm"       1 run "$multi" rm dupname
exists "first-dir dupname intact"               yes "$lib/dupname.png"
exists "second-dir dupname intact"              yes "$lib2/dupname.png"

# --- long values WRAP with a hanging indent — never merging with a label ----
if xattr_meta; then
png1x1 "$lib/long-src.png" 0 0 0
xattr -w theme.source "https://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bb.invalid/x" "$lib/long-src.png" 2>/dev/null
pv_out=$(COLUMNS=60 run_nokitty "$lib" preview long-src 2>/dev/null)
if printf '%s' "$pv_out" | grep -q '^  SOURCE       aaaa'; then
    pass "the long source starts in the value column"
else fail "the long source lost its label line"; fi
if printf '%s' "$pv_out" | grep -q '^               nvalid$'; then
    pass "the wrapped remainder carries the hanging indent"
else fail "a wrapped value landed outside the value column"; fi
if printf '%s\n' "$pv_out" | awk 'length($0) > 60 { exit 1 }'; then
    pass "no preview line exceeds the terminal width"
else fail "a preview line overran COLUMNS"; fi
if printf '%s' "$pv_out" | grep -q '^a'; then
    fail "a wrapped value merged into column 0 (the wrap-tangle)"
else pass "no wrapped value ever reaches column 0"; fi
lv_out=$(COLUMNS=100 run_nokitty "$lib" list -v --all 2>/dev/null)
if printf '%s' "$lv_out" | grep -q 'aaaaaaaaa…  png'; then
    pass "list -v bounds the SOURCE field"
else fail "list -v SOURCE field shifted later columns"; fi
png1x1 "$lib/ctrl-src.png" 0 0 0
xattr -w theme.source "$(printf 'bad\nline\airl')" "$lib/ctrl-src.png" 2>/dev/null
pv_out=$(COLUMNS=80 run_nokitty "$lib" preview ctrl-src 2>/dev/null)
if printf '%s' "$pv_out" | grep -q 'SOURCE       badlineirl'; then
    pass "control bytes in the source xattr are stripped"
else fail "control bytes reached the preview table"; fi
fi

# --- OWNER: only populated fields render; hostile metadata xattrs are inert -
png1x1 "$lib/bare-meta.png" 5 5 5
pv_out=$(COLUMNS=80 run_nokitty "$lib" preview bare-meta 2>/dev/null)
if printf '%s' "$pv_out" | grep -qE '^  (ARTIST|PUBLISHED|CAMERA|PLACE|LICENSE)'; then
    fail "an empty metadata field rendered for a bare file"
else pass "empty metadata fields are omitted, not rendered blank"; fi
if printf '%s' "$pv_out" | grep -q '^  TITLE        bare-meta' \
   && printf '%s' "$pv_out" | grep -q '^  SIZE' \
   && printf '%s' "$pv_out" | grep -q '^  LOCATION'; then
    pass "a bare file still renders its filesystem facts"
else fail "preview lost a filesystem fact on a bare file"; fi
if xattr_meta; then
xattr -w theme.artist "$(printf 'Ev\033]52;c;steal\ail Artist')" "$lib/bare-meta.png" 2>/dev/null
pv_out=$(COLUMNS=80 run_nokitty "$lib" preview bare-meta 2>/dev/null)
if printf '%s' "$pv_out" | grep -qF "$(printf '\033]')"; then
    fail "a metadata xattr smuggled terminal protocol into preview"
else pass "hostile metadata cannot emit terminal protocol"; fi
if printf '%s' "$pv_out" | grep -q '^  ARTIST       Ev]52;c;stealil Artist$'; then
    pass "the sanitized artist value still renders"
else fail "the artist metadata line was lost or mangled"; fi
fi
rm -f "$lib/bare-meta.png"
rm -f "$lib/long-src.png" "$lib/ctrl-src.png" "$lib/dupname.png" "$lib2/dupname.png"

# --- list -v outside kitty must still render -------------------------------
check  "list -v renders without kitty"          0 run_nokitty "$lib" list -v

# --- scratch hygiene: no temp file left, success or failure ----------------
check  "transformed local set succeeds"        0 run "$lib" set tiny.png --rotate right
check  "transform of a broken image fails"     1 run "$lib" set broken.jpg --rotate right
leftovers=$(find "$fixture/tmpdir" -type f 2>/dev/null | wc -l | tr -d ' ')
if [ "$leftovers" = "0" ]; then pass "no scratch files leaked"; else fail "scratch leak: $leftovers file(s) left in TMPDIR"; fi

# --- unsplash boundary, against the deterministic curl stub -----------------
stubdir="$fixture/bin"
mkdir -p "$stubdir"
cat >"$stubdir/curl" <<'EOS'
#!/bin/bash
printf 'ARGV: %s\n' "$*" >>"${CURL_LOG:?}"
url="" out="" prev="" kdash=0
for a in "$@"; do
    case "$a" in http://* | https://*) url="$a" ;; esac
    [ "$prev" = "-o" ] && out="$a"
    [ "$prev" = "--url" ] && url="$a"
    [ "$prev" = "-K" ] && [ "$a" = "-" ] && kdash=1
    prev="$a"
done
cfg=""
[ "$kdash" = 1 ] && cfg=$(cat)
case "$cfg" in
*stub-sentinel-key*) printf 'KEYTO %s\n' "$url" >>"$CURL_LOG" ;;
*stub-user-token*) printf 'TOKTO %s\n' "$url" >>"$CURL_LOG" ;;
esac
case "$url" in
*api.unsplash.com/photos/stub123/download*)
    printf '{"url": "%s"}' "${STUB_ENTITLED:-https://delivery.unsplash.com/entitled-bytes}"
    ;;
*api.unsplash.com/photos*)
    STUB_DESC="${STUB_DESC:-stub photo of a boundary test}" \
    STUB_DL="${STUB_DL:-https://api.unsplash.com/photos/stub123/download}" \
    STUB_WHO="${STUB_WHO:-Stub}" \
        python3 -c 'import json, os, sys
sys.stdout.write(json.dumps({
    "id": "stub123", "slug": "stub-photo-stub1234567",
    "alt_description": os.environ["STUB_DESC"],
    "width": 3840, "height": 2160,
    "urls": {"raw": os.environ.get("STUB_RAW", "https://img.invalid/raw"),
             "full": "https://img.invalid/full"},
    "links": {"download_location": os.environ["STUB_DL"]},
    "user": {"name": os.environ["STUB_WHO"]}}))'
    ;;
*img.invalid/* | *plus.unsplash.com/* | *delivery.unsplash.com/*)
    printf 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==' |
        base64 -d >"${out:?}"
    ;;
esac
exit 0
EOS
chmod +x "$stubdir/curl"
printf '#!/bin/bash\nexit 1\n' >"$stubdir/security"
chmod +x "$stubdir/security"

run_stub() { CURL_LOG="$1" PATH="$stubdir:$PATH" UNSPLASH_ACCESS_KEY=stub-sentinel-key \
    UNSPLASH_USER_TOKEN="${STUB_TOKEN:-}" UNSPLASH_SECRET_KEY="" \
    THEME_WALLPAPER_DIR="$lib" THEME_NO_APPLY=1 THEME_CACHE_DIR="$fixture/cache" \
    TMPDIR="$fixture/tmpdir" "$THEME" "${@:2}"; }

goodlog="$fixture/curl-good.log"; : >"$goodlog"
check  "unsplash photo-page URL accepted"      0 run_stub "$goodlog" unsplash https://unsplash.com/photos/winged-slug-coy_MhYMLHs
apihits=$(grep -c '^ARGV: .*api\.unsplash\.com/photos/winged-slug-coy_MhYMLHs' "$goodlog")
if [ "$apihits" = 1 ]; then pass "exactly one authenticated API request"; else fail "expected 1 API request, saw $apihits"; fi
if grep -q 'stub-sentinel-key' "$goodlog"; then fail "key leaked into curl argv"; else pass "key never in curl argv"; fi
if grep 'api\.unsplash\.com/photos/winged-slug' "$goodlog" | grep -q '^ARGV: -fsLg '; then
    pass "curl globbing off on the API request"
else fail "API request missing -g (globoff)"; fi
exists "photo saved under its description"     yes "$lib/unsplash/stub-photo-of-a-boundary-test.png"
exists "unsplash download not at library root" no  "$lib/stub-photo-of-a-boundary-test.png"

evillog="$fixture/curl-evil.log"; : >"$evillog"
check  "lookalike host refused"                1 run_stub "$evillog" unsplash https://evilunsplash.com/photos/x
check  "http (non-TLS) unsplash link refused"  1 run_stub "$evillog" unsplash http://unsplash.com/photos/abc
check  "glob-range slug refused"               1 run_stub "$evillog" unsplash "https://unsplash.com/photos/[1-3]"
if [ -s "$evillog" ]; then fail "refused inputs still reached curl"; else pass "refused inputs never reach curl"; fi

searchlog="$fixture/curl-search.log"; : >"$searchlog"
check  "hostile search query accepted"         0 run_stub "$searchlog" unsplash "cats&count=50[1-3]"
if grep -qF -- '--data-urlencode query=cats&count=50[1-3]' "$searchlog"; then
    pass "search text stays one literal encoded value"
else fail "hostile search not passed through --data-urlencode"; fi

dllog="$fixture/curl-dl.log"; : >"$dllog"
check  "clean photo: fetch + report succeed"   0 run_stub "$dllog" unsplash https://unsplash.com/photos/clean-slug-abcdef12345
dlhits=$(grep -c '^KEYTO https://api\.unsplash\.com/photos/stub123/download$' "$dllog")
if [ "$dlhits" = 1 ]; then pass "legit download endpoint reported exactly once"; else fail "expected 1 authenticated download report, saw $dlhits"; fi

STUB_DESC=$(printf 'safe name\thttps://evil.invalid/steal\nsecond line')
export STUB_DESC
tablog="$fixture/curl-tab.log"; : >"$tablog"
check  "tab/newline description still saves"   0 run_stub "$tablog" unsplash https://unsplash.com/photos/tabby-slug-abcdef12345
if grep '^KEYTO ' "$tablog" | grep -qv '^KEYTO https://api\.unsplash\.com/'; then
    fail "key sent off-host under a crafted description"
else pass "crafted description cannot retarget the key"; fi
tabkeys=$(grep -c '^KEYTO ' "$tablog")
tabreport=$(grep -c '^KEYTO https://api\.unsplash\.com/photos/stub123/download$' "$tablog")
if [ "$tabreport" = 1 ] && [ "$tabkeys" = 2 ]; then
    pass "crafted description leaves the report on its own target"
else fail "crafted description shifted the report ($tabreport legit of $tabkeys authenticated calls)"; fi
unset STUB_DESC

run_stub_tok() { STUB_TOKEN=stub-user-token run_stub "$@"; }
STUB_RAW="https://plus.unsplash.com/premium-raw-stub"
STUB_DESC="premium boundary test"
export STUB_RAW STUB_DESC
premlog="$fixture/curl-prem.log"; : >"$premlog"
check  "premium + account token saves"         0 run_stub_tok "$premlog" unsplash https://unsplash.com/photos/premium-slug-abcdef12345
exists "premium photo saved"                   yes "$lib/unsplash/premium-boundary-test.png"
if grep -q '^ARGV: .*delivery\.unsplash\.com/entitled-bytes' "$premlog" &&
    ! grep -q '^ARGV: .*plus\.unsplash\.com' "$premlog"; then
    pass "binary fetched from the entitled answer, not the watermarked raw"
else fail "entitled download did not replace the raw rendition"; fi
tokdl=$(grep -c '^TOKTO https://api\.unsplash\.com/photos/stub123/download$' "$premlog")
if [ "$tokdl" = 1 ]; then pass "entitled call doubles as the report — exactly one"
else fail "expected 1 authenticated download call, saw $tokdl"; fi
STUB_ENTITLED="https://evil.invalid/steal-the-fetch"
export STUB_ENTITLED
premevil="$fixture/curl-premevil.log"; : >"$premevil"
check  "hostile entitled answer tolerated"     0 run_stub_tok "$premevil" unsplash https://unsplash.com/photos/premium-slug-abcdef12345
if grep '^ARGV: ' "$premevil" | grep -q 'evil\.invalid'; then
    fail "a hostile entitled URL was fetched"
elif grep -q '^ARGV: .*plus\.unsplash\.com' "$premevil"; then
    pass "hostile entitled URL refused; fell back to the standard rendition"
else fail "hostile entitled URL: no fallback fetch happened"; fi
unset STUB_ENTITLED STUB_RAW STUB_DESC

STUB_DL="https://evil.invalid/dl"
export STUB_DL
evildllog="$fixture/curl-evildl.log"; : >"$evildllog"
check  "malicious download_location tolerated" 0 run_stub "$evildllog" unsplash https://unsplash.com/photos/evil-dl-abcdef123456
if grep '^KEYTO ' "$evildllog" | grep -qv '^KEYTO https://api\.unsplash\.com/'; then
    fail "key followed a non-api download_location"
else pass "non-api download_location never receives the key"; fi
unset STUB_DL

# --- a credential can never inject a curl CONFIG DIRECTIVE ------------------
credbin="$fixture/credbin"
mkdir -p "$credbin"
cat >"$credbin/curl" <<'EOS'
#!/bin/sh
printf 'TRANSFER %s\n' "$*" >>"${CURL_CALL_LOG:?}"
exit 0
EOS
chmod +x "$credbin/curl"
printf '#!/bin/sh\nexit 0\n' >"$credbin/open"; chmod +x "$credbin/open"
printf '#!/bin/sh\nexit 1\n' >"$credbin/security"; chmod +x "$credbin/security"
credlog="$fixture/curl-cred.log"
hostile_cred='goodtoken"
url = "http://127.0.0.1:9/injected-second-transfer'
cred_run() { # $1 label, $2.. env assignments then -- then argv
    local label="$1"; shift
    local envs=() ; while [ "$1" != "--" ]; do envs+=("$1"); shift; done; shift
    : >"$credlog"
    local o
    o=$(env "${envs[@]}" CURL_CALL_LOG="$credlog" PATH="$credbin:$PATH" \
        THEME_WALLPAPER_DIR="$lib" THEME_NO_APPLY=1 THEME_CACHE_DIR="$fixture/cache" \
        TMPDIR="$fixture/tmpdir" "$THEME" "$@" 2>&1 </dev/null)
    local n; n=$(wc -l <"$credlog" | tr -d ' ')
    if [ "$n" = 0 ]; then pass "$label reaches curl zero times"
    else fail "$label produced $n curl transfer(s) — config-directive injection"; fi
    case "$o" in
    *"cannot occur in an Unsplash credential"*) pass "$label is refused by the credential grammar" ;;
    *) fail "$label was not refused by the credential grammar" ;;
    esac
}
cred_run "a quote+newline account token" \
    "UNSPLASH_USER_TOKEN=$hostile_cred" UNSPLASH_ACCESS_KEY=validkey -- unsplash random
cred_run "a quote+newline access key" \
    UNSPLASH_USER_TOKEN= "UNSPLASH_ACCESS_KEY=$hostile_cred" -- unsplash random
cred_run "a quote+newline token on status" \
    "UNSPLASH_USER_TOKEN=$hostile_cred" UNSPLASH_ACCESS_KEY=validkey -- unsplash status
cred_run "a quote+newline key on the auth exchange" \
    "UNSPLASH_ACCESS_KEY=$hostile_cred" UNSPLASH_SECRET_KEY=sec -- unsplash auth
cred_run "a quote+newline app secret" \
    UNSPLASH_ACCESS_KEY=validkey "UNSPLASH_SECRET_KEY=$hostile_cred" -- unsplash auth
: >"$credlog"
env UNSPLASH_USER_TOKEN='abc-DEF_123.tok~x+y/z=' UNSPLASH_ACCESS_KEY=validkey \
    CURL_CALL_LOG="$credlog" PATH="$credbin:$PATH" THEME_WALLPAPER_DIR="$lib" \
    THEME_NO_APPLY=1 THEME_CACHE_DIR="$fixture/cache" TMPDIR="$fixture/tmpdir" \
    "$THEME" unsplash random >/dev/null 2>&1
if [ "$(wc -l <"$credlog" | tr -d ' ')" -ge 1 ]; then
    pass "a legitimate token is not refused by the grammar"
else fail "the credential grammar refused a legitimate token"; fi

# --- the scheme column reports the pigment cache, never a guess -------------
schemelib="$fixture/schemelib"; schemecache="$fixture/schemecache"
mkdir -p "$schemelib" "$schemecache"
png1x1 "$schemelib/red-one.png" 200 30 30
png1x1 "$schemelib/blue-one.png" 30 30 200
png1x1 "$schemelib/never-derived.png" 30 200 30
run_scheme_list() { COLUMNS=200 THEME_WALLPAPER_DIR="$schemelib" THEME_CACHE_DIR="$schemecache" \
    KITTY_WINDOW_ID='' TMPDIR="$fixture/tmpdir" "$THEME" list --all; }
# Under NO_APPLY nothing derives: every row is an honest dash.
noapply_out=$(THEME_NO_APPLY=1 run_scheme_list 2>/dev/null)
if printf '%s' "$noapply_out" | grep -q '48;2;'; then
    fail "NO_APPLY list invented colors with an empty cache"
else pass "an underived wallpaper lists an honest dash"; fi
# A live list derives what it shows (real backfill, scratch cache only) —
# and derives DETERMINISTICALLY: two cached runs agree byte-for-byte (the
# first run additionally announces the one-time backfill on stdout).
live1=$(run_scheme_list 2>/dev/null)
live2=$(run_scheme_list 2>/dev/null)
live1=$(printf '%s' "$live1" | grep -v 'missing colorscheme')
if printf '%s' "$live1" | grep '^  red-one' | grep -q '48;2;' &&
    printf '%s' "$live1" | grep '^  blue-one' | grep -q '48;2;'; then
    pass "backfill derives a scheme for every shown wallpaper"
else fail "backfill left listed wallpapers without schemes"; fi
if [ "$live1" = "$live2" ]; then
    pass "derivation is deterministic across runs"
else fail "two identical list runs disagreed"; fi
redrow=$(printf '%s' "$live1" | grep '^  red-one')
bluerow=$(printf '%s' "$live1" | grep '^  blue-one')
if [ -n "$redrow" ] && [ "$redrow" != "$bluerow" ]; then
    pass "different wallpapers carry different schemes (attribution by key)"
else fail "two different wallpapers rendered identical scheme rows"; fi
# A corrupt cache entry degrades to a dash, silently.
for f in "$schemecache"/schemes/*; do printf 'garbage' >"$f"; done
corrupt_out=$(THEME_NO_APPLY=1 run_scheme_list 2>"$fixture/corrupt.err")
if printf '%s' "$corrupt_out" | grep -q '48;2;' || [ -s "$fixture/corrupt.err" ]; then
    fail "a corrupt cache entry produced a swatch or an error"
else pass "corrupt cache entry degrades to a dash, silently"; fi

# --- `search` answers from FACTS, and from the same cache ------------------
# The corrupt entries above are still in place, so the first case proves the
# colors come from a derived scheme and are never invented; the live runs
# after it re-derive. Solid-color PNGs make every answer here exact.
run_scheme_search() { COLUMNS=200 THEME_WALLPAPER_DIR="$schemelib" THEME_CACHE_DIR="$schemecache" \
    KITTY_WINDOW_ID='' TMPDIR="$fixture/tmpdir" "$THEME" search "$@"; }
if THEME_NO_APPLY=1 run_scheme_search green --all 2>/dev/null | grep -q 'no wallpaper matches'; then
    pass "a color word finds nothing while no scheme is derived"
else fail "search invented a color word from an underived scheme"; fi
srch=$(run_scheme_search red-one --all 2>/dev/null)
if printf '%s' "$srch" | grep -q '^  red-one .*title: red-one' &&
    printf '%s' "$srch" | grep -q '^  1 of 3 wallpapers match$'; then
    pass "a title substring finds exactly its wallpaper"
else fail "the title search missed or over-matched"; fi
if run_scheme_search red-one png --all 2>/dev/null | grep -q 'title: red-one, format: png'; then
    pass "two terms AND together, each naming the fact it landed on"
else fail "two terms did not AND together"; fi
if run_scheme_search red-one qqqq --all 2>/dev/null |
    grep -q 'no wallpaper matches "red-one qqqq"'; then
    pass "one unanswerable term eliminates every wallpaper"
else fail "an unanswerable term did not eliminate"; fi
check  "a search with no hits is not an error"  0 run_scheme_search red-one qqqq --all
if run_scheme_search green --all 2>/dev/null | grep -q '^  red-one .*colors: .*green'; then
    pass "a color word matches the backfilled scheme that holds it"
else fail "the backfilled green accent was not searchable"; fi
# c81e1e IS red-one.png's own color; its derived red accent sits 43 away.
if run_scheme_search c81e1e --all 2>/dev/null | grep -q '^  red-one .*colors: #c81e1e'; then
    pass "a hex term matches a scheme color by distance"
else fail "a hex term did not match a near scheme color"; fi
if run_scheme_search 808080 --all 2>/dev/null | grep -q 'no wallpaper matches'; then
    pass "a hex term far from every scheme color matches nothing"
else fail "the hex distance gate let a far color through"; fi
capped=$(run_scheme_search png -n 2 2>/dev/null)
if printf '%s' "$capped" | grep -q '^  2 of 3 matches shown — more:' &&
    ! printf '%s' "$capped" | grep -q 'wallpapers match'; then
    pass "a capped table counts the rows shown, not the matches found"
else fail "the capped footer claimed the whole match count"; fi

# --- the DEFAULT listing is bounded: newest 10, honestly labelled -----------
for i in 1 2 3 4 5 6 7 8 9 10 11 12; do printf 'x' >"$lib/bulk-$i.jpg"; done
if run_nokitty "$lib" list 2>/dev/null | grep -q 'newest 10 of [0-9]* — more:'; then
    pass "default list bounds to the newest 10 with an honest footer"
else fail "default list bound or footer missing"; fi
rm -f "$lib"/bulk-*.jpg

# --- ADDED and the newest-first order are a SYSCALL, not a spawn ------------
# The sort key is a birth time in SECONDS, so the three files have to be born
# in different seconds for their order to be a fact rather than a coincidence
# — that is what the two sleeps buy. `stat` is shadowed by a counting stub:
# the plain listing used to spend ~2·n·log₂n spawns of it on a key it never
# printed, and must now spend none. The verbose listing keeps exactly one per
# PRINTED date on macOS, where the date is spelled in LOCAL time.
birthlib="$fixture/birthlib"; birthcache="$fixture/birthcache"
statbin="$fixture/statbin"; statlog="$fixture/stat-spawns"
mkdir -p "$birthlib" "$birthcache" "$statbin"
png1x1 "$birthlib/born-first.png" 10 10 10
sleep 1
png1x1 "$birthlib/born-second.png" 20 20 20
sleep 1
png1x1 "$birthlib/born-third.png" 30 30 30
cat >"$statbin/stat" <<EOS
#!/bin/sh
printf '%s\n' "\$*" >>"$statlog"
exec /usr/bin/stat "\$@"
EOS
chmod +x "$statbin/stat"
birth_run() { COLUMNS=200 THEME_WALLPAPER_DIR="$birthlib" THEME_CACHE_DIR="$birthcache" \
    KITTY_WINDOW_ID='' THEME_NO_APPLY=1 TMPDIR="$fixture/tmpdir" \
    PATH="$statbin:$PATH" "$THEME" "$@"; }
: >"$statlog"
birth_order=$(birth_run list --all 2>/dev/null | grep -o 'born-[a-z]*')
if [ "$birth_order" = "$(printf 'born-third\nborn-second\nborn-first')" ]; then
    pass "the listing is newest-birth-first"
else fail "birth order wrong: $(printf '%s' "$birth_order" | tr '\n' ' ')"; fi
statn=$(wc -l <"$statlog" | tr -d ' ')
if [ "$statn" = 0 ]; then pass "a plain listing spawns stat zero times"
else fail "a plain listing spawned stat $statn times"; fi
case "$(uname -s)" in
    Darwin) want_added=$(/usr/bin/stat -f '%SB' -t '%Y-%m-%d' "$birthlib/born-third.png") ;;
    *) want_added=$(date -u +%F) ;;
esac
: >"$statlog"
got_added=$(birth_run list -v --all 2>/dev/null | grep 'born-third' | awk '{print $NF}')
if [ "$got_added" = "$want_added" ]; then
    pass "ADDED is the file's own birth date ($want_added)"
else fail "ADDED said '$got_added', the birth date is '$want_added'"; fi
statn=$(wc -l <"$statlog" | tr -d ' ')
case "$(uname -s)" in
    Darwin)
        if [ "$statn" = 3 ]; then pass "a verbose listing spawns stat once per printed date"
        else fail "a verbose listing spawned stat $statn times, not once per row"; fi ;;
    *)
        if [ "$statn" = 0 ]; then pass "a verbose listing spawns stat zero times"
        else fail "a verbose listing spawned stat $statn times"; fi ;;
esac

# --- the desktop fact is asked ONCE per desktop change ---------------------
# macOS keeps every Space's choice in ONE store file under HOME, so a fixture
# HOME with a planted store is a whole desktop world. The helper's answer is
# recorded against that file's IDENTITY: while it holds, `wallpaper get` (57
# ms, the whole cost of the bare screen) must not run; when it moves, it must.
if [ "$(uname -s)" = Darwin ]; then
    dhome="$fixture/deskhome"; dcache="$fixture/deskcache"; dbin="$fixture/deskbin"
    dstore="$dhome/Library/Application Support/com.apple.wallpaper/Store"
    dran="$fixture/wallpaper-ran"
    mkdir -p "$dstore" "$dcache" "$dbin"
    printf 'store\n' >"$dstore/Index.plist"
    cat >"$dbin/wallpaper" <<EOS
#!/bin/sh
: >"$dran"
printf '%s\n' "$lib/tiny.png"
EOS
    chmod +x "$dbin/wallpaper"
    desk_run() { HOME="$dhome" PATH="$dbin:$PATH" COLUMNS=400 KITTY_WINDOW_ID='' \
        THEME_WALLPAPER_DIR="$lib" THEME_CACHE_DIR="$dcache" THEME_NO_APPLY=1 \
        TMPDIR="$fixture/tmpdir" "$THEME" "$@"; }
    rm -f "$dran" "$dcache/desktop"
    desk_cold=$(desk_run 2>&1)
    if [ -e "$dran" ] && printf '%s' "$desk_cold" | grep -q '^  tiny$'; then
        pass "with no record the helper is asked, and its answer opens the screen"
    else fail "the first bare screen did not ask the helper"; fi
    rm -f "$dran"
    desk_warm=$(desk_run 2>&1)
    if [ ! -e "$dran" ] && [ "$desk_warm" = "$desk_cold" ]; then
        pass "an unchanged store answers from the record: no spawn, same screen"
    else fail "the record was not used, or it changed the screen"; fi
    rm -f "$dran"
    if desk_run status 2>/dev/null | grep -q "current theme:   $lib/tiny.png" &&
        [ ! -e "$dran" ]; then
        pass "status reads the same record, and says the same thing"
    else fail "status asked the helper again, or disagreed with the bare screen"; fi
    # PRINT-ONLY: `preview` with no name OPENS the file it is handed and
    # renders its bytes, so it asks the helper every time — behind the very
    # record the bare screen just answered from.
    rm -f "$dran"
    if desk_run preview >/dev/null 2>&1 && [ -e "$dran" ]; then
        pass "preview with no name asks the helper, record or no record"
    else fail "preview with no name read the recorded path"; fi
    # Whatever changes the desktop moves the store; the record expires with
    # it — on the file's identity, never on a clock we keep ourselves.
    printf 'store-has-moved\n' >"$dstore/Index.plist"
    rm -f "$dran"
    desk_moved=$(desk_run 2>&1)
    if [ -e "$dran" ] && [ "$desk_moved" = "$desk_cold" ]; then
        pass "a moved store re-asks the helper"
    else fail "a moved store did not re-ask the helper"; fi
    # A cache that cannot be written costs speed, never correctness.
    rm -f "$dcache/desktop"
    chmod 500 "$dcache"
    rm -f "$dran"
    desk_ro=$(desk_run 2>&1)
    chmod 700 "$dcache"
    if [ -e "$dran" ] && [ "$desk_ro" = "$desk_cold" ]; then
        pass "an unwritable cache falls back to asking, with the same screen"
    else fail "an unwritable cache broke the bare screen"; fi
fi

# --- the save path's trust chain, end-to-end through the binary -------------
# (The check/use race windows and the forced-posix ACL predicate are driven
# natively in src/save_tests.rs; these are the end-to-end shapes.)
urlbin="$fixture/urlbin"
mkdir -p "$urlbin"
cat >"$urlbin/curl" <<'EOS'
#!/bin/bash
o=""; prev=""
[ -n "${CURL_LOG-}" ] && printf 'ARGV: %s\n' "$*" >>"$CURL_LOG"
for a in "$@"; do [ "$prev" = "-o" ] && o="$a"; prev="$a"; done
[ -n "$o" ] && printf '%s' 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==' | base64 -d >"$o"
exit 0
EOS
chmod +x "$urlbin/curl"
run_url() { PATH="$urlbin:$PATH" THEME_WALLPAPER_DIR="$1" THEME_NO_APPLY=1 \
    THEME_CACHE_DIR="$fixture/cache" TMPDIR="$fixture/tmpdir" "$THEME" set "$2"; }

# A dangling symlink is an occupied name, not a free one.
ln -s "$out/redirected.png" "$lib/hijacked.png"
check  "download onto a hijacked name succeeds" 0 run_url "$lib" https://img.invalid/hijacked.png
exists "nothing written through the symlink"   no "$out/redirected.png"
exists "download landed inside the library"    yes "$lib/hijacked-2.png"
if [ -L "$lib/hijacked.png" ]; then pass "the hijacking symlink is left alone"
else fail "the hijacking symlink was replaced or removed"; fi

# A symlinked provider folder receives nothing.
symlib="$fixture/subdir-lib"; symout="$fixture/subdir-out"
mkdir -p "$symlib" "$symout"
ln -s "$symout" "$symlib/unsplash"
symerr=$(run_url "$symlib" https://images.unsplash.com/pic.png 2>&1)
if [ "$(find "$symout" -type f | wc -l | tr -d ' ')" = 0 ]; then
    pass "a symlinked provider folder receives nothing"
else fail "the download escaped through the symlinked provider folder"; fi
if [ "$(find -P "$symlib" -type f | wc -l | tr -d ' ')" = 0 ]; then
    pass "and nothing was written into the library either"
else fail "a file appeared in the library after the refusal"; fi
if [ -L "$symlib/unsplash" ]; then pass "the planted provider symlink is left alone"
else fail "the planted provider symlink was replaced or removed"; fi
case "$symerr" in
*"refusing to save"*) pass "the refusal says it is refusing to save" ;;
*) fail "the parent-symlink refusal was silent or unclear" ;;
esac
# An ordinary provider folder still works.
okslib="$fixture/subdir-ok"; mkdir -p "$okslib"
run_url "$okslib" https://images.unsplash.com/fine.png >/dev/null 2>&1
exists "an ordinary provider folder still receives the download" yes "$okslib/unsplash/fine.png"

# --- a transformed WebP re-encodes as PNG: the mime is re-read AFTER the
# transform, so the save is named by the actual bytes and the width warning
# still fires (it reads dimensions by extension). Stub serves a real 6x4 webp.
webpbin="$fixture/webpbin"
mkdir -p "$webpbin"
cat >"$webpbin/curl" <<'EOS'
#!/bin/bash
o=""; prev=""
for a in "$@"; do [ "$prev" = "-o" ] && o="$a"; prev="$a"; done
[ -n "$o" ] && printf '%s' 'UklGRi4AAABXRUJQVlA4TCIAAAAvBcAAAB8wHkUZ5PmPA4CCRpKa7x1Qh0lMYiOi/wFw9dU/' | base64 -d >"$o"
exit 0
EOS
chmod +x "$webpbin/curl"
webplib="$fixture/webplib"; mkdir -p "$webplib"
run_webp() { PATH="$webpbin:$PATH" THEME_WALLPAPER_DIR="$webplib" THEME_NO_APPLY=1 \
    THEME_CACHE_DIR="$fixture/cache" TMPDIR="$fixture/tmpdir" "$THEME" set "$@"; }
webpout=$(run_webp https://img.invalid/photo.webp --rotate right 2>&1)
exists "a transformed webp is saved as png"    yes "$webplib/photo-rotated-right.png"
case "$webpout" in
*"below the 2560px desktop floor"*) pass "the width warning fires on the post-transform bytes" ;;
*) fail "the width warning vanished after a webp transform" ;;
esac
plainout=$(run_webp https://img.invalid/plain.webp 2>&1)
exists "an untransformed webp keeps .webp"     yes "$webplib/plain.webp"
case "$plainout" in
*"below the 2560px desktop floor"*) pass "the untransformed control warns too" ;;
*) fail "the untransformed webp control lost its width warning" ;;
esac

# --- SSRF: a page-controlled og:image must be a PUBLIC http(s) target, never
# file:, an option-shaped value, or the loopback/private network. The stub
# keys off --url: it serves HTML (with a chosen og:image) for a page request
# and a PNG for an image request, so the ok case resolves one hop and saves.
ssrfbin="$fixture/ssrfbin"
mkdir -p "$ssrfbin"
cat >"$ssrfbin/curl" <<'EOS'
#!/bin/bash
o=""; url=""; prev=""
for a in "$@"; do
  [ "$prev" = "-o" ] && o="$a"
  [ "$prev" = "--url" ] && url="$a"
  prev="$a"
done
html='<!DOCTYPE html><html><head><meta property="og:image" content='
case "$url" in
  *page-file*) body="${html}\"file:///etc/hosts\"></head></html>" ;;
  *page-loop*) body="${html}\"http://127.0.0.1/secret.png\"></head></html>" ;;
  *page-opt*)  body="${html}\"-O/tmp/pwned\"></head></html>" ;;
  # Decimal spelling of 127.0.0.1: passes the string gate, caught by the
  # resolve-and-vet (getaddrinfo parses it locally, no network).
  *page-dec*)  body="${html}\"http://2130706433/secret.png\"></head></html>" ;;
  # A PUBLIC IP LITERAL keeps this hermetic: vet resolves it without DNS.
  *page-ok*)   body="${html}\"https://93.184.216.34/real.png\"></head></html>" ;;
  *) [ -n "$o" ] && printf '%s' 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==' | base64 -d >"$o"; exit 0 ;;
esac
[ -n "$o" ] && printf '%s' "$body" >"$o"
exit 0
EOS
chmod +x "$ssrfbin/curl"
ssrflib="$fixture/ssrflib"; mkdir -p "$ssrflib"
run_ssrf() { PATH="$ssrfbin:$PATH" THEME_WALLPAPER_DIR="$ssrflib" THEME_NO_APPLY=1 \
    THEME_CACHE_DIR="$fixture/cache" TMPDIR="$fixture/tmpdir" "$THEME" set "$1"; }
for kind in file loop opt; do
    err=$(run_ssrf "https://pin.example/page-$kind" 2>&1)
    case "$err" in
    *"not a public http(s) image URL"*) pass "og:image ($kind) refused before any fetch" ;;
    *) fail "og:image ($kind) was not refused: $err" ;;
    esac
done
# The decimal-IP spelling passes the string gate but the resolve-and-vet
# rejects the loopback it resolves to — refused, nothing fetched.
decerr=$(run_ssrf "https://pin.example/page-dec" 2>&1)
case "$decerr" in
*"resolves to a non-public address"*) pass "og:image (decimal-IP) refused after resolution" ;;
*) fail "decimal-IP og:image was not refused: $decerr" ;;
esac
if [ "$(find "$ssrflib" -type f 2>/dev/null | wc -l | tr -d ' ')" = 0 ]; then
    pass "no SSRF og:image wrote anything into the library"
else fail "an SSRF og:image landed a file in the library"; fi
run_ssrf "https://pin.example/page-ok" >/dev/null 2>&1
# The page host (pin.example) is not a known provider, so the save keeps the
# library root — routing is by the served PAGE host, unchanged by this round.
exists "a public https og:image still resolves and saves" yes "$ssrflib/real.png"

# A world-writable ANCESTOR refuses the save (the whole chain is audited).
chaindir="$fixture/chain"; mkdir -p "$chaindir/parent/lib"
chmod 0777 "$chaindir/parent"
chainerr=$(run_url "$chaindir/parent/lib" https://images.unsplash.com/pic.png 2>&1)
chmod 0755 "$chaindir/parent"
case "$chainerr" in
*"group- or world-writable"*) pass "a world-writable ancestor is refused end-to-end" ;;
*) fail "a world-writable ancestor was accepted: $chainerr" ;;
esac
exists "and nothing was written under it" no "$chaindir/parent/lib/unsplash/pic.png"

# macOS ACL grants, end-to-end where chmod +a exists.
acldir="$fixture/acl"; mkdir -p "$acldir/lib"
if chmod +a "everyone allow add_file,delete_child" "$acldir/lib" 2>/dev/null; then
    aclerr=$(run_url "$acldir/lib" https://images.unsplash.com/pic.png 2>&1)
    chmod -a "everyone allow add_file,delete_child" "$acldir/lib" 2>/dev/null
    case "$aclerr" in
    *"ACL granting"*) pass "an ACL granting another principal write is refused end-to-end" ;;
    *) fail "an ACL-writable library was accepted: $aclerr" ;;
    esac
    denydir="$fixture/acl-deny"; mkdir -p "$denydir/lib"
    chmod +a "everyone deny delete" "$denydir/lib" 2>/dev/null
    check "a benign deny-delete ACL still saves" 0 run_url "$denydir/lib" https://images.unsplash.com/pic.png
    chmod -a "everyone deny delete" "$denydir/lib" 2>/dev/null
else
    echo "SKIP  darwin-native ACL cases: this platform has no chmod +a (src/save_tests.rs carries the forced-posix branch)"
fi

# An alias to identical bytes is still not that file.
printf '%s' 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==' | base64 -d >"$lib/aliastarget.png"
ln -s "$lib/aliastarget.png" "$lib/aliased.png"
check  "download onto an alias of identical bytes" 0 run_url "$lib" https://img.invalid/aliased.png
exists "the alias was stepped over, not adopted"   yes "$lib/aliased-2.png"
if [ -L "$lib/aliased.png" ]; then pass "the alias itself is left alone"
else fail "the alias was replaced by a regular file"; fi
check  "download onto a free name succeeds"    0 run_url "$lib" https://img.invalid/plain-name.png
if [ -f "$lib/plain-name.png" ] && [ ! -L "$lib/plain-name.png" ]; then
    pass "a free name yields a regular file in the library"
else fail "free-name download did not produce a regular library file"; fi

# --- `get`: the same download, stopping at the library ----------------------
# get lands a link's bytes and SHOWS them; applying is set's job alone. The
# retired `url` spelling explains itself instead of reading as a typo.
check  "the retired 'url' verb refuses"        1 run "$lib" url https://img.invalid/x.png
urlerr=$(run "$lib" url https://img.invalid/x.png 2>&1)
case "$urlerr" in
*"theme set"*) pass "the retired verb names its replacement" ;;
*) fail "the url refusal does not name 'theme set': $urlerr" ;;
esac
getlib="$fixture/getlib"; mkdir -p "$getlib"
getlog="$fixture/curl-get.log"; : >"$getlog"
run_get() { PATH="$urlbin:$PATH" THEME_WALLPAPER_DIR="$getlib" THEME_NO_APPLY=1 \
    THEME_CACHE_DIR="$fixture/cache" TMPDIR="$fixture/tmpdir" CURL_LOG="$getlog" \
    KITTY_WINDOW_ID='' COLUMNS=120 "$THEME" get "$@"; }
getout=$(run_get https://img.invalid/get-me.png 2>&1)
exists "get saves the download into the library"      yes "$getlib/get-me.png"
if printf '%s\n' "$getout" | grep -q '^  TITLE ' && printf '%s\n' "$getout" | grep -q '^  LOCATION '; then
    pass "get previews what it saved"
else fail "get printed no preview block: $getout"; fi
case "$getout" in
*"[no-apply] would"*) fail "get applied what it downloaded" ;;
*) pass "get changes no desktop and no palette" ;;
esac
exists "get exports no palette"                       no "$fixture/cache/wal"
run_get https://img.invalid/study.png --mkdir studies >/dev/null 2>&1
exists "--mkdir files the download in its own folder" yes "$getlib/studies/study.png"
: >"$getlog"
for bad in ../x .hidden a/b; do
    check "--mkdir '$bad' is refused" 1 run_get https://img.invalid/nope.png --mkdir "$bad"
done
check  "--mkdir swallows no following flag"    1 run_get https://img.invalid/nope.png --mkdir --rotate
check  "get refuses a library name"            1 run_get name-not-a-link
# A second positional is a grammar error, refused before any fetch — the same
# `theme get takes exactly one link` message the arm dies with pre-side-effect.
check  "'get' with an extra positional is refused" 1 run_get https://img.invalid/nope2.png unexpected
getextraerr=$(run_get https://img.invalid/nope2.png unexpected 2>&1)
case "$getextraerr" in
*"theme get takes exactly one link"*) pass "the extra-positional refusal names its cause" ;;
*) fail "the extra-positional refusal message is wrong: $getextraerr" ;;
esac
# A dangling --mkdir (nothing following it) is refused the same way rotate/-n
# already were — no value ever means no fetch.
check  "a dangling --mkdir on 'get' is refused"     1 run_get https://img.invalid/nope3.png --mkdir
getdangleerr=$(run_get https://img.invalid/nope3.png --mkdir 2>&1)
case "$getdangleerr" in
*"--mkdir takes one folder name"*) pass "the dangling --mkdir refusal names its cause" ;;
*) fail "the dangling --mkdir refusal message is wrong: $getdangleerr" ;;
esac
# A SECOND --mkdir that dangles must still refuse even though the first one
# already left a valid name behind — the bug this fixture pins.
check  "--mkdir studies --mkdir (re-dangled) is refused" 1 \
    run_get https://img.invalid/nope4.png --mkdir studies --mkdir
getredangleerr=$(run_get https://img.invalid/nope4.png --mkdir studies --mkdir 2>&1)
case "$getredangleerr" in
*"--mkdir takes one folder name"*) pass "the re-dangled --mkdir refusal names its cause" ;;
*) fail "the re-dangled --mkdir refusal message is wrong: $getredangleerr" ;;
esac
if [ ! -s "$getlog" ]; then pass "a refused get never reaches the network"
else fail "a refused get still ran curl: $(cat "$getlog")"; fi
# The well-formed grammar still downloads after the fix above.
run_get https://img.invalid/study2.png --mkdir studies2 >/dev/null 2>&1
exists "well-formed 'get --mkdir' still downloads" yes "$getlib/studies2/study2.png"
check  "rm refuses get's --mkdir"              1 run "$lib" rm keepme.jpg --mkdir x
exists "and the named victim survives"         yes "$lib/keepme.jpg"

# --- filenames are DATA, never terminal protocol ---------------------------
oscname="osc52-safe$(printf '\033]52;c;U0FGRQ==\007')"
png1x1 "$lib/$oscname.png" 0 0 0
osc_open=$(printf '\033]')
pv_osc=$(COLUMNS=120 run_nokitty "$lib" preview osc52- 2>/dev/null)
if printf '%s' "$pv_osc" | grep -qF -- "$osc_open"; then
    fail "preview emitted a filename's OSC sequence"
else pass "preview never emits a filename's OSC sequence"; fi
if printf '%s' "$pv_osc" | grep -q 'osc52-safe'; then
    pass "preview still shows the printable part of the title"
else fail "preview dropped the whole title instead of its control bytes"; fi
lv_osc=$(COLUMNS=200 run_nokitty "$lib" list --all 2>/dev/null)
if printf '%s' "$lv_osc" | grep -qF -- "$osc_open"; then
    fail "list emitted a filename's OSC sequence"
else pass "list never emits a filename's OSC sequence"; fi

# --- provenance is the parsed HOST, never a substring ----------------------
srccase() { # $1 description, $2 basename, $3 theme.source value, $4 expected label
    local got
    png1x1 "$lib/$2.png" 0 0 0
    xattr -w theme.source "$3" "$lib/$2.png" 2>/dev/null
    got=$(COLUMNS=120 run_nokitty "$lib" preview "$2" 2>/dev/null | sed -n 's/^ *SOURCE  *//p')
    if [ "$got" = "$4" ]; then pass "$1"; else fail "$1 (got '$got', wanted '$4')"; fi
}
if xattr_meta; then
srccase "the genuine host still labels unsplash"  srchost-good \
    "https://unsplash.com/photos/abc" unsplash
srccase "a subdomain of it still labels unsplash" srchost-sub \
    "https://images.unsplash.com/photo-1" unsplash
srccase "a lookalike PREFIX host is not unsplash" srchost-evil \
    "https://evilunsplash.com/payload" evilunsplash.com
srccase "a SUFFIX-extended host is not unsplash"  srchost-suffix \
    "https://unsplash.com.evil.invalid/x" unsplash.com.evil.invalid
srccase "userinfo cannot fake the host"           srchost-userinfo \
    "https://unsplash.com@evil.invalid/x" evil.invalid
fi

# --- NO command emits terminal control protocol, whatever disk or API says -
sweeplib="$fixture/sweep"; sweepcache="$fixture/sweepcache"
mkdir -p "$sweeplib" "$sweepcache"
oscpay=$(printf '\033]52;c;T1ND\007')
for stem in sweep-safe sweep-rm sweep-mv; do
    png1x1 "$sweeplib/$stem$oscpay.png" 0 0 0
done
oscfile="$sweeplib/sweep-safe$oscpay.png"
printf '%s' "$oscfile" >"$sweepcache/wal"
printf 'include %s/colors-kitty.conf\n' "$sweepcache" >"$sweepcache/current-theme.conf"
printf '#101010\n#202020\n#303030\n' >"$sweepcache/colors"
xattr -w theme.source "https://example.invalid/x" "$oscfile" 2>/dev/null
sweepbin="$fixture/sweepbin"
mkdir -p "$sweepbin"
printf '#!/bin/bash\nexit 1\n' >"$sweepbin/wallpaper"
chmod +x "$sweepbin/wallpaper"
injname=$(printf 'sizeinject\n  pixelWidth: %s' "$oscpay")
printf 'not an image' >"$sweeplib/$injname"

sweep_all="$fixture/sweep.out"; : >"$sweep_all"
sweep_bad=""
sweep() {
    local o
    o=$(THEME_WALLPAPER_DIR="$sweeplib" THEME_CACHE_DIR="$sweepcache" THEME_NO_APPLY=1 \
        CONFIG_DIR="$sweepcache" KITTY_CONFIG_DIRECTORY="$sweepcache" COLUMNS=120 \
        PATH="$sweepbin:$PATH" \
        TMPDIR="$fixture/tmpdir" KITTY_WINDOW_ID='' "$THEME" "$@" 2>&1)
    printf '%s' "$o" | grep -qF -- "$osc_open" && sweep_bad="$sweep_bad [$1]"
    printf '%s\n' "$o" >>"$sweep_all"
    return 0
}
sweep help
sweep status
sweep list
sweep list -v
sweep search sweep-safe
sweep preview sweep-safe
sweep preview sizeinject
sweep set sweep-safe
sweep random
sweep rename sweep-mv renamed-by-sweep
sweep rm sweep-rm
sweep "unknown$oscpay"
sweep "unknown$oscpay" --help
if [ -n "$sweep_bad" ]; then
    fail "OSC reached the terminal from:$sweep_bad"
else pass "no command emits a filename's OSC sequence"; fi
sweep_missing=""
for marker in 'would set the desktop wallpaper to' 'would derive a palette from' \
    'now: ' 'current theme:' 'palette image:' 'COLORSCHEME' 'sizeinject' \
    'successfully deleted' 'successfully renamed' 'unknown command' \
    'wallpapers match'; do
    grep -qF -- "$marker" "$sweep_all" || sweep_missing="$sweep_missing [$marker]"
done
if [ -z "$sweep_missing" ]; then
    pass "the sweep really reached every surface it claims to cover"
else fail "the sweep never printed:$sweep_missing"; fi
if grep -q 'sweep-safe' "$sweep_all"; then
    pass "printable filename text survives every command"
else fail "sanitizing removed the whole name, not just its control bytes"; fi

# --- VALID HELP PATHS print ENVIRONMENT data safely ------------------------
helpcache="$fixture/helpcache"
mkdir -p "$helpcache"
help_bad=""
help_all="$fixture/help.out"; : >"$help_all"
help_run() { # $1 label, $2 TERM_PROGRAM, $3 TERM, $4… theme arguments
    local o
    o=$(THEME_WALLPAPER_DIR="/nonexistent/hlibSAFE$oscpay" THEME_CACHE_DIR="$helpcache" \
        CONFIG_DIR="$helpcache" KITTY_CONFIG_DIRECTORY="$helpcache" COLUMNS=120 \
        THEME_NO_APPLY=1 TMPDIR="$fixture/tmpdir" KITTY_WINDOW_ID='' \
        PATH="$sweepbin:$PATH" TERM_PROGRAM="$2" TERM="$3" \
        "$THEME" "${@:4}" 2>&1)
    printf '%s' "$o" | grep -qF -- "$osc_open" && help_bad="$help_bad [$1]"
    printf '%s\n' "$o" >>"$help_all"
    return 0
}
for c in random set unsplash get list preview status rename rm update help; do
    help_run "$c--help" TermSAFE xtermSAFE "$c" --help
done
help_run help-TERM_PROGRAM "TermSAFE$oscpay" xtermSAFE help
help_run help-TERM '' "xtermSAFE$oscpay" help
if [ -n "$help_bad" ]; then
    fail "OSC reached the terminal from help:$help_bad"
else pass "no help path emits environment data as terminal protocol"; fi
help_missing=""
for marker in 'hlibSAFE' 'TermSAFE' 'xtermSAFE' \
    'Apply Commands' 'theme random' 'theme unsplash' 'theme rm' 'never elevates'; do
    grep -qF -- "$marker" "$help_all" || help_missing="$help_missing [$marker]"
done
if [ -z "$help_missing" ]; then
    pass "printable environment text survives every help path"
else fail "help sanitizing removed more than the control bytes:$help_missing"; fi
kittyout=$(THEME_WALLPAPER_DIR="$sweeplib" THEME_CACHE_DIR="$helpcache" CONFIG_DIR="$helpcache" \
    KITTY_CONFIG_DIRECTORY="$helpcache" THEME_NO_APPLY=1 TMPDIR="$fixture/tmpdir" \
    PATH="$sweepbin:$PATH" KITTY_WINDOW_ID=1 "$THEME" help 2>&1)
if printf '%s' "$kittyout" | grep -qF -- "$(printf '\033]8;;https://sw.kovidgoyal.net/kitty/')"; then
    pass "the intentional kitty hyperlink survives sanitizing"
else fail "sanitizing stripped the kitty hyperlink it was supposed to keep"; fi

# --- the OS row comes from the KERNEL, not from PATH -----------------------
# `uname` ×2 and `sw_vers` were three of the bare screen's four spawns. The
# row is uname(2) and a root-owned system plist now, so a planted pair on
# PATH may not run and may not be printed — and the row must still answer.
osbin="$fixture/osbin"
mkdir -p "$osbin"
for tool in uname sw_vers; do
    cat >"$osbin/$tool" <<EOS
#!/bin/sh
: >"$fixture/os-tool-ran"
printf 'PLANTED\n'
EOS
    chmod +x "$osbin/$tool"
done
rm -f "$fixture/os-tool-ran"
os_out=$(THEME_WALLPAPER_DIR="$sweeplib" THEME_CACHE_DIR="$helpcache" COLUMNS=120 \
    KITTY_WINDOW_ID='' THEME_NO_APPLY=1 TMPDIR="$fixture/tmpdir" \
    PATH="$osbin:$sweepbin:$PATH" "$THEME" help 2>&1)
if [ ! -e "$fixture/os-tool-ran" ] && ! printf '%s' "$os_out" | grep -q PLANTED; then
    pass "the OS row spawns neither uname nor sw_vers, and prints neither"
else fail "the header still reads its OS facts through PATH"; fi
if printf '%s' "$os_out" | grep -qE 'OS {2,}[A-Za-z]'; then
    pass "the OS row still names this system"
else fail "the OS row lost its value"; fi

# --- a CONTRIBUTOR name is remote free text under the same threat model -----
STUB_WHO=$(printf 'Contributor\033]52;c;UkVNT1RF\007')
export STUB_WHO
wholog="$fixture/curl-who.log"; : >"$wholog"
who_out=$(CURL_LOG="$wholog" PATH="$stubdir:$PATH" UNSPLASH_ACCESS_KEY=stub-sentinel-key \
    UNSPLASH_SECRET_KEY='' \
    UNSPLASH_USER_TOKEN='' \
    THEME_WALLPAPER_DIR="$lib" THEME_NO_APPLY=1 THEME_CACHE_DIR="$fixture/cache" \
    TMPDIR="$fixture/tmpdir" "$THEME" unsplash https://unsplash.com/photos/whoslug-abcdef12345 2>&1)
unset STUB_WHO
if printf '%s' "$who_out" | grep -qF -- "$osc_open"; then
    fail "a contributor name emitted OSC through the credit note"
else pass "a contributor name cannot emit OSC"; fi
if printf '%s' "$who_out" | grep -q 'photo by Contributor'; then
    pass "the contributor's printable name still shows"
else fail "the credit note lost the contributor's name entirely"; fi

# --- version: three lines, exact shape; the number itself floats -------------
vout=$(run "$lib" version 2>&1)
if printf '%s\n' "$vout" | sed -n 1p | grep -Eq '^version: v[0-9]+\.[0-9]+\.[0-9]+$'; then
    pass "version line has the v-prefixed semver shape"
else fail "version line malformed: $(printf '%s' "$vout" | sed -n 1p)"; fi
if [ "$(printf '%s\n' "$vout" | sed -n 2p)" = "github: https://github.com/snaraj/theme" ] \
   && [ "$(printf '%s\n' "$vout" | sed -n 3p)" = "maintainer: Samuel Naranjo" ] \
   && [ "$(printf '%s\n' "$vout" | wc -l | tr -d ' ')" = "3" ]; then
    pass "version prints repo and maintainer, three lines exactly"
else fail "version output shape drifted: $vout"; fi
if [ "$(run "$lib" --version 2>&1)" = "$vout" ] && [ "$(run "$lib" -V 2>&1)" = "$vout" ]; then
    pass "--version and -V alias the version command"
else fail "--version/-V do not match the version output"; fi

# --- update: verify-then-rename self-update against a stubbed GitHub --------
# The invariant under test: the running binary is NEVER replaced by
# unverified bytes. A copy of the real binary is the install target; a PATH
# stub plays the whole GitHub flow (API → 302 → asset host), logging every
# URL curl is asked for — so "refused" means curl was never spawned at it.
updd="$fixture/upd"
mkdir -p "$updd/bin" "$updd/stubbin" "$updd/Cellar/theme/0.0.0/bin" "$updd/cargo/bin"
cur_ver=$(printf '%s\n' "$vout" | sed -n 1p | sed 's/^version: v//')
case "$(uname -s)-$(uname -m)" in
Darwin-arm64) triple=aarch64-apple-darwin ;;
Darwin-x86_64) triple=x86_64-apple-darwin ;;
Linux-aarch64) triple=aarch64-unknown-linux-gnu ;;
*) triple=x86_64-unknown-linux-gnu ;;
esac
# The release asset shape: a tar.gz holding the single member `theme`
# (SHA256SUMS digests the TARBALL, exactly as the release workflow builds).
# Local macOS tar would add AppleDouble/pax entries for this machine's
# provenance xattrs — entries the strict in-process walker rightly refuses
# and the real runner-built assets don't have — so members are stripped and
# archived with COPYFILE_DISABLE, reproducing the shipped shape.
# python tarfile in USTAR format: byte-exact plain headers, arcname = the
# path's basename, symlinks preserved as entries. (macOS provenance xattrs
# are SIP-protected — local bsdtar smuggles them in no matter what.)
mktar() { # $1 out.tar.gz  $2 srcdir  $3… member paths relative to srcdir
    python3 - "$@" <<'PY'
import os, sys, tarfile
out, d, *members = sys.argv[1:]
with tarfile.open(out, 'w:gz', format=tarfile.USTAR_FORMAT) as t:
    for m in members:
        t.add(os.path.join(d, m), arcname=os.path.basename(m), recursive=False)
PY
}
inner="$updd/inner"
printf 'NEW-RELEASE-BYTES-%s' "$triple" >"$updd/theme"
cp "$updd/theme" "$inner"
payload="$updd/payload.tar.gz"
mktar "$payload" "$updd" theme
rm -f "$updd/theme"
if command -v sha256sum >/dev/null 2>&1; then
    paysha=$(sha256sum "$payload" | cut -d' ' -f1)
else
    paysha=$(shasum -a 256 "$payload" | cut -d' ' -f1)
fi
# The stub learns its controls from the ctl FILE upd_run writes, never from
# env: the binary env-clears every transport child (round 9), so the
# environment provably cannot reach this script. PATH is pinned explicitly
# because an env-cleared bash otherwise runs on its compiled-in default.
printf '#!/bin/bash\nPATH=/usr/bin:/bin\n. "%s/ctl"\n' "$updd" >"$updd/stubbin/curl"
cat >>"$updd/stubbin/curl" <<'EOS'
out=""; hdr=""; url=""; k=0
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
    case "${args[$i]}" in
    -o) out="${args[$((i + 1))]}" ;;
    -D) hdr="${args[$((i + 1))]}" ;;
    --url) url="${args[$((i + 1))]}" ;;
    -K) k=1 ;;
    esac
done
[ "$k" = 1 ] && cat >/dev/null
printf '%s\n' "$url" >>"$UPD_LOG"
resp() { printf 'HTTP/2 %s\r\n%s\r\n\r\n' "$1" "$2" >"$hdr"; }
release_json() { # $1 tag — STABLE asset names, no version infix (#14 r3)
    local a="https://github.com/snaraj/theme/releases/download/$1"
    printf '{"tag_name":"%s","assets":[{"name":"SHA256SUMS","browser_download_url":"%s/SHA256SUMS"},{"name":"theme-%s.tar.gz","browser_download_url":"%s/theme-%s.tar.gz"}]}' \
        "$1" "$a" "$UPD_TRIPLE" "$a" "$UPD_TRIPLE"
}
case "$url" in
*api.github.com/repos/snaraj/theme/releases/latest*)
    release_json "$UPD_TAG"
    ;;
*api.github.com/repos/snaraj/theme/releases/tags/*)
    want="${url##*/}"
    if [ "$want" = "$UPD_TAG" ]; then release_json "$want"; else exit 22; fi
    ;;
*/releases/download/*/SHA256SUMS*)
    : >"$out"; resp 302 "location: https://release-assets.githubusercontent.com/sums"
    ;;
*release-assets.githubusercontent.com/sums*)
    cp "$UPD_SUMS_FILE" "$out"; resp 200 "content-type: text/plain"
    ;;
*/releases/download/*/theme-*)
    : >"$out"
    if [ "${UPD_EVIL:-}" = 1 ]; then
        resp 302 "location: https://evil.invalid/steal"
    else
        resp 302 "location: https://release-assets.githubusercontent.com/bin"
    fi
    ;;
*release-assets.githubusercontent.com/bin*)
    if [ "${UPD_PARTIAL:-}" = 1 ]; then
        head -c 8 "$UPD_PAYLOAD" >"$out"
        resp 200 "content-type: application/octet-stream"
        exit 23
    fi
    if [ "${UPD_BIG:-}" = 1 ]; then
        dd if=/dev/zero of="$out" bs=1048576 count=101 2>/dev/null
        resp 200 "content-type: application/octet-stream"
        exit 0
    fi
    cp "$UPD_PAYLOAD" "$out"; resp 200 "content-type: application/octet-stream"
    ;;
*evil.invalid*)
    printf 'EVIL' >"$out"; resp 200 ""
    ;;
*) exit 6 ;;
esac
exit 0
EOS
chmod +x "$updd/stubbin/curl"
updlog="$updd/log"
upd_out="$updd/out"
upd_run() { # output lands in $upd_out, exit code in $upd_rc (parent shell —
    # a $() capture would strand both in a subshell); env pairs, then an
    # optional `--` followed by extra update arguments.
    # UPD_* control pairs travel by the ctl FILE the stub sources, NOT env:
    # the binary env-clears every transport child (round 9), so the
    # environment provably cannot reach the stub. (UPD_TAR_MARKER stays in
    # the theme process env on purpose — a hypothetical PATH-tar spawn
    # would inherit it, which is exactly what that pin watches for.)
    # $UPD_BIN (a plain shell variable, not an env pair) moves the install
    # target so the route section can run the same flow from a keg or a
    # cargo bin; unset or empty is the ordinary $updd/bin/theme.
    local envs=() ctl=() kv bin="${UPD_BIN:-$updd/bin/theme}"
    while [ $# -gt 0 ] && [ "$1" != "--" ]; do
        case "$1" in
        UPD_TAR_MARKER=*) envs+=("$1") ;;
        UPD_*) ctl+=("$1") ;;
        *) envs+=("$1") ;;
        esac
        shift
    done
    [ "${1:-}" = "--" ] && shift
    # rm first: macOS caches code-signing state by inode, and cp -f over a
    # previously-executed binary poisons it — the next exec dies SIGKILL.
    # DEBUG build: the trusted-transport lane only reaches the curl stub
    # through the THEME_CURL seam — PATH curl is dead to it (round 8).
    rm -f "$bin"
    cp "$THEME_DBG" "$bin"
    : >"$updlog"
    {
        printf "UPD_LOG='%s'\n" "$updlog"
        printf "UPD_TRIPLE='%s'\n" "$triple"
        printf "UPD_PAYLOAD='%s'\n" "$payload"
        printf "UPD_SUMS_FILE='%s'\n" "$updd/sums"
        for kv in ${ctl[@]+"${ctl[@]}"}; do printf '%s\n' "$kv"; done
    } >"$updd/ctl"
    env UPD_TAR_MARKER="$updd/tar-ran" THEME_CURL="$updd/stubbin/curl" \
        PATH="$updd/stubbin:$PATH" \
        THEME_WALLPAPER_DIR="$lib" THEME_CACHE_DIR="$fixture/cache" \
        THEME_NO_APPLY=1 TMPDIR="$fixture/tmpdir" \
        "${envs[@]}" "$bin" update "$@" >"$upd_out" 2>&1
    upd_rc=$?
}
upd_intact() { # target still byte-identical to the run binary, no temp left
    local bin="${UPD_BIN:-$updd/bin/theme}"
    cmp -s "$THEME_DBG" "$bin" \
        && [ -z "$(find "${bin%/*}" -name '.*update*' 2>/dev/null)" ]
}
printf '%s  theme-%s.tar.gz\n' "$paysha" "$triple" >"$updd/sums"

upd_run UPD_TAG="v$cur_ver"
if [ "$upd_rc" = 0 ] && grep -qF "already up to date (v$cur_ver)" "$upd_out"; then
    pass "an up-to-date binary short-circuits"
else fail "same-version update did not short-circuit: $(cat "$upd_out")"; fi
if [ "$(wc -l <"$updlog" | tr -d ' ')" = 1 ]; then
    pass "the short-circuit downloads no asset"
else fail "same-version update still transferred: $(cat "$updlog")"; fi

upd_run UPD_TAG=v9.9.9
if [ "$upd_rc" = 0 ] && grep -qF "theme v$cur_ver → v9.9.9" "$upd_out"; then
    pass "update prints current → new"
else fail "update success output drifted: $(cat "$upd_out")"; fi
if cmp -s "$inner" "$updd/bin/theme" && [ -x "$updd/bin/theme" ]; then
    pass "the target holds exactly the verified member bytes, executable"
else fail "installed bytes differ from the release payload"; fi
if [ -z "$(find "$updd/bin" -name '.*update*' 2>/dev/null)" ]; then
    pass "no install temp file survives success"
else fail "an install temp file was left beside the binary"; fi

printf '%064d  theme-%s.tar.gz\n' 0 "$triple" >"$updd/sums"
upd_run UPD_TAG=v9.9.9
if [ "$upd_rc" != 0 ] && grep -q 'SHA256 verification FAILED' "$upd_out"; then
    pass "a corrupted hash is refused"
else fail "corrupted hash not refused: $(cat "$upd_out")"; fi
if upd_intact; then
    pass "the corrupted download never touched the binary"
else fail "unverified bytes reached the install target"; fi

printf '%s  theme-%s.tar.gz\n' "$paysha" "$triple" >"$updd/sums"
upd_run UPD_TAG=v9.9.9 UPD_EVIL=1
if [ "$upd_rc" != 0 ] && grep -q 'refusing non-GitHub download host' "$upd_out"; then
    pass "a redirect off GitHub's hosts is refused"
else fail "foreign redirect host not refused: $(cat "$upd_out")"; fi
if ! grep -q 'evil.invalid' "$updlog" && upd_intact; then
    pass "the foreign host was never contacted and the binary is intact"
else fail "curl was spawned at the foreign host or the binary changed"; fi

upd_run UPD_TAG=v9.9.9 UPD_PARTIAL=1
if [ "$upd_rc" != 0 ] && upd_intact; then
    pass "an interrupted download leaves the binary untouched"
else fail "a partial download reached the install path: $(cat "$upd_out")"; fi

# A digest-VALID tarball that holds no `theme` member: verification passes,
# the unpack must still refuse and leave the target alone.
printf 'IMPOSTOR' >"$updd/not-theme"
badtar="$updd/badtar.tar.gz"
mktar "$badtar" "$updd" not-theme
rm -f "$updd/not-theme"
if command -v sha256sum >/dev/null 2>&1; then
    badsha=$(sha256sum "$badtar" | cut -d' ' -f1)
else
    badsha=$(shasum -a 256 "$badtar" | cut -d' ' -f1)
fi
printf '%s  theme-%s.tar.gz\n' "$badsha" "$triple" >"$updd/sums"
upd_run UPD_TAG=v9.9.9 UPD_PAYLOAD="$badtar"
if [ "$upd_rc" != 0 ] && grep -q "not a single 'theme' binary" "$upd_out" && upd_intact; then
    pass "a verified archive without the binary is refused"
else fail "member-less archive not refused: $(cat "$upd_out")"; fi
printf '%s  theme-%s.tar.gz\n' "$paysha" "$triple" >"$updd/sums"

# --- the archive shape is enforced IN-PROCESS (Codex round-2 findings) -----
# A PATH-planted tar must not be able to influence the installed bytes —
# extraction is in-process now, so the stub must NEVER EVEN RUN.
cat >"$updd/stubbin/tar" <<'EOS'
#!/bin/sh
: >"$UPD_TAR_MARKER"
printf 'EVIL-TAR-BYTES'
exit 0
EOS
chmod +x "$updd/stubbin/tar"
rm -f "$updd/tar-ran"
upd_run UPD_TAG=v9.9.9
if [ "$upd_rc" = 0 ] && cmp -s "$inner" "$updd/bin/theme" && [ ! -e "$updd/tar-ran" ]; then
    pass "a PATH-planted tar never executes and cannot touch the install"
else fail "PATH tar influenced the install (marker: $([ -e "$updd/tar-ran" ] && echo ran))"; fi

# Duplicate members concatenated FIRST+SECOND under the old extractor —
# now any second entry refuses.
mkdir -p "$updd/dupa" "$updd/dupb"
printf 'FIRST' >"$updd/dupa/theme"
printf 'SECOND' >"$updd/dupb/theme"
duptar="$updd/dup.tar.gz"
mktar "$duptar" "$updd" dupa/theme dupb/theme
dupsha=$(if command -v sha256sum >/dev/null 2>&1; then sha256sum "$duptar"; else shasum -a 256 "$duptar"; fi | cut -d' ' -f1)
printf '%s  theme-%s.tar.gz\n' "$dupsha" "$triple" >"$updd/sums"
upd_run UPD_TAG=v9.9.9 UPD_PAYLOAD="$duptar"
if [ "$upd_rc" != 0 ] && grep -q "not a single 'theme' binary" "$upd_out" && upd_intact; then
    pass "a duplicate-member archive refuses instead of concatenating"
else fail "dup-member archive not refused: $(cat "$upd_out")"; fi

# A symlink member (digest-valid) must refuse as a non-regular file.
mkdir -p "$updd/lnk"
ln -sf /etc/passwd "$updd/lnk/theme"
lnktar="$updd/lnk.tar.gz"
mktar "$lnktar" "$updd" lnk/theme
lnksha=$(if command -v sha256sum >/dev/null 2>&1; then sha256sum "$lnktar"; else shasum -a 256 "$lnktar"; fi | cut -d' ' -f1)
printf '%s  theme-%s.tar.gz\n' "$lnksha" "$triple" >"$updd/sums"
upd_run UPD_TAG=v9.9.9 UPD_PAYLOAD="$lnktar"
if [ "$upd_rc" != 0 ] && grep -q "not a single 'theme' binary" "$upd_out" && upd_intact; then
    pass "a link member refuses as non-regular"
else fail "symlink member not refused: $(cat "$upd_out")"; fi

# An uncompressed size over the cap (tiny gzip, 101MiB member) refuses at
# the header, before a byte lands anywhere.
mkdir -p "$updd/big"
dd if=/dev/zero of="$updd/big/theme" bs=1048576 count=101 2>/dev/null
bigtar="$updd/big.tar.gz"
mktar "$bigtar" "$updd/big" theme
rm -rf "$updd/big"
bigsha=$(if command -v sha256sum >/dev/null 2>&1; then sha256sum "$bigtar"; else shasum -a 256 "$bigtar"; fi | cut -d' ' -f1)
printf '%s  theme-%s.tar.gz\n' "$bigsha" "$triple" >"$updd/sums"
upd_run UPD_TAG=v9.9.9 UPD_PAYLOAD="$bigtar"
if [ "$upd_rc" != 0 ] && grep -q 'byte cap' "$upd_out" && upd_intact; then
    pass "an over-cap uncompressed member refuses with the target intact"
else fail "decompression bomb not refused: $(cat "$upd_out")"; fi
printf '%s  theme-%s.tar.gz\n' "$paysha" "$triple" >"$updd/sums"

upd_run UPD_TAG=v9.9.9 UPD_BIG=1
if [ "$upd_rc" != 0 ] && grep -q 'byte cap' "$upd_out" && upd_intact; then
    pass "an oversized download is refused before install"
else fail "oversized download not refused: $(cat "$upd_out")"; fi
if [ -z "$(find "$fixture/tmpdir" -type f -name 'theme.*' 2>/dev/null)" ]; then
    pass "update leaves no scratch residue on any path"
else fail "update left scratch files behind"; fi

# --- update and the footer note share ONE cache ----------------------------
rm -f "$fixture/cache/update-check"
upd_run UPD_TAG="v$cur_ver"
if [ "$(cat "$fixture/cache/update-check" 2>/dev/null)" = "v$cur_ver" ]; then
    pass "a latest-mode update stamps the shared check cache"
else fail "theme update did not refresh the update-check cache"; fi

# --- update --version: a specific release through the same pipeline --------
rm -f "$fixture/cache/update-check"
upd_run UPD_TAG=v9.9.9 -- --version 9.9.9
if [ "$upd_rc" = 0 ] && grep -qF "theme v$cur_ver → v9.9.9" "$upd_out" \
   && cmp -s "$inner" "$updd/bin/theme"; then
    pass "--version installs the requested release"
else fail "--version happy path broke: $(cat "$upd_out")"; fi
if grep -q '/releases/tags/v9.9.9' "$updlog"; then
    pass "--version normalizes and uses the by-tag endpoint"
else fail "--version did not hit the by-tag endpoint: $(cat "$updlog")"; fi
if [ ! -e "$fixture/cache/update-check" ]; then
    pass "a --version fetch never stamps the latest-check cache"
else fail "--version poisoned the update-check cache"; fi

upd_run UPD_TAG=v9.9.9 -- --version '../../evil'
if [ "$upd_rc" != 0 ] && grep -q 'takes a release version' "$upd_out" \
   && [ ! -s "$updlog" ]; then
    pass "an injection-shaped --version refuses before any network"
else fail "--version injection reached further than the parser: $(cat "$upd_out")"; fi
upd_run UPD_TAG=v9.9.9 -- --version
if [ "$upd_rc" != 0 ] && [ ! -s "$updlog" ]; then
    pass "a dangling --version refuses before any network"
else fail "dangling --version was not refused: $(cat "$upd_out")"; fi

# --- --version exists for update ONLY; update's grammar is strict ----------
# (Codex round 3: the flag was consumed globally, so `rm victim --version x`
# deleted the victim. Now every other verb refuses it pre-mutation, and any
# residual update token refuses before a single transfer.)
png1x1 "$lib/vic-flag.png" 1 2 3
vic_sum=$(cksum <"$lib/vic-flag.png")
check "rm refuses a foreign --version flag"      1 run "$lib" rm vic-flag --version v9.9.9
if [ -f "$lib/vic-flag.png" ] && [ "$(cksum <"$lib/vic-flag.png")" = "$vic_sum" ]; then
    pass "the rm victim survives byte-identical"
else fail "rm mutated despite the refused flag"; fi
check "rename refuses a foreign --version flag"  1 run "$lib" rename vic-flag renamed-vic --version v9.9.9
if [ -f "$lib/vic-flag.png" ] && [ ! -e "$lib/renamed-vic.png" ] \
   && [ "$(cksum <"$lib/vic-flag.png")" = "$vic_sum" ]; then
    pass "the rename victim is untouched and no destination appeared"
else fail "rename mutated despite the refused flag"; fi
rm -f "$lib/vic-flag.png"
upd_run UPD_TAG=v9.9.9 -- extraneous
if [ "$upd_rc" != 0 ] && [ ! -s "$updlog" ]; then
    pass "a residual update positional refuses with zero transfers"
else fail "update accepted a stray positional: $(cat "$upd_out")"; fi
upd_run UPD_TAG=v9.9.9 -- --rotate left
if [ "$upd_rc" != 0 ] && [ ! -s "$updlog" ]; then
    pass "a global flag is unknown to update and spawns zero transfers"
else fail "update accepted --rotate: $(cat "$upd_out")"; fi
upd_run UPD_TAG=v9.9.9 -- --version=
if [ "$upd_rc" != 0 ] && [ ! -s "$updlog" ]; then
    pass "an empty --version value refuses with zero transfers"
else fail "update accepted an empty --version: $(cat "$upd_out")"; fi
upd_run UPD_TAG=v9.9.9 -- --version 1.0.0 --version 2.0.0
if [ "$upd_rc" != 0 ] && [ ! -s "$updlog" ]; then
    pass "a duplicate --version refuses with zero transfers"
else fail "update accepted duplicate --version: $(cat "$upd_out")"; fi

upd_run UPD_TAG=v9.9.9 -- --version 0.0.0
if grep -q 'warning: older versions may be unsupported or break — proceeding to v0.0.0' "$upd_out"; then
    pass "a downgrade warns at launch and proceeds"
else fail "downgrade warning missing: $(cat "$upd_out")"; fi
if [ "$upd_rc" != 0 ] && grep -q 'no release v0.0.0' "$upd_out" && upd_intact; then
    pass "a missing tag refuses cleanly with the binary intact"
else fail "missing tag not refused cleanly: $(cat "$upd_out")"; fi

# --- round 8: the update transport is not PATH's to give -------------------
# A hostile curl planted FIRST on PATH must never execute in the update
# lane — the lane runs only the trusted transport (the THEME_CURL seam in
# this debug build; fixed root-owned candidates in release) — while a full
# update through the stub still succeeds. Mirrors the tar-stub pin.
plantbin="$updd/plantbin"
mkdir -p "$plantbin"
printf '#!/bin/sh\n: >"%s/hostile-curl-ran"\nprintf EVIL\nexit 0\n' "$updd" >"$plantbin/curl"
chmod +x "$plantbin/curl"
upd_run UPD_TAG=v9.9.9 PATH="$plantbin:$updd/stubbin:$PATH"
if [ "$upd_rc" = 0 ] && grep -qF "theme v$cur_ver → v9.9.9" "$upd_out" \
   && cmp -s "$inner" "$updd/bin/theme" && [ ! -e "$updd/hostile-curl-ran" ]; then
    pass "a curl planted first on PATH never executes in the update lane"
else fail "PATH curl reached the update transport (rc=$upd_rc): $(cat "$upd_out")"; fi

# THEME_CURL= (empty) simulates "no candidate validates": the explicit
# update refuses BEFORE any network with its own message and zero
# transfers; the running binary stays byte-identical.
upd_run THEME_CURL= UPD_TAG=v9.9.9
if [ "$upd_rc" != 0 ] && grep -q 'no trusted system curl' "$upd_out" \
   && [ ! -s "$updlog" ] && upd_intact; then
    pass "a missing trusted transport is its own pre-network refusal"
else fail "missing-transport refusal broke (rc=$upd_rc): $(cat "$upd_out")"; fi

# The RELEASE binary carries no seam at all: with a hostile curl first on
# PATH and THEME_CURL aimed at a second marker stub, the only transport it
# may use is the validated system curl — the request 404s (online) or
# fails to resolve (offline), both the same clean refusal; the markers
# stay absent and the target byte-identical either way. (This one pin may
# send a single credential-free GET to the real API when online — it is
# the release transport itself under test, and nothing else can prove the
# seam compiled out.)
printf '#!/bin/sh\n: >"%s/seam-curl-ran"\nexit 0\n' "$updd" >"$plantbin/seamcurl"
chmod +x "$plantbin/seamcurl"
rm -f "$updd/bin/theme"
cp "$THEME" "$updd/bin/theme"
env PATH="$plantbin:$PATH" THEME_CURL="$plantbin/seamcurl" \
    THEME_WALLPAPER_DIR="$lib" THEME_CACHE_DIR="$fixture/cache" \
    THEME_NO_APPLY=1 TMPDIR="$fixture/tmpdir" \
    "$updd/bin/theme" update --version v99.99.99 >"$upd_out" 2>&1
upd_rc=$?
if [ "$upd_rc" != 0 ] && cmp -s "$THEME" "$updd/bin/theme" \
   && [ ! -e "$updd/hostile-curl-ran" ] && [ ! -e "$updd/seam-curl-ran" ]; then
    pass "the release binary ignores PATH and the seam alike"
else fail "release transport was steerable (rc=$upd_rc): $(cat "$upd_out")"; fi

# --- round 9: the environment channel is dead to the boundary ---------------
# The parent exports the full hostile set — TLS-trust substitution
# (CURL_CA_BUNDLE/SSL_CERT_*/CURL_SSL_BACKEND), loader injection (both the
# LD_ and DYLD_ families), a proxy trio, and BASH_ENV, which WOULD execute
# a marker script inside any bash child that inherited env — and a full
# update through BOTH command shapes (metadata + two asset hops) still
# succeeds with no marker: every transport child starts from an empty
# environment, and the stub itself learns its controls from the ctl file
# because env provably cannot reach it.
# (DYLD_INSERT_LIBRARIES carries a REAL, inert system dylib: dyld
# hard-kills any process it cannot load the insert into, and the theme
# process's own launch env is the caller's domain, not this boundary's —
# what is under test is that the variable never reaches a CHILD.)
printf ': >"%s/envchan-ran"\n' "$updd" >"$updd/bashenv.sh"
upd_run UPD_TAG=v9.9.9 \
    BASH_ENV="$updd/bashenv.sh" \
    CURL_CA_BUNDLE=/dev/null SSL_CERT_FILE=/dev/null SSL_CERT_DIR=/dev/null \
    CURL_SSL_BACKEND=hostile LD_PRELOAD=/dev/null LD_AUDIT=/dev/null \
    LD_LIBRARY_PATH=/dev/null DYLD_INSERT_LIBRARIES=/usr/lib/libz.1.dylib \
    DYLD_LIBRARY_PATH=/dev/null https_proxy=http://127.0.0.1:9 \
    HTTPS_PROXY=http://127.0.0.1:9 all_proxy=http://127.0.0.1:9
if [ "$upd_rc" = 0 ] && grep -qF "theme v$cur_ver → v9.9.9" "$upd_out" \
   && cmp -s "$inner" "$updd/bin/theme" && [ ! -e "$updd/envchan-ran" ]; then
    pass "a fully hostile environment never reaches a boundary child"
else fail "the environment channel leaked (rc=$upd_rc): $(cat "$upd_out")"; fi

# --- issue #33: a managed install is routed, never replaced ----------------
# A keg or a cargo build belongs to whoever installed it, so `theme update`
# prints that route and stops BEFORE the transport check — the stub log
# stays empty and the binary byte-identical. (Copies of the debug build
# stand in for the real installs; only the path decides.)
UPD_BIN="$updd/Cellar/theme/0.0.0/bin/theme"
upd_run UPD_TAG=v9.9.9
if [ "$upd_rc" = 0 ] && grep -qF 'brew upgrade snaraj/theme/theme' "$upd_out"; then
    pass "a Homebrew keg is routed to brew"
else fail "keg route missing (rc=$upd_rc): $(cat "$upd_out")"; fi
if [ ! -s "$updlog" ] && upd_intact; then
    pass "the keg route transfers nothing and replaces nothing"
else fail "the keg route fetched or installed: $(cat "$updlog")"; fi
upd_run UPD_TAG=v9.9.9 -- --version v9.9.9
if [ "$upd_rc" = 0 ] && grep -qF 'brew upgrade snaraj/theme/theme' "$upd_out" \
   && [ ! -s "$updlog" ] && upd_intact; then
    pass "--version cannot route around a keg"
else fail "--version bypassed the keg route (rc=$upd_rc): $(cat "$upd_out")"; fi
UPD_BIN="$updd/cargo/bin/theme"
upd_run UPD_TAG=v9.9.9 CARGO_HOME="$updd/cargo"
if [ "$upd_rc" = 0 ] \
   && grep -qF 'cargo install --git https://github.com/snaraj/theme --locked' "$upd_out" \
   && [ ! -s "$updlog" ] && upd_intact; then
    pass "a cargo install is routed back to cargo, transferring nothing"
else fail "cargo route missing (rc=$upd_rc): $(cat "$upd_out")"; fi
UPD_BIN=

# --- the update-available footer on the bare `theme` screen ----------------
notecache="$fixture/notecache"
mkdir -p "$notecache"
failbin="$fixture/failbin"
mkdir -p "$failbin"
notefaillog="$fixture/notefail.log"
: >"$notefaillog"
# The attempt log path is BAKED at generation — the child is env-cleared
# (round 9), so a $NOTE_FAIL_LOG reference would see nothing.
cat >"$failbin/curl" <<EOS
#!/bin/sh
printf 'x\n' >>"$notefaillog"
exit 6
EOS
chmod +x "$failbin/curl"
note_out="$updd/note.out"
note_run() { # bare `theme` with the check ENABLED; env-pair overrides last.
    # DEBUG build + THEME_CURL seam: the footer's trusted-transport lane
    # reaches the failing stub by absolute path, never via PATH (round 8).
    env THEME_NO_UPDATE_CHECK= \
        THEME_WALLPAPER_DIR="$lib" THEME_CACHE_DIR="$notecache" \
        THEME_NO_APPLY=1 TMPDIR="$fixture/tmpdir" KITTY_WINDOW_ID='' \
        THEME_CURL="$failbin/curl" \
        PATH="$failbin:$sweepbin:$PATH" "$@" "$THEME_DBG" >"$note_out" 2>&1
    note_rc=$?
}
expect1="update to the latest theme version: v9.9.9 -> https://github.com/snaraj/theme/releases/tag/v9.9.9"
expect2="to update run: theme update"

printf 'v9.9.9' >"$notecache/update-check"
note_run
if [ "$(tail -2 "$note_out" | sed -n 1p)" = "$expect1" ] \
   && [ "$(tail -2 "$note_out" | sed -n 2p)" = "$expect2" ]; then
    pass "a newer cached release ends the screen with the two-line footer"
else fail "footer shape drifted: $(tail -3 "$note_out")"; fi
if [ ! -s "$notefaillog" ]; then
    pass "a fresh cache spawns no network attempt"
else fail "the note refreshed despite a fresh cache"; fi

printf 'v%s' "$cur_ver" >"$notecache/update-check"
note_run
if ! grep -qF 'update to the latest' "$note_out"; then
    pass "an up-to-date cache renders no footer"
else fail "the footer rendered for the running version"; fi
printf 'v0.0.0' >"$notecache/update-check"
note_run
if ! grep -qF 'update to the latest' "$note_out"; then
    pass "a dev build newer than the latest renders no footer"
else fail "the footer rendered for an older latest"; fi
printf 'v9.9.9junk' >"$notecache/update-check"
note_run
if ! grep -qF 'update to the latest' "$note_out"; then
    pass "a malformed cache is silently ignored"
else fail "a malformed cache still rendered"; fi
printf 'v9.9.9\033]52;c;steal\007' >"$notecache/update-check"
note_run
if ! grep -qF 'update to the latest' "$note_out" \
   && ! grep -qF "$(printf '\033]')" "$note_out"; then
    pass "a hostile cache renders neither a footer nor terminal protocol"
else fail "hostile cache content reached the terminal"; fi

rm -f "$notecache/update-check"
note_run
if [ "$note_rc" = 0 ] && ! grep -qiE 'update to the latest|error|curl' "$note_out"; then
    pass "no cache + no network is silent and exits clean"
else fail "the offline check leaked noise (rc=$note_rc): $(tail -3 "$note_out")"; fi
if [ "$(wc -l <"$notefaillog" | tr -d ' ')" = 1 ] && [ -e "$notecache/update-check" ]; then
    pass "the failed attempt was stamped into the cache"
else fail "offline attempt accounting is wrong: $(wc -l <"$notefaillog") attempts"; fi
note_run
if [ "$(wc -l <"$notefaillog" | tr -d ' ')" = 1 ]; then
    pass "one bounded attempt per TTL window, not per run"
else fail "the note retried inside the TTL window"; fi

rm -f "$notecache/update-check"
note_run THEME_NO_UPDATE_CHECK=1
if ! grep -qF 'update to the latest' "$note_out" \
   && [ "$(wc -l <"$notefaillog" | tr -d ' ')" = 1 ]; then
    pass "the kill-switch spawns nothing and renders nothing"
else fail "THEME_NO_UPDATE_CHECK did not disable the check"; fi

# Round 8, decided-and-stated: NO trusted transport ⇒ no network AND no
# stamp — the TTL stamp rate-limits network ATTEMPTS, and none happened,
# so a recovered transport a minute later must not find itself masked.
rm -f "$notecache/update-check"
note_run THEME_CURL=
if [ "$note_rc" = 0 ] && [ ! -e "$notecache/update-check" ] \
   && ! grep -qF 'update to the latest' "$note_out" \
   && [ "$(wc -l <"$notefaillog" | tr -d ' ')" = 1 ]; then
    pass "a missing transport neither stamps nor fetches nor renders"
else fail "missing transport misbehaved (rc=$note_rc): $(ls -l "$notecache" 2>/dev/null)"; fi
# ...but a still-fresh cache renders with no transport at all — displaying
# already-earned data needs no network.
printf 'v9.9.9' >"$notecache/update-check"
note_run THEME_CURL=
if [ "$note_rc" = 0 ] && grep -qF "$expect1" "$note_out"; then
    pass "a fresh cache renders without any transport"
else fail "the fresh-cache render needed a transport: $(tail -3 "$note_out")"; fi

# --- version asks LIVE, every call (issues #25, #42) -----------------------
# The footer's custody and transport, asked as a QUESTION — so the answer
# may never outrun the evidence, and (#42) may never come from a day-old
# stamp: the owner's v0.3.0 binary said "you're on the latest" one minute
# after v0.3.1 published, because `theme update` had stamped the shared
# cache an hour earlier. Every call of the WORD asks now, and the cache is
# only WRITTEN here; the three plain lines are written BEFORE the request,
# so the facts never wait on GitHub, and one closing line follows. The
# FLAG forms (-V, --version) are the banner scripts call and ask nothing at
# all. Its own cache dir, stub, request log and ordering witness leave the
# footer's accounting above exactly as pinned.
vercache="$fixture/vercache"
verbin="$fixture/verbin"
verctl="$fixture/verctl"
verlog="$fixture/ver.log"
verwit="$fixture/ver.witness"
ver_out="$updd/ver.out"
mkdir -p "$vercache" "$verbin"
: >"$verlog"
: >"$verwit"
# The stub learns its tag from the ctl FILE, never from env — the transport
# child is env-cleared (round 9) — and logs every URL it is asked for, so
# "no request" means curl was never spawned at one. It also copies stdout
# AS IT STANDS at request time into the witness, which is how the streaming
# order is proved. An empty tag plays an unreachable API (exit 6, the
# failing-transport pattern the footer uses).
cat >"$verbin/curl" <<EOS
#!/bin/sh
PATH=/usr/bin:/bin
. "$verctl"
url=""
while [ \$# -gt 0 ]; do
    case "\$1" in
    --url) url="\$2" ;;
    -K) cat >/dev/null ;;
    esac
    shift
done
printf '%s\n' "\$url" >>"$verlog"
cat "$ver_out" >"$verwit"
[ -n "\$VER_TAG" ] || exit 6
printf '{"tag_name":"%s"}' "\$VER_TAG"
EOS
chmod +x "$verbin/curl"
ver_tag=""
ver_run() { # $1 the verb (version|--version|-V); env-pair overrides follow.
    local verb="$1"
    shift
    printf "VER_TAG='%s'\n" "$ver_tag" >"$verctl"
    : >"$verlog"
    : >"$verwit"
    env THEME_NO_UPDATE_CHECK= \
        THEME_WALLPAPER_DIR="$lib" THEME_CACHE_DIR="$vercache" \
        THEME_NO_APPLY=1 TMPDIR="$fixture/tmpdir" KITTY_WINDOW_ID='' \
        THEME_CURL="$verbin/curl" \
        PATH="$verbin:$sweepbin:$PATH" "$@" "$THEME_DBG" "$verb" >"$ver_out" 2>&1
    ver_rc=$?
}
verline() { sed -n "$1p" "$ver_out"; }
vercount() { wc -l <"$ver_out" | tr -d ' '; }
verreqs() { wc -l <"$verlog" | tr -d ' '; }
vercached() { cat "$vercache/update-check" 2>/dev/null; }
plain2="github: https://github.com/snaraj/theme"
plain3="maintainer: Samuel Naranjo"
three_plain() { # today's three lines, and nothing else
    [ "$(verline 1)" = "version: v$cur_ver" ] && [ "$(verline 2)" = "$plain2" ] \
        && [ "$(verline 3)" = "$plain3" ] && [ "$(vercount)" = 3 ]
}
closes() { # the same three lines, then $1 as the fourth, and nothing after
    [ "$(verline 1)" = "version: v$cur_ver" ] && [ "$(verline 2)" = "$plain2" ] \
        && [ "$(verline 3)" = "$plain3" ] && [ "$(verline 4)" = "$1" ] \
        && [ "$(vercount)" = 4 ]
}
newer="latest release: v9.9.9 — update with 'theme update'"
equal="you're on the latest release."
ahead="latest release: v0.0.0"
unknown="latest release: unknown (could not check)"

# THE SCREENSHOT (#42): a stamp fresh inside the TTL and naming this very
# build, against a release published since. The cached answer is "latest";
# the true one is not, and the true one is what was asked for.
printf 'v%s' "$cur_ver" >"$vercache/update-check"
ver_tag=v9.9.9
ver_run version
if closes "$newer"; then
    pass "a fresh stamp cannot stop version seeing a newer release"
else fail "version answered from the day-old cache (#42): $(cat "$ver_out")"; fi
if [ "$(verreqs)" = 1 ] \
   && grep -q 'api\.github\.com/repos/snaraj/theme/releases/latest' "$verlog"; then
    pass "version asks the releases API exactly once"
else fail "version's request accounting is wrong: $(cat "$verlog")"; fi
if [ "$(vercached)" = v9.9.9 ]; then
    pass "the live answer re-stamps the cache the footer shares"
else fail "version did not refresh the shared stamp: $(vercached)"; fi
# ORDER: the stub copied stdout as it stood when the request reached it.
# The three lines must ALREADY be there — the facts never wait on GitHub.
if [ "$(wc -l <"$verwit" | tr -d ' ')" = 3 ] \
   && [ "$(sed -n 1p "$verwit")" = "version: v$cur_ver" ]; then
    pass "the three plain lines are written BEFORE the request is made"
else fail "version withheld its output until the answer: $(cat "$verwit")"; fi
# The FLAG forms are what scripts and other tools call: they print the
# build and stop — no request, no witness, no stamp — and byte-for-byte
# what the question opens with, so the two forms agree on what they share.
ver_three=$(sed -n 1,3p "$ver_out")
printf 'v0.0.0' >"$vercache/update-check" # a STALE value they must not touch
verstamp=$(cksum <"$vercache/update-check")
for f in --version -V; do
    ver_run "$f"
    if three_plain && [ "$(cat "$ver_out")" = "$ver_three" ] \
       && [ "$(verreqs)" = 0 ] && [ ! -s "$verwit" ] \
       && [ "$(cksum <"$vercache/update-check")" = "$verstamp" ]; then
        pass "theme $f prints the build alone: no request, no stamp"
    else fail "theme $f did more than print the build: $(cat "$ver_out")"; fi
done
# The other direction: a stale stamp claiming a release that is not there.
printf 'v9.9.9' >"$vercache/update-check"
ver_tag="v$cur_ver"
ver_run version
if closes "$equal" && [ "$(vercached)" = "v$cur_ver" ]; then
    pass "a live same-version answer says so and corrects the stamp"
else fail "the up-to-date answer drifted: $(cat "$ver_out")"; fi
ver_tag=v0.0.0
ver_run version
if closes "$ahead"; then
    pass "a build ahead of the latest states it and offers no update"
else fail "a dev build made the wrong claim: $(cat "$ver_out")"; fi
# The tag is REMOTE data now, on every call: OSC-52 smuggled in as valid
# JSON escapes (so the parser hands the real ESC/BEL to the shape check)
# must reach neither the answer, nor the terminal, nor the stamp the footer
# will read next.
printf 'v%s' "$cur_ver" >"$vercache/update-check"
ver_tag='v9.9.9\u001b]52;c;steal\u0007'
ver_run version
if closes "$unknown" && ! grep -qF "$(printf '\033]')" "$ver_out" \
   && [ "$(vercached)" = "v$cur_ver" ]; then
    pass "a hostile tag reaches neither the answer, the terminal, nor the stamp"
else fail "hostile API content reached version: $(cat "$ver_out")"; fi
# #42's other half: a failed ask SAYS it failed rather than guessing, and
# must never overwrite a good stamp — that silences the footer for a day.
printf 'v9.9.9' >"$vercache/update-check"
verstamp=$(cksum <"$vercache/update-check")
ver_tag=
ver_run version
if [ "$ver_rc" = 0 ] && closes "$unknown" && ! grep -qiE 'error|curl' "$ver_out" \
   && [ "$(verreqs)" = 1 ]; then
    pass "an unreachable API says unknown instead of guessing"
else fail "the offline answer misbehaved (rc=$ver_rc): $(cat "$ver_out")"; fi
if [ "$(cksum <"$vercache/update-check")" = "$verstamp" ]; then
    pass "a failed live ask leaves the footer's good stamp byte-identical"
else fail "a failed ask overwrote the shared stamp: $(vercached)"; fi
rm -f "$vercache/update-check"
ver_run version
if closes "$unknown" && [ ! -e "$vercache/update-check" ]; then
    pass "a failed live ask stamps nothing at all"
else fail "a failed ask stamped the cache: $(vercached)"; fi
ver_tag=v9.9.9
ver_run version THEME_CURL=
if [ "$ver_rc" = 0 ] && closes "$unknown" && [ "$(verreqs)" = 0 ] \
   && [ ! -e "$vercache/update-check" ]; then
    pass "no trusted transport neither fetches nor stamps, and says unknown"
else fail "missing transport misbehaved (rc=$ver_rc): $(cat "$ver_out")"; fi
ver_run version THEME_NO_UPDATE_CHECK=1
if three_plain && [ "$(verreqs)" = 0 ] && [ ! -e "$vercache/update-check" ]; then
    pass "the kill-switch leaves version at three lines and spawns nothing"
else fail "THEME_NO_UPDATE_CHECK did not disable version's check: $(cat "$ver_out")"; fi
# The FOOTER is deliberately not live and is not re-pinned here: the
# section above already proves a fresh cache spawns no network attempt on
# the bare screen ("a fresh cache spawns no network attempt").

# --- the cache stamp is fail-closed (Codex round 4) ------------------------
# An attacker-writable cache dir with a planted symlink: the old stamp
# followed it and truncated the victim on a bare help. Now custody refuses
# the dir outright — no write, no read, no note, no network attempt.
hostile="$fixture/hostile-cache"
mkdir -p "$hostile"
printf 'twenty-four byte victim!' >"$fixture/stamp-victim"
vic2_sum=$(cksum <"$fixture/stamp-victim")
ln -s "$fixture/stamp-victim" "$hostile/update-check"
chmod 777 "$hostile"
before_ls=$(ls -a "$hostile")
note_run THEME_CACHE_DIR="$hostile"
after_ls=$(ls -a "$hostile")
if [ "$note_rc" = 0 ] && ! grep -qF 'update to the latest' "$note_out" \
   && [ "$(cksum <"$fixture/stamp-victim")" = "$vic2_sum" ] \
   && [ "$before_ls" = "$after_ls" ] \
   && [ "$(wc -l <"$notefaillog" | tr -d ' ')" = 1 ]; then
    pass "a hostile cache dir is refused: victim intact, dir untouched, zero transfers"
else fail "hostile cache dir was touched (rc=$note_rc): $(ls -al "$hostile")"; fi
chmod 700 "$hostile"

# A FIFO planted at the cache name in an otherwise-valid dir must neither
# hang the bare command nor render — and the next stamp heals it into a
# regular file by renameat-replacing the entry.
rm -f "$notecache/update-check"
mkfifo "$notecache/update-check"
note_run
if [ "$note_rc" = 0 ] && ! grep -qF 'update to the latest' "$note_out" \
   && [ -f "$notecache/update-check" ] && [ ! -p "$notecache/update-check" ]; then
    pass "a planted FIFO neither hangs nor renders, and the stamp heals it"
else fail "FIFO at the cache name broke (rc=$note_rc): $(ls -l "$notecache" 2>/dev/null)"; fi

# --- custody depth: steering refused, benign symlinks legal (round 5) ------
# A symlink held in a world-writable dir steers the endpoint wherever the
# attacker likes — even at a perfectly clean 0700 target it must refuse
# (the SPELLED chain is audited, not just the resolved one)…
mkdir -p "$fixture/steer-target"
chmod 700 "$fixture/steer-target"
ln -s "$fixture/steer-target" "$fixture/hostile-cache/steer"
chmod 777 "$fixture/hostile-cache"
note_run THEME_CACHE_DIR="$fixture/hostile-cache/steer"
if [ "$note_rc" = 0 ] && ! grep -qF 'update to the latest' "$note_out" \
   && [ -z "$(ls -A "$fixture/steer-target")" ]; then
    pass "a steered cache path refuses: clean target, but hostile spelled chain"
else fail "endpoint steering accepted (rc=$note_rc): $(ls -A "$fixture/steer-target")"; fi
chmod 700 "$fixture/hostile-cache"
# …a symlink AT the cache-dir name refuses outright (the leaf opens
# O_NOFOLLOW — round-6 killer d), even into clean territory…
ln -s "$fixture/steer-target" "$fixture/goodlink-cache"
printf 'v9.9.9' >"$fixture/steer-target/update-check"
note_run THEME_CACHE_DIR="$fixture/goodlink-cache"
if [ "$note_rc" = 0 ] && ! grep -qF 'update to the latest' "$note_out"; then
    pass "a symlink at the cache-dir name refuses silently"
else fail "leaf symlink accepted (rc=$note_rc): $(tail -3 "$note_out")"; fi
# …while an INTERMEDIATE benign symlink keeps working — every macOS path
# crosses /var -> /private/var, so links inside audited territory must not
# break custody.
mkdir -p "$fixture/steer-target/sub"
printf 'v9.9.9' >"$fixture/steer-target/sub/update-check"
note_run THEME_CACHE_DIR="$fixture/goodlink-cache/sub"
if grep -qF 'update to the latest theme version: v9.9.9' "$note_out"; then
    pass "an intermediate benign symlink still carries custody"
else fail "benign intermediate symlink broke custody: $(tail -3 "$note_out")"; fi

# --- the audit itself shells NOTHING plantable (Codex round 7) -------------
# id and getfacl are gone from the audit; ls is reached only at /bin/ls.
# Planted lookalikes carrying markers prove none of them ever executes,
# while the custody outcomes above stayed exactly as pinned.
auditbin="$fixture/auditbin"
mkdir -p "$auditbin"
for tool in id getfacl ls; do
    printf '#!/bin/sh\n: >"%s/audit-stub-ran-%s"\nexit 0\n' "$fixture" "$tool" >"$auditbin/$tool"
    chmod +x "$auditbin/$tool"
done
# The FIFO section above leaves a regular file behind ONLY if the heal it
# pins actually happened; a stale FIFO here would block this write forever
# and hang CI instead of failing it, so the entry goes before it is written.
rm -f "$notecache/update-check"
printf 'v9.9.9' >"$notecache/update-check"
note_run PATH="$auditbin:$failbin:$sweepbin:$PATH"
if grep -qF 'update to the latest theme version: v9.9.9' "$note_out" \
   && [ -z "$(ls "$fixture" | grep audit-stub-ran)" ]; then
    pass "planted id/getfacl/ls never execute and custody is unchanged"
else fail "an audit helper was PATH-resolved: $(ls "$fixture" | grep audit-stub-ran)"; fi

# Refusal must create NOTHING (round-7 LOW): a hostile ancestor with an
# ABSENT final component ends with no directory conjured at the target.
chmod 777 "$fixture/hostile-cache"
note_run THEME_CACHE_DIR="$fixture/hostile-cache/newcache"
if [ "$note_rc" = 0 ] && [ ! -e "$fixture/hostile-cache/newcache" ]; then
    pass "a refused chain conjures no directory at the steered target"
else fail "refusal still created something: $(ls -A "$fixture/hostile-cache")"; fi
chmod 700 "$fixture/hostile-cache"

# --- narrow terminals: the image is never torn, text never at column 0 -----
# (issue #19) A stub kitten emits a deterministic APC + rows of '#' cells and
# a stub wallpaper answers with a long-titled library file, so the checker
# can SEE the image/text ordering. narrowck.py strips APC/OSC/CSI escapes
# and asserts: no visible line wider than COLUMNS (0 skips the width check —
# below prefix+12 the wrap floors rather than shredding), the '#' image rows
# contiguous with the expected count and width (0 = image must be absent),
# and no line at column 0 outside the known section starters.
narrowbin="$fixture/narrowbin"
mkdir -p "$narrowbin"
narrowimg="$lib/narrow-check-quite-long-wallpaper-title.png"
png1x1 "$narrowimg" 3 7 11
cat >"$narrowbin/kitten" <<'EOS'
#!/bin/bash
place=""
for a in "$@"; do case "$a" in --place=*) place="${a#--place=}" ;; esac; done
w="${place%%x*}"
rest="${place#*x}"
h="${rest%%@*}"
printf '\033_Ga=T,f=100,c=%s,r=%s\033\\' "$w" "$h"
for ((i = 0; i < h; i++)); do
    printf '\033[38;2;7;7;7m'
    for ((j = 0; j < w; j++)); do printf '#'; done
    printf '\n'
done
EOS
chmod +x "$narrowbin/kitten"
cat >"$narrowbin/wallpaper" <<EOS
#!/bin/sh
printf '%s\n' "$narrowimg"
EOS
chmod +x "$narrowbin/wallpaper"
cat >"$fixture/narrowck.py" <<'EOS'
import re, sys
path, cols, imgrows, imgw = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4])
raw = open(path, encoding='utf-8', errors='replace').read()
raw = raw.replace('\r', '')
raw = re.sub(r'\x1b_G.*?\x1b\\', '', raw, flags=re.S)
raw = re.sub(r'\x1b\].*?(\x07|\x1b\\)', '', raw)
raw = re.sub(r'\x1b\[[0-9;:]*[A-Za-z]', '', raw)
lines = raw.split('\n')
if cols:
    bad = [l for l in lines if len(l) > cols]
    if bad:
        sys.exit('WIDER than %d: %r' % (cols, bad[:3]))
rows = [i for i, l in enumerate(lines) if re.search('#{%d}' % max(imgw, 1), l)]
if imgrows == 0:
    if any('#' in l for l in lines):
        sys.exit('image present where it must be absent')
else:
    if len(rows) != imgrows:
        sys.exit('expected %d image rows, saw %d' % (imgrows, len(rows)))
    if rows != list(range(rows[0], rows[0] + imgrows)):
        sys.exit('image rows are NOT contiguous: %r' % rows)
starters = ('Apply Commands:', 'Library Commands:', 'Info Commands:', 'Usage:',
            'Global Flags', 'Use "', 'current theme:', 'mode:', 'color scheme:',
            'palette source:', 'palette image:', 'wallpaper dir:', 'variables:',
            'update to the latest', 'to update run:', 'wallpapers', 'search:')
for l in lines:
    if l and not l.startswith(' ') and not l.startswith(starters):
        sys.exit('column-0 line outside the known starters: %r' % l)
EOS
narrow_out="$fixture/narrow.out"
narrow_run() { # $1 COLUMNS, rest: theme args
    local c="$1"
    shift
    # These cases ARE their own desktop world: the stub below is the helper
    # whose answer they check, so no earlier case's recorded answer may
    # stand in for it.
    rm -f "$fixture/cache/desktop"
    env COLUMNS="$c" KITTY_WINDOW_ID=1 PATH="$narrowbin:$sweepbin:$PATH" \
        THEME_WALLPAPER_DIR="$lib" THEME_CACHE_DIR="$fixture/cache" \
        THEME_NO_APPLY=1 TMPDIR="$fixture/tmpdir" \
        "$THEME" "$@" >"$narrow_out" 2>&1
    narrow_rc=$?
}
narrowck() { python3 "$fixture/narrowck.py" "$narrow_out" "$@" 2>&1; }

narrow_run 100
if [ "$narrow_rc" = 0 ] && why=$(narrowck 100 6 14) && grep -q '#.*COLORSCHEME' "$narrow_out"; then
    pass "wide bare screen keeps the side-by-side header"
else fail "wide bare screen regressed (rc=$narrow_rc): ${why:-$(tail -3 "$narrow_out")}"; fi
narrow_run 40
if [ "$narrow_rc" = 0 ] && why=$(narrowck 40 6 14) && ! grep -q '#.*THEME' "$narrow_out" \
   && grep -q '^  THEME CLI$' "$narrow_out"; then
    pass "40 columns stacks: image whole above, fields below"
else fail "40-column bare screen broke (rc=$narrow_rc): ${why:-$(tail -3 "$narrow_out")}"; fi
narrow_run 25
if [ "$narrow_rc" = 0 ] && why=$(narrowck 25 6 14) && ! grep -q '#.*THEME' "$narrow_out"; then
    pass "25 columns stacks with the image whole"
else fail "25-column bare screen broke (rc=$narrow_rc): ${why:-$(tail -3 "$narrow_out")}"; fi
narrow_run 9
if [ "$narrow_rc" = 0 ] && why=$(narrowck 0 0 0); then
    pass "below the floor the image is absent with dignity, never torn"
else fail "9-column bare screen broke (rc=$narrow_rc): ${why:-$(tail -3 "$narrow_out")}"; fi
narrow_run 25 preview narrow-check-quite-long-wallpaper-title
if [ "$narrow_rc" = 0 ] && why=$(narrowck 25 10 23); then
    pass "a 25-column preview clamps its thumbnail and wraps its fields"
else fail "25-column preview broke (rc=$narrow_rc): ${why:-$(tail -3 "$narrow_out")}"; fi
narrow_run 40 status
if [ "$narrow_rc" = 0 ] && why=$(narrowck 40 0 0); then
    pass "a 40-column status wraps every value with a hanging indent"
else fail "40-column status broke (rc=$narrow_rc): ${why:-$(tail -3 "$narrow_out")}"; fi
narrow_run 40 search narrow-check
if [ "$narrow_rc" = 0 ] && why=$(narrowck 40 0 0); then
    pass "a 40-column search stacks its three columns, none at column 0"
else fail "40-column search broke (rc=$narrow_rc): ${why:-$(tail -3 "$narrow_out")}"; fi

# --- the REAL terminal width, from the tty itself (issue #21) ---------------
# v0.2.1 read only COLUMNS, which zsh does not export — every real terminal
# fell to the wide default and the owner's 42-column kitty still tore. This
# pin is the one that would have caught it: a genuine 42-column pty with
# COLUMNS UNSET must stack. (The env-forced pins above stay: they pin the
# pipe/test class, where COLUMNS is the explicit override.)
cat >"$fixture/ptyrun.py" <<'EOS'
import fcntl, os, pty, struct, subprocess, sys, termios
cols, out, cmd = int(sys.argv[1]), sys.argv[2], sys.argv[3:]
m, s = pty.openpty()
fcntl.ioctl(s, termios.TIOCSWINSZ, struct.pack('HHHH', 24, cols, 0, 0))
env = dict(os.environ)
env.pop('COLUMNS', None)
p = subprocess.Popen(cmd, stdout=s, stderr=s, env=env)
os.close(s)
buf = b''
while True:
    try:
        d = os.read(m, 65536)
    except OSError:
        break
    if not d:
        break
    buf += d
p.wait()
os.close(m)
open(out, 'wb').write(buf.replace(b'\r\n', b'\n'))
sys.exit(p.returncode)
EOS
narrow_pty() { # $1 cols, rest: theme args — a real tty answer, no COLUMNS
    local c="$1"
    shift
    env KITTY_WINDOW_ID=1 PATH="$narrowbin:$sweepbin:$PATH" \
        THEME_WALLPAPER_DIR="$lib" THEME_CACHE_DIR="$fixture/cache" \
        THEME_NO_APPLY=1 TMPDIR="$fixture/tmpdir" \
        python3 "$fixture/ptyrun.py" "$c" "$narrow_out" "$THEME" "$@"
    narrow_rc=$?
}
narrow_pty 42
if [ "$narrow_rc" = 0 ] && why=$(narrowck 42 6 14) && ! grep -q '#.*THEME' "$narrow_out" \
   && grep -q '^  THEME CLI$' "$narrow_out"; then
    pass "a real 42-column tty stacks with COLUMNS unset"
else fail "the tty's own width was ignored (rc=$narrow_rc): ${why:-$(tail -3 "$narrow_out")}"; fi

# The header opens with the wallpaper's OWN name — no label, no title line —
# and closes with the CLI version the `version` subcommand reports.
if grep -q '^  narrow-check.*…$' "$narrow_out" && grep -q "^    v$cur_ver\$" "$narrow_out"; then
    pass "the stacked header leads with the truncated name and ends with v$cur_ver"
else fail "stacked header wrong: $(tail -3 "$narrow_out")"; fi
narrow_run 100
if grep -q '#.*narrow-check' "$narrow_out" && grep -q "THEME CLI *v$cur_ver" "$narrow_out"; then
    pass "the wide header carries the name beside the image and ends with v$cur_ver"
else fail "wide header wrong: $(tail -3 "$narrow_out")"; fi

# --- no kitty, no protocol: absence is clean (portability doctrine) ---------
# On an iTerm2-class terminal (no KITTY_WINDOW_ID) the screen must carry
# ZERO kitty graphics bytes — no APC, no placeholder cells — and still
# render its fields.
env COLUMNS=42 KITTY_WINDOW_ID= PATH="$narrowbin:$sweepbin:$PATH" \
    THEME_WALLPAPER_DIR="$lib" THEME_CACHE_DIR="$fixture/cache" \
    THEME_NO_APPLY=1 TMPDIR="$fixture/tmpdir" \
    "$THEME" >"$narrow_out" 2>&1
if [ "$?" = 0 ] && ! grep -q "$(printf '\033_G')" "$narrow_out" \
   && ! grep -q '#' "$narrow_out" && grep -q '^  THEME CLI$' "$narrow_out" \
   && why=$(narrowck 42 0 0); then
    pass "no kitty means no graphics bytes, fields intact"
else fail "non-kitty degradation leaked protocol: ${why:-$(tail -3 "$narrow_out")}"; fi

if [ "$fails" -eq 0 ]; then echo "ALL PASS"; else echo "$fails FAILURES"; fi
[ "$fails" -eq 0 ]
