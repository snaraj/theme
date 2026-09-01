#!/usr/bin/env bash
# Boundary fixture for the Rust `theme` binary — the port of the dotfiles
# theme-boundary-tests.sh acceptance suite. Same doctrine: destructive verbs
# act on library NAMES only; positives assert the MUTATION, not exit 0;
# refusals leave victims untouched; the network boundary runs against a
# deterministic PATH-stubbed curl; credentials never reach argv and a hostile
# credential produces ZERO transfers; every command surface is swept with
# OSC-52-poisoned inputs and may emit no terminal protocol.
#
# Two sections of the shell fixture extracted python from the script under
# test and drove it directly (the descriptor-bound saver's check/use windows,
# the ACL interrogator branches, and the contrast floor). Those trust
# decisions now live in Rust and are driven natively — src/save_tests.rs
# (FIFO-opened swap windows, forced-posix getfacl predicate, darwin ACL
# allow/deny/writesecurity) and the floor regression in src/apply.rs — so
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

fixture=$(mktemp -d -t theme-boundary) || exit 1
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
mstat=$(run_nokitty "$multi" status 2>&1)
if printf '%s' "$mstat" | grep -qF "$multi"; then
    pass "status shows the whole directory list"
else fail "status hides the extra library dirs"; fi
# Same basename in both dirs: a bare stem is ambiguous across the list.
check  "same stem across dirs refuses rm"       1 run "$multi" rm dupname
exists "first-dir dupname intact"               yes "$lib/dupname.png"
exists "second-dir dupname intact"              yes "$lib2/dupname.png"

# --- long values WRAP with a hanging indent — never merging with a label ----
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
xattr -w theme.artist "$(printf 'Ev\033]52;c;steal\ail Artist')" "$lib/bare-meta.png" 2>/dev/null
pv_out=$(COLUMNS=80 run_nokitty "$lib" preview bare-meta 2>/dev/null)
if printf '%s' "$pv_out" | grep -qF "$(printf '\033]')"; then
    fail "a metadata xattr smuggled terminal protocol into preview"
else pass "hostile metadata cannot emit terminal protocol"; fi
if printf '%s' "$pv_out" | grep -q '^  ARTIST       Ev]52;c;stealil Artist$'; then
    pass "the sanitized artist value still renders"
else fail "the artist metadata line was lost or mangled"; fi
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

# --- the DEFAULT listing is bounded: newest 10, honestly labelled -----------
for i in 1 2 3 4 5 6 7 8 9 10 11 12; do printf 'x' >"$lib/bulk-$i.jpg"; done
if run_nokitty "$lib" list 2>/dev/null | grep -q 'newest 10 of [0-9]* — more:'; then
    pass "default list bounds to the newest 10 with an honest footer"
else fail "default list bound or footer missing"; fi
rm -f "$lib"/bulk-*.jpg

# --- the save path's trust chain, end-to-end through the binary -------------
# (The check/use race windows and the forced-posix ACL predicate are driven
# natively in src/save_tests.rs; these are the end-to-end shapes.)
urlbin="$fixture/urlbin"
mkdir -p "$urlbin"
cat >"$urlbin/curl" <<'EOS'
#!/bin/bash
o=""; prev=""
for a in "$@"; do [ "$prev" = "-o" ] && o="$a"; prev="$a"; done
[ -n "$o" ] && printf '%s' 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==' | base64 -d >"$o"
exit 0
EOS
chmod +x "$urlbin/curl"
run_url() { PATH="$urlbin:$PATH" THEME_WALLPAPER_DIR="$1" THEME_NO_APPLY=1 \
    THEME_CACHE_DIR="$fixture/cache" TMPDIR="$fixture/tmpdir" "$THEME" url "$2"; }

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
    THEME_CACHE_DIR="$fixture/cache" TMPDIR="$fixture/tmpdir" "$THEME" url "$@"; }
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
    THEME_CACHE_DIR="$fixture/cache" TMPDIR="$fixture/tmpdir" "$THEME" url "$1"; }
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
    'successfully deleted' 'successfully renamed' 'unknown command'; do
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
cat >"$sweepbin/uname" <<STUB
#!/bin/bash
printf '%s' "LinuxSAFE$oscpay"
STUB
chmod +x "$sweepbin/uname"
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
for c in random set unsplash url list preview status rename rm update help; do
    help_run "$c--help" TermSAFE xtermSAFE "$c" --help
done
help_run help-TERM_PROGRAM "TermSAFE$oscpay" xtermSAFE help
help_run help-TERM '' "xtermSAFE$oscpay" help
if [ -n "$help_bad" ]; then
    fail "OSC reached the terminal from help:$help_bad"
