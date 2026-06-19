# OpenECS specification changelog

All notable changes to the OpenECS standard. The spec is versioned
independently of the `eeg-codec-standard` crate. See
[`OpenECS-v1.0.md`](OpenECS-v1.0.md) §11 for the version/stability policy.

## 1.0 — 2026-06-19 (first OpenECS release)

The Open EEG Codec Standard: a vendor-neutral, codec-agnostic benchmark for EEG
compression. (Lineage: extracted and rebranded from an in-house quality
standard; OpenECS v1.0 is the first independent, public release.)

### The standard
- **Five tiers**, strictness order **L < N < C < M < A**:
  - `ECS-L` Lossless — bit-exact (integer-domain PRD = 0), CR ≥ 0.8.
  - `ECS-N` Near-Lossless — R ≥ 0.99, PRD ≤ 5 %, CR ≥ 1.0, no per-band reqs
    (the strongest non-lossless tier; for small-error, not-bit-exact codecs).
  - `ECS-C` Clinical — R ≥ 0.95, PRD ≤ 9 %, CR ≥ 20 + per-band δθαβγ floors.
  - `ECS-M` Monitoring — R ≥ 0.85, PRD ≤ 20 %, CR ≥ 100 + per-band.
  - `ECS-A` Alerting — R ≥ 0.70, PRD ≤ 40 %, CR ≥ 200 + per-band.
- **Codec-conformance contract** (§6): a file-based CLI
  (`encode <in> <out>` / `decode <in> <out> --channels --samples --rate --dtype`)
  that makes ANY codec, in ANY language, gradable with no source integration.
- **Codec manifest** (§7) + **corpus manifest** (§8) schemas (JSON Schema
  mirrors under `schemas/`), and a **results-submission envelope** (§9,
  `EcsSubmission` wrapping per-file `EcsReport`s).
- **Verify-don't-trust** lossless gate (integer-domain exact equality).
- **Optional task-concordance axis** (§10) — codec-agnostic downstream-task
  preservation, reported separately and **out of the tier gates**.
- **Version/stability policy** (§11): SemVer-style; a threshold/metric/L-gate
  change is a major bump. A grader accepts manifests of its major or older.

### Reference tooling (`openecs` CLI)
- `openecs bench` — grade the codec under test + built-in baselines, ranked,
  with a **95 % bootstrap CI** on mean R and a **paired sign-test** p-value vs
  the strongest baseline.
- Parallel, bounded-memory corpus grading (`rayon`) with a live progress bar;
  **median throughput** (citable). Colored unicode-boxed read-out + grade
  badges + sparklines; ASCII charts (`--charts`); a self-contained **HTML
  report** (`--report`, inline SVG, no JS). Real `--help` (clap).
- **ECS-Bench-v1** — the canonical, hash-pinned, publicly-downloadable corpus
  (PhysioNet CHB-MIT subset) under `bench/ECS-Bench-v1/`; an offline synthetic
  smoke corpus (`corpora/ecs-smoke.toml`) is the zero-download default.

### Notes
- Canonical thresholds live in `src/levels.rs` and are pinned by
  `tests/spec_thresholds.rs`. Confidence intervals + significance are derived at
  render time (not stored in the submission). `throughput_mibs` is rounded to
  0.001 MiB/s (stable + JSON-round-trip safe).
