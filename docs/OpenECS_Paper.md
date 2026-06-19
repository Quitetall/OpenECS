---
title: "OpenECS: An Open Standard and Benchmarking Suite for EEG Signal Compression"
tags:
  - EEG
  - signal compression
  - benchmarking
  - biomedical signal processing
  - Rust
authors:
  - name: Brian Lam
    orcid: 0000-0000-0000-0000
    affiliation: 1
affiliations:
  - name: Independent Researcher
    index: 1
date: 2026-06-19
bibliography: OpenECS_Paper.bib
---

# Summary

OpenECS (Open EEG Codec Standard) is a vendor-neutral framework for evaluating
and comparing electroencephalogram (EEG) signal compression codecs. It defines
a five-tier compliance ladder (L/N/C/M/A) grounded in clinically motivated
signal-fidelity thresholds, provides a file-based codec contract that admits
implementations in any programming language, and ships a benchmarking harness
with hash-pinned corpus manifests, 95% bootstrap confidence intervals, and
paired sign-test comparisons. The standard is distributed as a Rust library
(`open-eeg-codec-standard`) and a command-line tool (`openecs`) on crates.io.

# Statement of Need

EEG is a high-bandwidth biosignal routinely recorded at 256–2048 Hz across
32–256 channels, generating multi-gigabyte datasets over typical clinical or
research sessions. Compression is therefore practical in every stage of the
signal chain: on-device storage in wearables and implants, transmission from
bedside monitors to servers, and long-term archival of large corpora such as the
Temple University EEG Corpus [@obeid2016] and CHB-MIT Scalp EEG Database
[@shoeb2009]. Despite this ubiquity, the field lacks a shared evaluation
protocol.

Published EEG compression studies differ in metric definition, signal
preparation, and reporting granularity. Percent Root-mean-square Difference
(PRD) is the most common scalar quality measure [@norris2005], but researchers
apply it to raw digital samples, calibrated physical units, and channel-demeaned
variants (PRDN) interchangeably [@memon2020]. Compression ratio (CR) is defined
against different denominators—raw samples, the EDF container, or a fixed-width
PCM baseline. Pearson correlation R is reported at various temporal and spectral
granularities, or omitted entirely. As a result, a codec claiming "PRD < 10%"
may be incommensurable with a second claiming "R > 0.95" on the same underlying
signal [@hejrati2019].

Two additional gaps compound the comparison problem. First, most codec papers
evaluate on small, non-public signal excerpts; reproducibility requires both the
exact corpus and the exact preprocessing pipeline. Second, existing audio codec
evaluation frameworks (e.g., the MUSHRA listening test for perceptual audio,
objective metrics libraries for speech codecs) are inappropriate for EEG because
they assume perceptual criteria and do not expose the sub-band fidelity
requirements that matter clinically—delta-band morphology for seizure detection,
alpha-band power for anesthesia monitoring, and gamma-band coherence for
cognitive research [@niedermeyer2011].

OpenECS addresses all three gaps: a fixed, agreed-upon metric protocol; a
hash-pinned public corpus (`ECS-Bench-v1`, derived from CHB-MIT at 256 Hz, 23
channels); and per-clinical-band (δ/θ/α/β/γ) fidelity gates that are part of
the compliance verdict rather than optional secondary reporting.

# The OpenECS Standard

## Tiers

A codec is assigned the highest tier it fully satisfies. Tiers are evaluated in
descending order of strictness:

| Tier | Name | R ≥ | PRD ≤ | CR ≥ | Per-band |
|------|------|-----|-------|------|----------|
| **L** | Lossless | 1.0 (exact) | 0.0 (exact) | 0.8 | — |
| **N** | Near-Lossless | 0.99 | 5.0% | 1.0 | — |
| **C** | Clinical | 0.95 | 9.0% | 20.0 | δθαβγ |
| **M** | Monitoring | 0.85 | 20.0% | 100.0 | δθαβγ |
| **A** | Alerting | 0.70 | 40.0% | 200.0 | δθαβγ |

The **L** tier requires integer-domain bit-exact reconstruction (PRD
exactly zero on the digital sample values) and does not apply the continuous PRD
formula. The **N** tier is the strictest lossy grade: distortion is small enough
that inter-rater clinical agreement is preserved, but losslessness is not
guaranteed. Tiers **C**, **M**, and **A** additionally require each of the five
clinical frequency bands to meet their own per-band R and PRD floors, so a codec
that achieves good global metrics but destroys delta-band morphology cannot
claim Clinical compliance.

