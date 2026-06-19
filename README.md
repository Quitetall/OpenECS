# OpenECS — the Open EEG Codec Standard

[![crates.io](https://img.shields.io/crates/v/eeg-codec-standard.svg)](https://crates.io/crates/eeg-codec-standard)

A **universal, vendor-neutral benchmark standard for EEG signal compression.**
Grade any codec — lossless, lossy, neural, classical, hybrid, **in any
language** — against a hash-pinned corpus and get a single, reproducible,
comparable verdict.

- **Crate:** `eeg-codec-standard` · **library:** `eeg_codec_standard` ·
  **reference CLI:** `openecs`
- **Spec:** [`SPEC/OpenECS-v1.0.md`](SPEC/OpenECS-v1.0.md) (normative) ·
  [`STANDARD.md`](STANDARD.md) (narrative) · JSON schemas in `SPEC/schemas/`

## Tiers (strictness order)

| Grade | Tier | Gate |
|------|------|------|
| `ECS-L` | Lossless | bit-exact (PRD = 0), CR ≥ 0.8 |
| `ECS-N` | Near-Lossless | R ≥ 0.99, PRD ≤ 5 %, CR ≥ 1.0 |
| `ECS-C` | Clinical | R ≥ 0.95, PRD ≤ 9 %, CR ≥ 20 + per-band |
| `ECS-M` | Monitoring | R ≥ 0.85, PRD ≤ 20 %, CR ≥ 100 + per-band |
| `ECS-A` | Alerting | R ≥ 0.70, PRD ≤ 40 %, CR ≥ 200 + per-band |

## Grade your codec (any language)

Your codec just needs an executable exposing the file-based contract
(`encode <in.edf> <out.blob>` / `decode <in.blob> <out.raw> --channels …`),
described by a TOML manifest. Then:

```bash
openecs bench --codec-manifest mycodec.toml \
    --corpus-manifest bench/ECS-Bench-v1/ECS-Bench-v1.toml \
    --report report.html --charts
```

→ a ranked, colored leaderboard with 95 % confidence intervals and a paired
significance test vs the baselines, terminal charts, and a self-contained HTML
report. Built-in reference codecs (`store`, `gzip`, `zstd`) ship for comparison;
an offline synthetic smoke corpus (`corpora/ecs-smoke.toml`) needs zero
downloads.

See [`STANDARD.md`](STANDARD.md) §6 for the codec contract and
[`bench/ECS-Bench-v1/`](bench/ECS-Bench-v1/README.md) for the canonical,
publicly-downloadable benchmark corpus.

## License

Apache 2.0
