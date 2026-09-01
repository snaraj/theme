# pigment

The in-house wallpaper-to-palette color engine for `theme`. One decode, one
in-memory downsample, deterministic clustering, a 16-slot derivation, the
blend-aware contrast floor, and string emitters. No subprocesses, no
templating, no terminal access — applying colors anywhere is the CLI's job.

## Pipeline

```
image file ──decode──▶ ≤128×128 block-mean grid + full-res average
           ──extract─▶ k-means++ clusters (seeded, deterministic)
           ──derive──▶ 16 ANSI slots + fg/cursor (hue-archetype mapping)
           ──floor───▶ every text color readable on the EFFECTIVE background
           ──emit────▶ kitty conf | alacritty TOML | OSC sequences | cache text
```

- **decode** (`image` crate): format sniffed from the content's magic bytes
  via `with_guessed_format` — the extension is only a fallback when the
  magic is unrecognized (`image::open` alone dispatches on the extension;
  the PR #8 review caught that). Decoder allocation is bounded (512 MiB
  default) and edges are capped at 16,384 px. A single pass computes both
  the analysis grid and the full-image average color.
- **extract**: in-house k-means++ (splitmix64-seeded) over at most 16,384
  samples. The seed is fixed by default, so the same bytes always produce the
  same palette — that is what makes the gold-file tests sound and the cache
  stable.
- **derive**: ANSI 1-6 each chase their conventional hue among the clusters
  (red/green/yellow/blue/magenta/cyan); a slot with no credible cluster is
  synthesized at the archetype hue rather than duplicating a neighbour.
  Near-monochrome art takes a luma-ramp path and always derives. Light mode
  is structural (`ModePref`), not an afterthought.
- **floor**: the reviewer-hardened contrast floor from `theme.sh`, ported
  1:1 — effective background = kitty opacity blended with the wallpaper
  average; each failing color takes the smallest binary-search mix toward
  whichever of white/black can *achievably* reach the floor; strongest
  endpoint when neither can. Slot 0 is never floored. One deliberate
  difference: Rust rounds half away from zero where Python rounded half to
  even — off-by-one on exact midpoints only (common on the blend path at
  round opacities, immaterial to readability), invariant-tested. Flooring
  returns the `Floored` newtype, and the terminal emitters exist only on it:
  skipping the floor before recoloring a terminal is a type error, not a
  silent regression.
- **cache**: read-through, keyed on (canonical path, mtime, size, options,
  engine version), stored in the line-oriented `pigment1` text format. A hit
  is a file read; corrupt entries re-derive instead of erroring.

## Measured (Apple Silicon, release build)

criterion (`cargo bench -p pigment --bench stages`), synthetic 4K PNG:

| stage | time |
| --- | --- |
| derive, uncached | 32.7 ms |
| derive, cached | 25.7 µs |
| floor (16 colors) | 22.4 µs |
| emit (all four formats) | 11.3 µs |

hyperfine, real 4K photograph (JPEG), all tools uncached, 8 runs:

| tool | mean | vs pigment |
| --- | --- | --- |
| pigment (derive + floor + emit) | 177.5 ms | 1× |
| pywal ladder (`wal -n … --backend colorz --contrast 4.5`) | 1.184 s | 6.7× slower |
| wallrust v1.0.5 (`-f`) | 16.73 s | 94× slower |

Real-JPEG runs are decode-bound (the 32.7 ms → 177 ms gap is JPEG decoding);
the cached path — what `theme list`/`preview`/repeat-`set` actually hit — is
five orders of magnitude faster than a pywal invocation, which pays ~100 ms
of interpreter startup before any work.

## vs wallrust, measured

[prime-run/wallrust](https://github.com/prime-run/wallrust) (MIT) was the
study reference. The comparison sticks to three axes and to what was
measured; what pigment does not have is listed as dropped, not spun.

**Speed.** Same real 4K JPEG, same machine (Apple Silicon), every tool
uncached with caches wiped between runs — hyperfine, 8 runs: pigment
177.5 ms, the pywal ladder 1.184 s (6.7× slower), wallrust 16.73 s (94×
slower); per-stage criterion numbers are in the table above. The gap is
structural, not tuning: pigment spawns zero subprocesses and clusters a
≤128×128 grid, where wallrust shells out to ImageMagick ~52 times per
uncached run and clusters at full resolution.

**Color.** Same 16-slot output shape; two behaviors neither wallrust nor
pywal has: hue-archetype mapping so ANSI 1-6 chase their conventional hue
instead of duplicating neighbours, and the blend-aware contrast floor —
text held readable against the *effective* background a translucent
terminal actually shows. Determinism is a contract here, not a lucky
behavior: the k-means++ is seeded in-house and invariant-tested — gold-file
tests pin the palette, independent of any external binary version. wallrust
reproduces palettes only as an undocumented accident of the installed
ImageMagick build (an IM upgrade can silently shift every palette; nothing
there tests or pins it), and pywal not at all on its randomly-initialised
colorz-class backends. One idea flows the other way and is used here with
attribution: the fixed accent-curve ladder and the dark/light sort on
correct WCAG relative luminance descend from wallrust, © prime-run, MIT
license.

**Security and safety.** One audited runtime dependency (`image`, exactly
the `THEME_FORMATS` codecs) vs wallrust's 14. Content-sniffed decode bounded
at 512 MiB allocation and 16,384 px per edge. No template engine, so no
shell-expanded template-directed filesystem writes (wallrust's `{# output:
… #}` directives write to arbitrary expanded paths). Subprocess-free, so no
exit-status masking (wallrust treats a `magick` run whose stderr contains
"warning:" as success). `unsafe_code = "deny"` enforced from the workspace
lint table. 33 tests vs 0.

**Deliberately dropped, not improved on:** wallrust's tera templating
breadth (arbitrary user templates), HTML palette preview, and Hyprland
detection. The CLI this engine serves needs none of them; if that changes,
they are features to build, not gaps that were closed.

**Size, told with the tests in:** 1,795 lines of Rust including all tests,
benches, and the example harness, vs wallrust's 2,163 with zero tests.

## Dependencies

`image` (default features off, exactly the `THEME_FORMATS` codecs: jpeg,
png, webp, gif, bmp, tiff) is the sole runtime dependency — decoding hostile
image bytes is precisely where a fuzzed, widely-audited crate beats in-house
code.
`criterion` is dev-only. Everything else — PRNG, k-means, color math, WCAG
contrast, FNV cache keys, emitters — is in-house (1,795 lines including all
tests, benches, and the example harness).

## Invariants the tests pin

- Determinism: same bytes + same `Options` ⇒ identical palette.
- Floor: post-floor contrast ≥ target against the effective background, or
  the target is provably unreachable from both endpoints and the strongest
  endpoint was taken. Background slot untouched; cursor untouched (shell
  parity).
- Grayscale art derives a usable ramp; it never errors.
- ANSI slots 1-6 are pairwise distinct on multi-color art.
- Cache: hit ≡ miss; content/mtime change invalidates; corrupt entry
  re-derives.