The per-band check is computed by bandpass-filtering both the original and
reconstructed signals through non-overlapping fifth-order Butterworth filters at
the standard clinical boundaries (δ: 0.5–4 Hz; θ: 4–8 Hz; α: 8–13 Hz; β:
13–30 Hz; γ: 30–100 Hz) and computing R and PRD independently in each band.

## Metric Definitions

All continuous metrics are computed on the flattened multichannel signal
(channels concatenated in order). PRD is:

$$\text{PRD} = 100 \cdot \sqrt{\frac{\sum (x_i - \hat{x}_i)^2}{\sum x_i^2}}$$

Pearson R is computed over the flattened pair. CR is the ratio of the raw
serialized byte count to the compressed byte count, measured against the
reference `Store` baseline (identity encoding to an 8-bytes-per-sample i64
container) so that every codec's ratio is measured against the same denominator
regardless of input format.

## Codec Contract

Any codec in any language qualifies as long as it can be invoked as two
executables satisfying:

```
encode  <input.edf>   <output.blob>
decode  <input.blob>  <output.raw>  --channels N --samples M --rate FS --dtype DT
```

A TOML manifest declares the binary paths, the declare-lossless flag, and
optional metadata. This contract means that MATLAB codecs, Python pipelines,
compiled C tools, and hardware decoders are all graded on equal footing without
requiring any source-level integration.

## Corpus Manifests

Corpus manifests are TOML files that pin each EDF file by SHA-256 digest, number
of channels, total sample count, and sample rate. The harness verifies all
digests before any codec is invoked; a mismatched file aborts with an error
rather than silently grading a different signal. `ECS-Bench-v1` ships four
CHB-MIT recordings (subject 01, files 01–04; 23 channels, 256 Hz, 921,600
samples each) with verified SHA-256 hashes as the canonical open benchmark.

## Statistical Reporting

The `bench` subcommand runs every registered codec plus three built-in baselines
(Store, Gzip, optionally Zstd) in parallel with a live progress bar, then
reports:

- Mean and 95% bootstrap confidence intervals on R (1000 resamples, seeded
  for reproducibility) for each codec.
- A paired Wilcoxon sign test p-value against the strongest baseline, so an
  improvement claim has a formal statistical backing [@wilcoxon1945].
- An HTML report with inline SVG charts.

# Implementation

The library core (`metrics.rs`, `bands.rs`, `levels.rs`) is pure Rust with no
non-trivial dependencies beyond `nalgebra` for matrix operations and `statrs`
for the bootstrap resampling distribution. The codec contract adapter layer
(`adapter.rs`, `adapters_external.rs`) wraps subprocess invocations and handles
the self-describing `ECS0` envelope format used by the reference codecs. The
corpus, manifest, and suite modules are I/O-only. The CLI (`openecs`) is the
sole consumer of the terminal-experience crate dependencies (`clap`, `indicatif`,
`console`, `plotters`, `textplots`), so the library can be embedded in other
tools without pulling in those dependencies transitively.

The minimum supported Rust version (MSRV) is 1.74 (released October 2023),
chosen to match broad CI toolchain availability without blocking any
currently-maintained Rust installation.

# Comparison with Related Work

BrainCodec [@défossez2024] is a neural EEG codec evaluated on TUAB and TUEG.
It reports PRD and compression ratio but uses proprietary evaluation code and
a non-public training split; its results cannot be reproduced without the
original model weights and data access. SNAC [@siuzdak2024] targets audio
signals and cannot be applied to EEG without modification. FEMBA [@liu2024] is
a transformer-based EEG compressor evaluated on a single dataset without
per-band fidelity reporting. None of these provide a reproducible, corpus-pinned
benchmark that a third party can run against an independent codec; OpenECS fills
this gap.

# Usage

```bash
# Install the CLI
cargo install open-eeg-codec-standard

# Grade a codec in 30 seconds (no corpus download needed)
openecs bench --codec gzip \
              --corpus-manifest corpora/ecs-smoke.toml \
              --charts

# Grade against the canonical public corpus
openecs verify-corpus bench/ECS-Bench-v1/ECS-Bench-v1.toml
openecs bench --codec gzip \
              --corpus-manifest bench/ECS-Bench-v1/ECS-Bench-v1.toml \
              --report report.html

# Grade a custom codec via the file contract
openecs bench --codec-manifest my_codec.toml \
              --corpus-manifest bench/ECS-Bench-v1/ECS-Bench-v1.toml
```

# Availability

OpenECS is released under the Apache 2.0 license. Source code, specification,
and canonical corpus manifest are available at
<https://github.com/Quitetall/OpenECS>. The crate is published at
<https://crates.io/crates/open-eeg-codec-standard> (version 1.0.0).

# References
