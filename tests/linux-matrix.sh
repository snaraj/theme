#!/usr/bin/env bash
# Does the PUBLISHED Linux binary run on the distributions people use?
#
# ci.yml runs the source and the whole boundary fixture on ubuntu-latest
# every push. What no runner can prove is the RELEASE TARBALL — the artifact
# a stranger downloads — because the binary inherits the glibc floor of the
# runner that built it, and that floor is invisible until it meets an older
# distribution. This walks the images and reports one line each. Through
# v0.2.2 that floor is glibc 2.39, so ubuntu:22.04 and debian:12 report FAIL
# here: the gap this exists to name, not a fault in the script.
#
# Nothing is installed inside the images: a base image already carries tar,
# sha256sum and base64, so a failure here is the binary's, never a missing
# package. Every invocation is THEME_NO_APPLY=1 against a scratch library
# and cache, so no container touches a desktop.
#
# Needs docker. Arch publishes x86_64 images only and Alpine is musl (the
# glibc tarball cannot run there, by design) — add either deliberately:
#   IMAGES='archlinux:latest' tests/linux-matrix.sh
set -u
IMAGES=${IMAGES:-"ubuntu:24.04 ubuntu:22.04 debian:12 fedora:latest"}
case "$(uname -m)" in
    arm64 | aarch64) arch=aarch64; platform=linux/arm64 ;;
    *) arch=x86_64; platform=linux/amd64 ;;
esac
tgz="theme-$arch-unknown-linux-gnu.tar.gz"
base=https://github.com/snaraj/theme/releases/latest/download
work=$(mktemp -d "${TMPDIR:-/tmp}/theme-matrix.XXXXXX") || exit 1
trap 'rm -rf "$work"' EXIT
curl -fsSL -o "$work/$tgz" "$base/$tgz" || { echo "FAIL  cannot fetch $tgz"; exit 1; }
curl -fsSL -o "$work/SHA256SUMS" "$base/SHA256SUMS" || exit 1
# A 1x1 PNG, decoded INSIDE the container: a library needs one real image,
# and macOS and GNU base64 spell the decode flag differently.
png=iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==
probe='
set -e
cd /m && grep " $TGZ$" SHA256SUMS | sha256sum -c - >/dev/null
mkdir -p /t/lib && cd /t && tar -xzf "/m/$TGZ"
printf %s "$PNG" | base64 -d >/t/lib/tiny.png
export THEME_NO_APPLY=1 THEME_NO_UPDATE_CHECK=1 THEME_CACHE_DIR=/t/cache \
    THEME_WALLPAPER_DIR=/t/lib HOME=/t COLUMNS=100
try() { # keep the diagnosis: a loader refusal explains itself on stderr
    err=$("$@" 2>&1 >/dev/null) || { echo "${*#./}: ${err:-failed}"; exit 1; }
}
for verb in version help list random status; do try ./theme "$verb"; done
try ./theme set tiny.png
try ./theme preview tiny.png
'
fails=0
for img in $IMAGES; do
    if out=$(docker run --rm --platform "$platform" -e TGZ="$tgz" -e PNG="$png" \
        -v "$work":/m:ro "$img" sh -c "$probe" 2>&1); then
        printf 'PASS  %-20s %s: 7 commands\n' "$img" "$arch"
    else
        printf 'FAIL  %-20s %s\n' "$img" "$(printf '%s' "$out" | tail -1)"
        fails=$((fails + 1))
    fi
done
[ "$fails" = 0 ] && echo "ALL PASS" || echo "$fails FAILURES"
[ "$fails" = 0 ]
