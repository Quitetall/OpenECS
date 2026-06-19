# OpenECS v1.0 — A Universal Standard for EEG Signal Compression

**Status:** Released · **Version:** 1.0 · **Spec ID:** `ECS-1.0`

OpenECS (the Open EEG Codec Standard) is a **vendor-neutral, codec-agnostic**
benchmark for grading the quality of EEG signal compression. Any lab can take any
codec — lossless, lossy, neural, classical, hybrid, in any programming language —
wrap it behind one small file-based interface, and obtain a single, reproducible
compliance verdict that is directly comparable to every other codec graded the
same way against the same corpus.

This document is the **frozen, versioned normative specification** for OpenECS v1.0.
The canonical machine-readable source of every threshold and formula is the Rust
crate alongside this file (`src/levels.rs`, `src/metrics.rs`, `src/bands.rs`,
`src/harness.rs`); where a number appears here it is taken verbatim from that
implementation. The living narrative companion is [`../STANDARD.md`](../STANDARD.md).

Each section is badged **Normative** (a conformance requirement) or
**Informative** (rationale / examples). Only Normative text constrains a
conforming codec, grader, or submission.

---

## 1. Scope and terminology

> **Normative.**

OpenECS grades a codec through its **public encode/decode boundary only**. It never
reaches inside the codec; grading is a function of *observable behaviour*: the
compressed size produced, the signal reconstructed after a full encode → decode
round trip, and the wall-clock time and working-set bytes that round trip cost.

Terms used throughout, with the meaning fixed for v1.0:

- **Signal.** A per-channel stream of integer samples in the ADC-count (digital)
  domain: one ordered sequence of `i64` samples per channel. Channels may differ
  in length (ragged) and a channel may be empty.
- **`fs`.** The sample rate in Hz, shared metadata the grader holds; a codec may
  use or ignore it.
- **Blob.** The opaque byte string a codec's `encode` produces and its `decode`
  consumes. OpenECS imposes no structure on the blob.
- **Round trip.** `decode(encode(signal))`; its output is the **reconstruction**.
- **Grade.** The single tier code a codec earns on a signal or corpus: one of
  `L`, `C`, `M`, `A`, or the below-floor sentinel.
- **Grader.** A conforming implementation of this spec that drives a codec and
  produces an `EcsReport`. The Rust crate beside this file is the **reference
  grader** and the canonical authority for all thresholds.
- **Conformant codec.** An executable satisfying the codec-conformance contract
  of §6.

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHALL**, **SHOULD**, and
**MAY** are to be interpreted as in RFC 2119.

---

## 2. The five tiers

> **Normative.** Thresholds verbatim from `src/levels.rs`.

A codec is graded against an ordered ladder of tiers. Each tier sets floors and
ceilings on the global metrics and, for the lossy tiers, per-band fidelity
floors. The grade is the **highest tier the codec fully satisfies** (§5).

| Tier | Name | Global gate | Per-band reqs | Meaning |
|------|------|-------------|---------------|---------|
| **L** | Lossless | PRD **exactly 0** on the integer sample domain **AND** CR ≥ **0.8** | none | Bit-exact reconstruction. The only honest lossless claim. |
| **N** | Near-Lossless | R ≥ **0.99**, PRD ≤ **5.0 %**, CR ≥ **1.0** | none | Small error, shape preserved, not bit-exact. The strongest non-lossless tier. |
| **C** | Clinical | R ≥ **0.95**, PRD ≤ **9.0 %**, CR ≥ **20.0** | δ θ α β γ (§2.2) | Distortion small enough for clinical read. |
| **M** | Monitoring | R ≥ **0.85**, PRD ≤ **20.0 %**, CR ≥ **100.0** | δ θ α β γ (§2.2) | Usable for continuous monitoring. |
| **A** | Alerting | R ≥ **0.70**, PRD ≤ **40.0 %**, CR ≥ **200.0** | δ θ α β γ (§2.2) | Coarse, but events still detectable. |
| **—** | below floor | fails even the A gate | — | Non-compliant. |

A lossy tier passes **iff** the global R, PRD, and CR thresholds are all met
**AND** every *measured* band meets its per-band R and PRD floors. R and PRD and
CR are independent AND-gates; passing one does not relax another. `max_snr_loss`
is carried per tier (L 0.0, C 3.0, M 6.0, A 10.0 dB) for reporting parity but is
**not gated** — `snr_db` is measured and reported, never used to pass or fail.

