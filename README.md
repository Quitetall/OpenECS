# OpenECS — the Open EEG Codec Standard

[![crates.io](https://img.shields.io/crates/v/eeg-codec-standard.svg)](https://crates.io/crates/eeg-codec-standard)
[![license](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

A **universal, vendor-neutral benchmark standard for EEG signal compression.**
Grade any codec — lossless, lossy, neural, classical, hybrid, **in any
language** — against a hash-pinned corpus and get a single, reproducible,
comparable verdict.

- **Crate:** `eeg-codec-standard` · **library:** `eeg_codec_standard` ·
  **reference CLI:** `openecs`
- **Spec:** [`SPEC/OpenECS-v1.0.md`](SPEC/OpenECS-v1.0.md) (normative) ·
  [`STANDARD.md`](STANDARD.md) (narrative) · JSON schemas in `SPEC/schemas/`

## Tiers (strictness order L < N < C < M < A)

| Grade | Tier | Gate |
|-------|------|------|
| `ECS-L` | Lossless | bit-exact (integer-domain PRD = 0), CR ≥ 0.8 |
| `ECS-N` | Near-Lossless | R ≥ 0.99, PRD ≤ 5 %, CR ≥ 1.0 |
| `ECS-C` | Clinical | R ≥ 0.95, PRD ≤ 9 %, CR ≥ 20 + per-band δθαβγ |
| `ECS-M` | Monitoring | R ≥ 0.85, PRD ≤ 20 %, CR ≥ 100 + per-band |
| `ECS-A` | Alerting | R ≥ 0.70, PRD ≤ 40 %, CR ≥ 200 + per-band |
| `—` | below floor | fails even the A gate |

The grade is the **highest tier a codec fully satisfies**, verified through its
encode/decode boundary only — the lossless claim is *checked, not trusted*.

## Install

```bash
# the `openecs` command-line benchmark
cargo install eeg-codec-standard

# or use it as a library
cargo add eeg-codec-standard      # then: use eeg_codec_standard::...
```

## 60-second quickstart (zero download)

A tiny synthetic smoke corpus ships in-repo, so you can grade immediately:

```bash
# grade a codec + built-in baselines (store, gzip), ranked, with an HTML report
openecs bench --codec gzip --corpus-manifest corpora/ecs-smoke.toml \
    --charts --report report.html
```

Out comes a colored, ranked leaderboard (grade + pooled CR + mean-R 95 % CI +
a paired significance test vs the strongest baseline), terminal charts, and a
self-contained `report.html` (inline SVG, no JS):

```
  rank  codec      grade   pooled CR   mean R (95% CI)            PRD%   p vs base
    1   gzip       ECS-L      24.57:1   1.0000 [1.0000,1.0000]    0.00       —
    2   store      ECS-L       1.00:1   1.0000 [1.0000,1.0000]    0.00    1.0000
```

Other subcommands: `openecs grade` (one codec), `openecs verify-corpus`
(check a corpus's SHA-256 pins), `openecs emit-corpus-manifest` (hash a directory
of EDFs into a pinned manifest). Run `openecs <cmd> --help` for each.

## Grade YOUR codec (any language)

Your codec just needs an executable that speaks the **file-based contract**
(full spec: [`SPEC/OpenECS-v1.0.md`](SPEC/OpenECS-v1.0.md) §6):

```text
<cmd> [prefix…] encode <in.edf>  <out.blob>
<cmd> [prefix…] decode <in.blob> <out.raw> --channels N --samples M --rate FS --dtype DT
```

Describe how to invoke it in a `codec-manifest.toml` ([schema](SPEC/schemas/codec-manifest-v1.json)):

```toml
spec_version = "1.0"
[codec]
name = "my-codec"
cmd = "python3"                  # any binary/script; resolved on PATH
prefix_args = ["my_codec.py"]
declared_lossless = false        # your claim — verified, not trusted
sample_dtype = "i32"             # raw decode width: i16 | i32 | i64
input_format = "edf"             # edf (default) | ecs0
output_format = "raw"            # raw (default) | ecs0
```

Then benchmark it against the baselines:

```bash
openecs bench --codec-manifest codec-manifest.toml \
    --corpus-manifest corpora/ecs-smoke.toml --report report.html
```

No Rust, no source access, no special build — the standard grades whatever comes
out the other end.

## Use it as a library

```rust
use eeg_codec_standard::{adapter::Gzip, harness};

// `signal` is one Vec<i64> per channel (integer ADC counts); `fs` is the rate.
let report = harness::run(&Gzip, &signal, 256.0);
println!("grade ECS-{}  CR {:.2}  R {:.4}", report.grade, report.cr, report.r);
// Implement the `Codec` trait for your own in-process codec, or drive an
// external one via `eeg_codec_standard::manifest` + `corpus::grade_manifest_parallel`.
```

## Canonical corpus (cross-lab comparable numbers)

For citable results, grade against the pinned public corpus
[`bench/ECS-Bench-v1/`](bench/ECS-Bench-v1/README.md) (PhysioNet CHB-MIT subset):

```bash
cd bench/ECS-Bench-v1
sh fetch.sh                                              # download (~170 MB)
openecs verify-corpus --corpus-manifest ECS-Bench-v1.toml   # SHA-256 + shape
openecs bench --codec-manifest <yours> --corpus-manifest ECS-Bench-v1.toml --report r.html
```

Data is not redistributed — only the record list + SHA-256 pins, so every lab
grades byte-identical data.

## License

Apache-2.0. See [LICENSE](LICENSE).
