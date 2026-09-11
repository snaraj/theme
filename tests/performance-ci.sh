#!/usr/bin/env bash
# Compare identically built source revisions; artifacts survive a failing gate.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
base="${THEME_PERF_BASE:?set THEME_PERF_BASE to the reviewed base commit}"
[[ "$base" =~ ^[0-9a-f]{40}$ && "$base" != 0000000000000000000000000000000000000000 ]] || {
    echo "performance: invalid or absent baseline commit" >&2; exit 1;
}
python3 -I -B "$root/tests/performance.py" --self-test
git -C "$root" cat-file -e "$base^{commit}"
mkdir -p "$root/target"
source_dir="$(mktemp -d "$root/target/performance-base.XXXXXX")"
cleanup() { git -C "$root" worktree remove --force "$source_dir" >/dev/null 2>&1 || true; }
trap cleanup EXIT
git -C "$root" worktree add --detach "$source_dir" "$base"
cargo build --manifest-path "$source_dir/Cargo.toml" --release --locked \
    --target-dir "$root/target/performance-base-build"
cargo build --manifest-path "$root/Cargo.toml" --release --locked
export THEME_PERF_BASE_SHA="$base"
export THEME_PERF_HEAD_SHA="$(git -C "$root" rev-parse HEAD)"
python3 -I -B "$root/tests/performance.py" \
    --before "$root/target/performance-base-build/release/theme" \
    --after "$root/target/release/theme" --built-artifacts --output "$root/target/performance"