else pass "no help path emits environment data as terminal protocol"; fi
help_missing=""
for marker in 'hlibSAFE' 'TermSAFE' 'xtermSAFE' 'LinuxSAFE' \
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
rm -f "$sweepbin/uname"

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
mkdir -p "$updd/bin" "$updd/stubbin"
cur_ver=$(printf '%s\n' "$vout" | sed -n 1p | sed 's/^version: v//')
case "$(uname -s)-$(uname -m)" in
Darwin-arm64) triple=aarch64-apple-darwin ;;
Darwin-x86_64) triple=x86_64-apple-darwin ;;
Linux-aarch64) triple=aarch64-unknown-linux-gnu ;;
*) triple=x86_64-unknown-linux-gnu ;;
esac
# The release asset shape: a tar.gz holding the single member `theme`
# (SHA256SUMS digests the TARBALL, exactly as the release workflow builds).
inner="$updd/inner"
printf 'NEW-RELEASE-BYTES-%s' "$triple" >"$updd/theme"
cp "$updd/theme" "$inner"
payload="$updd/payload.tar.gz"
tar -czf "$payload" -C "$updd" theme
rm -f "$updd/theme"
if command -v sha256sum >/dev/null 2>&1; then
    paysha=$(sha256sum "$payload" | cut -d' ' -f1)
else
    paysha=$(shasum -a 256 "$payload" | cut -d' ' -f1)
fi
cat >"$updd/stubbin/curl" <<'EOS'
#!/bin/bash
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
    # optional `--` followed by extra update arguments
    local envs=()
    while [ $# -gt 0 ] && [ "$1" != "--" ]; do envs+=("$1"); shift; done
    [ "${1:-}" = "--" ] && shift
    # rm first: macOS caches code-signing state by inode, and cp -f over a
    # previously-executed binary poisons it — the next exec dies SIGKILL.
    rm -f "$updd/bin/theme"
    cp "$THEME" "$updd/bin/theme"
    : >"$updlog"
    env UPD_LOG="$updlog" UPD_TRIPLE="$triple" UPD_PAYLOAD="$payload" \
        UPD_SUMS_FILE="$updd/sums" PATH="$updd/stubbin:$PATH" \
        THEME_WALLPAPER_DIR="$lib" THEME_CACHE_DIR="$fixture/cache" \
        THEME_NO_APPLY=1 TMPDIR="$fixture/tmpdir" \
        "${envs[@]}" "$updd/bin/theme" update "$@" >"$upd_out" 2>&1
    upd_rc=$?
}
upd_intact() { # target still byte-identical to the real binary, no temp left
    cmp -s "$THEME" "$updd/bin/theme" \
        && [ -z "$(find "$updd/bin" -name '.*update*' 2>/dev/null)" ]
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
tar -czf "$badtar" -C "$updd" not-theme
rm -f "$updd/not-theme"
if command -v sha256sum >/dev/null 2>&1; then
    badsha=$(sha256sum "$badtar" | cut -d' ' -f1)
else
    badsha=$(shasum -a 256 "$badtar" | cut -d' ' -f1)
fi
printf '%s  theme-%s.tar.gz\n' "$badsha" "$triple" >"$updd/sums"
upd_run UPD_TAG=v9.9.9 UPD_PAYLOAD="$badtar"
if [ "$upd_rc" != 0 ] && grep -q "no 'theme' binary" "$upd_out" && upd_intact; then
    pass "a verified archive without the binary is refused"
else fail "member-less archive not refused: $(cat "$upd_out")"; fi
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

upd_run UPD_TAG=v9.9.9 -- --version 0.0.0
if grep -q 'warning: older versions may be unsupported or break — proceeding to v0.0.0' "$upd_out"; then
    pass "a downgrade warns at launch and proceeds"
else fail "downgrade warning missing: $(cat "$upd_out")"; fi
if [ "$upd_rc" != 0 ] && grep -q 'no release v0.0.0' "$upd_out" && upd_intact; then
    pass "a missing tag refuses cleanly with the binary intact"
else fail "missing tag not refused cleanly: $(cat "$upd_out")"; fi

# --- the update-available footer on the bare `theme` screen ----------------
notecache="$fixture/notecache"
mkdir -p "$notecache"
failbin="$fixture/failbin"
mkdir -p "$failbin"
notefaillog="$fixture/notefail.log"
: >"$notefaillog"
cat >"$failbin/curl" <<'EOS'
#!/bin/sh
printf 'x\n' >>"$NOTE_FAIL_LOG"
exit 6
EOS
chmod +x "$failbin/curl"
note_out="$updd/note.out"
note_run() { # bare `theme` with the check ENABLED; env-pair overrides last
    env THEME_NO_UPDATE_CHECK= NOTE_FAIL_LOG="$notefaillog" \
        THEME_WALLPAPER_DIR="$lib" THEME_CACHE_DIR="$notecache" \
        THEME_NO_APPLY=1 TMPDIR="$fixture/tmpdir" KITTY_WINDOW_ID='' \
        PATH="$failbin:$sweepbin:$PATH" "$@" "$THEME" >"$note_out" 2>&1
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

if [ "$fails" -eq 0 ]; then echo "ALL PASS"; else echo "$fails FAILURES"; fi
[ "$fails" -eq 0 ]
