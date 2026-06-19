# ECS-Bench-v1 — the canonical EEG-compression benchmark corpus

A fixed, hash-pinned, **publicly downloadable** corpus so any two labs grade
their codecs against byte-identical data and get directly comparable OpenECS
numbers. This is the corpus to cite when you report an OpenECS grade.

## What it is

- **Source:** PhysioNet **CHB-MIT Scalp EEG Database v1.0.0**
  (<https://physionet.org/content/chbmit/1.0.0/>), licensed **ODC-BY 1.0**.
- **Selection:** a fixed subset of subject `chb01` (see [`records.txt`](records.txt)) —
  scalp EEG, **256 Hz, uniform sample rate**, ~23 channels, mixing
  seizure-bearing and interictal hours.
- **Pinned:** [`ECS-Bench-v1.toml`](ECS-Bench-v1.toml) lists every file with its
  SHA-256 + shape. `verify-corpus` refuses to grade if a byte differs, so the
  numbers are reproducible.

The raw EEG is **not** redistributed here (it carries its own license and is
large); only the record list and the pins are. You fetch the data once.

## Why CHB-MIT / the single-rate constraint

The bundled pure-Rust EDF reader grades a **single shared sample rate** across
channels (it never silently mixes rates). CHB-MIT is uniformly 256 Hz, so it
qualifies; multi-rate corpora (e.g. Sleep-EDF, whose EMG/respiration channels
differ) are **excluded** until a multi-rate reader lands. Siena (512 Hz uniform)
is a candidate for a future `ECS-Bench-v2`.

## Use it

Run from **this directory** (`bench/ECS-Bench-v1/`); the committed manifest's
paths (`data/chb01/*.edf`) line up with `fetch.sh`'s default `./data`, so it
works with no re-locking:

```bash
# 1. Fetch the canonical files (~170 MB) into ./data
sh fetch.sh

# 2. Verify the download matches the committed pins (SHA-256 + shape)
openecs verify-corpus --corpus-manifest ECS-Bench-v1.toml

# 3. Benchmark your codec against the baselines, with charts + an HTML report
openecs bench --codec-manifest YOUR_CODEC.toml --corpus-manifest ECS-Bench-v1.toml \
    --report report.html --charts
```

`ECS-Bench-v1.toml` is the **committed reference lock**: its SHA-256 values are
the canonical bytes your download must match (verified against the real
PhysioNet files). To stage the data elsewhere, re-lock with
`openecs emit-corpus-manifest --root <DIR> --name ECS-Bench-v1 --version 1.0.0`
(same hashes, your paths).

## Offline / no download

For a zero-download smoke run, use the in-repo synthetic corpus
[`../../corpora/ecs-smoke.toml`](../../corpora/ecs-smoke.toml) (the
**ECS-Bench-mini** default) — deterministic and license-clean, but synthetic.
`ECS-Bench-v1` is the corpus for real, citable numbers.
