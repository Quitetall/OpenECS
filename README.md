# OpenECS — Open EEG Codec Standard

[![crates.io](https://img.shields.io/crates/v/open-eeg-codec-standard.svg)](https://crates.io/crates/open-eeg-codec-standard)
[![license](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Vendor-neutral benchmark standard for EEG signal compression. Grades **any codec in any language** against a hash-pinned corpus and produces a single reproducible compliance verdict.

- **Crate:** `open-eeg-codec-standard` · **library:** `open_eeg_codec_standard` · **CLI:** `openecs`
- **Spec:** [`SPEC/OpenECS-v1.0.md`](SPEC/OpenECS-v1.0.md) (normative) · [`STANDARD.md`](STANDARD.md) (narrative) · JSON schemas in `SPEC/schemas/`

## Tiers

| Grade | Tier | Global gate | Per-band (δθαβγ) |
|-------|------|-------------|-----------------|
| `ECS-L` | Lossless | integer-domain PRD = 0 exactly, CR ≥ 0.8 | — |
| `ECS-N` | Near-Lossless | R ≥ 0.99, PRD ≤ 5 %, CR ≥ 1.0 | — |
| `ECS-C` | Clinical | R ≥ 0.95, PRD ≤ 9 %, CR ≥ 20 | required |
| `ECS-M` | Monitoring | R ≥ 0.85, PRD ≤ 20 %, CR ≥ 100 | required |
| `ECS-A` | Alerting | R ≥ 0.70, PRD ≤ 40 %, CR ≥ 200 | required |
| `—` | below floor | fails ECS-A | — |

Grade is the highest tier fully satisfied. The lossless claim (ECS-L) is verified by integer-domain bit comparison — not inferred from the codec's own declaration.

## Install

```bash
cargo install open-eeg-codec-standard   # openecs CLI
cargo add open-eeg-codec-standard       # library dependency
```

## Quickstart

```bash
# Grade gzip + built-in baselines against the in-repo synthetic corpus
openecs bench --codec gzip --corpus-manifest corpora/ecs-smoke.toml \
    --charts --report report.html
```

Produces a ranked leaderboard (grade, pooled CR, mean-R 95 % bootstrap CI, paired sign-test p-value vs strongest baseline), terminal ASCII charts, and a self-contained HTML report with inline SVG.

Other subcommands: `grade` (single codec, single signal), `verify-corpus` (SHA-256 + shape check), `emit-corpus-manifest` (hash a directory of EDFs into a pinned manifest).

## Codec contract (any language)

Two executables satisfying the file-based contract qualify:

```text
<cmd> [prefix…] encode <in.edf>  <out.blob>
<cmd> [prefix…] decode <in.blob> <out.raw> --channels N --samples M --rate FS --dtype DT
```

Declare the invocation in a `codec-manifest.toml` ([schema](SPEC/schemas/codec-manifest-v1.json)):

```toml
spec_version = "1.0"
[codec]
name            = "my-codec"
cmd             = "python3"
prefix_args     = ["my_codec.py"]
declared_lossless = false       # verified against actual output, not trusted
sample_dtype    = "i32"         # decode output width: i16 | i32 | i64
input_format    = "edf"         # edf (default) | ecs0
output_format   = "raw"         # raw (default) | ecs0
```

```bash
openecs bench --codec-manifest codec-manifest.toml \
    --corpus-manifest corpora/ecs-smoke.toml --report report.html
```

## Library

```rust
use open_eeg_codec_standard::{adapter::Gzip, harness};

// signal: one Vec<i64> per channel (integer ADC counts)
let report = harness::run(&Gzip, &signal, 256.0);
println!("ECS-{}  CR {:.2}  R {:.4}", report.grade, report.cr, report.r);
```

Implement the `Codec` trait for an in-process codec, or use `manifest` + `corpus::grade_manifest_parallel` to drive an external subprocess against a full corpus.

## Canonical corpus

For citable results use [`bench/ECS-Bench-v1/`](bench/ECS-Bench-v1/README.md) (CHB-MIT subset, 4 recordings, 23 ch, 256 Hz, SHA-256 pinned):

```bash
cd bench/ECS-Bench-v1 && sh fetch.sh
openecs verify-corpus --corpus-manifest ECS-Bench-v1.toml
openecs bench --codec-manifest <yours> --corpus-manifest ECS-Bench-v1.toml --report r.html
```

## License

Apache-2.0 — see [LICENSE](LICENSE).