Strictness order, strongest first: **L < C < M < A < below-floor**.

### 2.1 The L tier — exact zero, not "small PRD"

> **Normative.**

The Lossless tier is special-cased. It does **not** test a float PRD against an
epsilon. It tests **integer-domain exact equality**: the reconstructed samples
MUST equal the originals element-for-element. A codec off by a single LSB on a
single sample is graded **lossy**, not lossless. The L tier additionally requires
**CR ≥ 0.8**: a codec that *expands* the data cannot claim lossless compliance
even with a perfect reconstruction. The L tier has **no per-band requirements**;
a bit-exact reconstruction has zero error in every band by construction.

### 2.2 Per-band fidelity requirements (C / M / A)

> **Normative.** Verbatim from `src/levels.rs`. `freq_range` is documentary; the
> gate consults only `max_prd` and `min_r`.

A band requirement constrains a measured band **only when a band of that name is
present** in the measurement; unmeasured bands are not penalized.

**C — Clinical**

| band | freq_range (Hz) | max PRD | min R |
|------|-----------------|---------|-------|
| delta | 0.5 – 4.0 | 5.0 % | 0.98 |
| theta | 4.0 – 8.0 | 7.0 % | 0.97 |
| alpha | 8.0 – 13.0 | 8.0 % | 0.96 |
| beta | 13.0 – 30.0 | 12.0 % | 0.93 |
| gamma | 30.0 – 50.0 | 20.0 % | 0.85 |

**M — Monitoring**

| band | freq_range (Hz) | max PRD | min R |
|------|-----------------|---------|-------|
| delta | 0.5 – 4.0 | 10.0 % | 0.95 |
| theta | 4.0 – 8.0 | 12.0 % | 0.93 |
| alpha | 8.0 – 13.0 | 15.0 % | 0.90 |
| beta | 13.0 – 30.0 | 25.0 % | 0.80 |
| gamma | 30.0 – 50.0 | 40.0 % | 0.60 |

**A — Alerting**

| band | freq_range (Hz) | max PRD | min R |
|------|-----------------|---------|-------|
| delta | 0.5 – 4.0 | 20.0 % | 0.85 |
| theta | 4.0 – 8.0 | 25.0 % | 0.80 |
| alpha | 8.0 – 13.0 | 30.0 % | 0.75 |
| beta | 13.0 – 30.0 | 40.0 % | 0.65 |
| gamma | 30.0 – 50.0 | 60.0 % | 0.40 |

A band fails its tier if its measured R is below `min_r` **or** its measured PRD
exceeds `max_prd`. A single band failure drops the codec to the next tier.

---

## 3. Metrics

> **Normative.** One definition each, in `src/metrics.rs`.

Aggregate global metrics are computed over the **flattened** multichannel signal:
channels concatenated in order into one contiguous vector, then a single metric
over the whole stream. On a length mismatch every metric truncates to the shorter
slice.

### 3.1 PRD — Percentage Root-mean-square Difference

```
PRD = 100 · sqrt( Σ (x − x̂)²  /  Σ x² )
```

Lower is better. All-zero guard: if `Σ x²` < 1e-12 the result is `0.0` when the
residual is also ≈0, else `100.0`. For the **L tier only**, PRD is replaced by the
integer-domain exact-zero test (element-wise integer equality, `false` on any
length mismatch), not the float PRD.

### 3.2 PRDN — Normalized PRD

```
PRDN = 100 · sqrt( Σ (x − x̂)²  /  Σ (x − mean(x))² )
```

Mean-subtracted denominator so a DC offset cannot deflate the reported error.
Measured and reported; not gated.

### 3.3 Pearson R, SNR, CR, QS, Entropy

- **Pearson R** — correlation of the two flattened streams, mean-centred. If
  either standard deviation < 1e-8 the correlation is undefined; the function
  returns `1.0` when the streams are element-wise ≈equal and `0.0` otherwise.
  Higher is better.
- **SNR (dB)** — `10·log10( mean(x²) / mean((x − x̂)²) )`, capped at **120.0 dB**
  when noise power < 1e-30. Reported, not gated.
- **CR — compression ratio** — `raw_bytes / max(comp_bytes, 1)`, where
  `raw_bytes` is the signal size under the reference container (§6.4) so the ratio
  is comparable across codecs and the `store` baseline reports CR ≈ 1.0. Across a
  corpus, CR is **pooled by bytes** (`Σ raw / Σ comp`), not averaged.
