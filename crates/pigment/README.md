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

- **decode** (`image` crate, decoders only): format sniffed from content,
  never the extension. A single pass computes both the analysis grid and the
  full-image average color.
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
  even — off-by-one on exact midpoints only, invariant-tested.
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

## Why not wallrust?

[prime-run/wallrust](https://github.com/prime-run/wallrust) (MIT) was the
study reference. It is a CLI orchestrator, not an engine: every analysis step
shells out to ImageMagick (~52 subprocess spawns per uncached run measured on
defaults), k-means runs on the full-resolution image, and the 2,163-line tree
carries zero tests and 14 runtime dependencies. Two ideas were worth
deriving, with thanks: the fixed accent-curve ladder (our bright-variant
step) and sorting by correct WCAG relative luminance. Everything else here is
independent implementation.

Attribution: the accent-ladder concept and dark/light sort flow descend from
wallrust, © prime-run, MIT license — see the repository above.

## Dependencies

`image` (decoders only, exactly the `THEME_FORMATS` codecs: jpeg, png, webp,
gif, bmp, tiff) is the sole runtime dependency — decoding hostile image bytes
is precisely where a fuzzed, widely-audited crate beats in-house code.
`criterion` is dev-only. Everything else — PRNG, k-means, color math, WCAG
contrast, FNV cache keys, emitters — is in-house (~1,700 lines including all
tests and benches; wallrust is 2,163 with neither).

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