- **QS — quality score** — `CR / PRD`; when PRD ≤ 0 (lossless) QS is the raw CR.
- **Shannon entropy (bits)** — `H = −Σ pᵢ log₂ pᵢ` from a symbol histogram; a
  utility metric, not gated.

### 3.4 Per-band fidelity — same filter, both signals

> **Normative.**

For each clinical band, a **rectangular frequency-domain mask** keeps the DFT
bins whose centre frequency falls in the half-open band `[lo, hi)`, applied
**identically to the original and the reconstruction**, then inverse-transformed;
R, PRD, SNR are measured on the masked band signals. Because both signals are
filtered the same way, per-band metrics reflect **codec error inside that band**,
never a mismatch between two differently-filtered signals.

The measurement band edges (`CLINICAL_BANDS`, inclusive-low / exclusive-high Hz)
are: sub-delta 0–1, delta 1–4, theta 4–8, alpha 8–12, beta 13–30, gamma 30–100.
The grade pairs a measured band with its tier requirement **by name**: `delta`,
`theta`, `alpha`, `beta`, `gamma` are measured and gated; `sub-delta` is measured
and reported but has no tier requirement. (The measurement and tier-requirement
tables are two tables; the names are kept identical so the by-name pairing works.
The tier table's documentary `freq_range` uses the 0.5–4 / 8–13 conventions.)

---

## 4. Verify, don't trust

> **Normative.**

A codec's lossless claim is **graded, not assumed**. A grader MUST verify the
claim by comparing the reconstruction to the original on the integer domain. A
codec that *declares* lossless but reconstructs a single sample off by one LSB
MUST be reported `bit_exact = false` and graded as lossy. A decode that returns
the wrong length, wrong values, or nothing on a malformed blob MUST surface to the
L-tier gate as a mismatch and fail the lossless claim — never as a grader crash.
This "verify the claim, do not trust it" principle is the integrity guarantee of
the standard.

---

## 5. Grade dispatch

> **Normative.** Logic in `src/harness.rs` + `src/levels.rs`.

1. **Lossless gate (short-circuit).** Flatten original and reconstruction to
   integer streams; test exact integer equality. If **bit-exact AND CR ≥ 0.8**,
   grade **L** and skip the lossy battery (PRD = 0, R = 1, SNR = 120, per-band
   perfect-by-construction).
2. **Lossy battery — C → M → A descent.** Otherwise convert to `f64`, compute the
   global metrics over the flattened signal, build the per-band table, and grade.
   The first (highest) tier that fully passes wins.
3. **Below floor.** If no lossy tier passes, the grade is the below-floor
   sentinel (rendered `—`).

When a codec settles on a tier, the report's `violations` field carries the
reasons the *strictest tier it attempted* failed — a precise to-do list for
climbing a tier. **Corpus runs** grade each file, pool CR by bytes, average PRD
and R, and report the **worst** (lowest) grade observed: a codec's corpus
compliance is bounded by its weakest file. The **leaderboard** ranks codecs
best-first — grade first, then QS descending, then CR descending, then codec name.

---

## 6. Codec-conformance contract — the file-based CLI

> **Normative.** This is how ANY codec, in ANY language, becomes gradable.

A **conformant codec** is an executable (binary or script) that exposes two
subcommands over files. The reference grader drives it via a codec manifest (§7);
no source-level integration and no specific language are required.

### 6.1 Invocation

```
<cmd> [prefix_args…] encode <in_path>  <out_path>
<cmd> [prefix_args…] decode <in_path>  <out_path> --channels <N> --samples <M> --rate <FS> --dtype <DT>
```

- **`encode`** MUST read the signal from `<in_path>`, compress it, and write the
  opaque compressed blob to `<out_path>`. It MUST exit `0` on success.
- **`decode`** MUST read the blob from `<in_path>`, reconstruct the signal, and
  write the samples to `<out_path>`. It MUST exit `0` on success.
- `prefix_args` are fixed tokens declared in the manifest (e.g. the script path,
  a model checkpoint) inserted before the subcommand.
- A codec MAY use a **custom argument template** declared in the manifest if its
  flag style differs; the default template is the invocation above.

### 6.2 Input and output formats

- **Encode input (`in_path`).** Default `edf`: a minimal, spec-valid EDF file the
  grader writes (digital int16 samples, one data record, uniform rate). A codec
  MAY instead request `lqs0` (the reference container of §6.4) via the manifest.
- **Decode output (`out_path`).** Default `raw`: a flat, **channel-major,
  little-endian** stream of exactly `N × M` integers, each of width `DT`
  (`i16` | `i32` | `i64`). The grader validates
  `byte_length == N × M × width(DT)` and reshapes by channel. A codec MAY instead
  emit `lqs0` via the manifest, in which case it carries its own shape and the
  `--channels/--samples` flags are advisory.

`N` (channel count), `M` (samples per channel), `FS` (rate), and `DT` (dtype) are
supplied by the grader both as the CLI flags above **and** as environment
variables `ECS_CHANNELS`, `ECS_SAMPLES`, `ECS_RATE`, `ECS_DTYPE` (a codec MAY read
either). The grader also sets `ECS_WORKDIR` to the per-invocation scratch dir.
This spec fixes the default to **equal-length channels** for the EDF input path
(EDF's single-rate requirement); ragged/empty channels are only round-trippable
through the `lqs0` format.

### 6.3 Failure semantics

> **Normative.**

On **any** failure — non-zero exit, a missing or short `out_path`, a byte length
that disagrees with the declared shape, a spawn or I/O error, or exceeding the
manifest `timeout_secs` — the grader MUST treat the round trip as failed and grade
accordingly (a failed lossless claim / below-floor as the metrics dictate). A
grader MUST NOT crash on codec failure. `declared_lossless` is a **manifest
field** (the codec author's claim); it is conveyed to the grader, never to the
codec, and is always verified per §4.

### 6.4 Reference container (`lqs0`)

> **Normative** for codecs that select the `lqs0` format; otherwise Informative.

A tiny, deterministic, byte-exact container (all integers little-endian):

```text
magic   : 4 bytes  = b"ECS0"
n_chan  : u32       = number of channels
per chan: u32 len, then `len` × i64 samples
```

`fs` is deliberately not in this stream — it is metadata the grader holds. The
reference adapters (`store`, `gzip`, optional `zstd`) use this container; a codec
is not required to.

---

## 7. Codec manifest

> **Normative.** Schema mirrored in `SPEC/schemas/codec-manifest-v1.json`.

A codec manifest (TOML) tells the grader how to invoke a conformant codec.

```toml
spec_version = "1.0"

[codec]
name = "gzip-ext"            # report identifier
declared_lossless = true     # the claim — verified, not trusted
cmd = "python3"              # binary/script; resolved path → $ECS_CODEC_<NAME>_BIN → PATH
prefix_args = ["scripts/gzip_codec.py"]
sample_dtype = "i32"         # i16 | i32 | i64  (the decode `raw` width)
input_format = "edf"         # edf (default) | lqs0
output_format = "raw"        # raw (default) | lqs0
timeout_secs = 600

# Optional explicit templates; omit to use the default contract of §6.1.
# Placeholders: {input} {output} {channels} {samples} {rate} {dtype}
encode_args = ["encode", "{input}", "{output}"]
decode_args = ["decode", "{input}", "{output}",
               "--channels", "{channels}", "--samples", "{samples}",
               "--rate", "{rate}", "--dtype", "{dtype}"]

[codec.env]                  # extra env merged over the ECS_* variables
PYTHONUNBUFFERED = "1"
```

Required fields: `[codec].name`, `cmd`, `declared_lossless`. All others have the
defaults shown. A grader MUST refuse a manifest whose `spec_version` major it does
not implement (§10).

---

## 8. Reference corpus and manifests

> **Normative.** Schema mirrored in `SPEC/schemas/corpus-manifest-v1.json`.

Cross-lab comparability REQUIRES a frozen, hash-pinned corpus. A corpus manifest
names a corpus, its version, and each file with a pinned SHA-256, sample rate, and
shape. A grader MUST verify each file's SHA-256 against the manifest before
grading and MUST refuse (integrity error) on any mismatch.

```toml
spec_version = "1.0"
name = "ecs-smoke"
version = "1.0.0"

[[file]]
path = "smoke/synthetic_a.edf"   # relative to the manifest's directory
sha256 = "…64 hex…"
fs = 256.0
n_chan = 4
n_samples = 1024
```

A small, redistributable **smoke** corpus ships in-repo (`lqs/corpora/`) so the
standard is runnable offline with zero external data. Full public corpora
(CHB-MIT, TUEG, sleep-EDF, …) are **not** redistributed; a lab pins them locally
with `openecs emit-corpus-manifest` and grades against the resulting manifest.
A submission MUST name the corpus manifest (`name` + `version`) it was graded
against so two submissions are only compared when graded on the same corpus.

---

## 9. Results submission

> **Normative.** Schemas in `SPEC/schemas/ecs-report-v1.json` and
> `SPEC/schemas/ecs-submission-v1.json`.

The grader emits a self-describing **submission** — the wire format two labs
exchange. It is a stable-key JSON envelope carrying the spec version, the codec
identity (name + manifest SHA-256), the corpus identity (name + version), one
`EcsReport` per file, the corpus summary, the leaderboard ordering, and an
**optional** `task_concordance` block (§10). Every `EcsReport` carries a
`spec_version` field so it is self-identifying out of context.

A conforming submission MUST validate against `ecs-submission-v1.json`. A grader
MUST stamp `spec_version = "1.0"` on every report and on the envelope.

---

## 10. Optional task-concordance axis

> **Informative**, with a **Normative** reporting rule.

Signal fidelity is necessary but not always sufficient: a lab MAY also ask whether
compression preserves a *downstream task* (e.g. seizure detection, BCI decoding).
OpenECS v1.0 defines an **optional, codec-agnostic** task-concordance axis: a separate
tool runs a reference detector on the original and on the reconstruction and
reports the degradation (e.g. seizure-F1 delta, Hjorth-parameter concordance).

**Normative reporting rule:** task concordance is **advisory only**. It is carried
in the submission's `task_concordance` block and reported separately; it MUST NOT
alter a codec's L / C / M / A grade. A codec's tier in v1.0 is a function of signal
fidelity and resource cost alone. The vocabulary for task requirements
(`TaskRequirement`: task, metric, max degradation) is reserved and informative in
v1.0; folding it into the tier gates is deferred to a future major version.

---

## 11. Version and stability policy

> **Normative.**

OpenECS uses SemVer-style versioning on the spec, independent of the crate version.

- **v1.x (minor / additive).** New optional report or manifest fields (with
  defaults), new optional formats, clarifications. Backward compatible: a v1.0
  grader reads a v1.1 submission's known fields and ignores unknown ones.
- **v2.0 (major / breaking).** Any change to a **tier threshold**, a **metric
  formula**, the **L-tier exact-zero rule**, or the codec-conformance contract.

Every submission and report carries `spec_version`. A grader **MUST** refuse a
submission whose **major** version it does not implement, and **SHOULD** accept a
higher **minor** version of its own major. The canonical thresholds live in
`src/levels.rs`; this document and any mirror implementation track them, and a
divergence is a spec bug to be reconciled in favour of `src/levels.rs`.

---

## 12. Conformance checklist

> **Informative.**

A codec is **OpenECS-v1.0 conformant** if:

1. It ships an executable satisfying the §6 file-based CLI contract.
2. A codec manifest (§7) describes how to invoke it.
3. Driven by a conforming grader over a hash-pinned corpus (§8), it produces a
   submission (§9) that validates against the schema.
4. Its declared lossless claim, if any, survives the §4 verification.

A worked end-to-end example — wrapping `gzip` as an external subprocess codec
through a manifest and grading it to **L** over the smoke corpus, writing no Rust
for the codec — is the integration test `tests/external_adapter.rs` in this crate.

---

## Appendix A — relationship to the reference implementation

> **Informative.**

The Rust crate beside this file is the **reference grader** and the **canonical**
source of thresholds. The generic file-based codec contract of §6 is implemented
by `src/adapters_external.rs`; the manifest loaders by `src/manifest.rs` and
`src/corpus.rs`; the dispatch by `src/harness.rs`; the tier table and gate by
`src/levels.rs`.

The reference grader recovers the decode shape statelessly via an **adapter-private
envelope**: `encode` returns `b"ECSX" + n_chan:u32 + n_samples:u32 + dtype:u8 +
<codec blob>`, which the grader treats as opaque and feeds back to `decode`, where
the header is stripped to recover `(N, M, dtype)`. This is an implementation detail
of one grader, **not** part of the normative codec contract (§6) — the codec never
sees the envelope, and a grader that holds the original shape by other means need
not use it.

A frozen **Python mirror** of the tier tables and metrics
(`lamquant_codec/lqs.py`) lets a Python pipeline grade against OpenECS without a Rust
dependency; it is kept in parity with `src/levels.rs` (enforced by
`tests/python_parity.rs` and `tests/metrics_parity.rs`) and stamps the same
`ECS_SPEC_VERSION`.
