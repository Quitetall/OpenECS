# OpenECS corpora

Cross-lab comparability requires a **frozen, hash-pinned** corpus: every file
is verified against a pinned SHA-256 before grading, so two labs that grade the
same codec on the same corpus manifest get directly comparable numbers
(`SPEC/OpenECS-v1.0.md` §8).

## The smoke corpus (ships in-repo)

`ecs-smoke.toml` + `smoke/*.edf` is a tiny, **synthetic, redistributable** EEG
corpus so OpenECS runs offline with zero external data. It is **not** patient data —
it is a deterministic sum of band-placed sinusoids in the EDF i16 digital
domain, regenerable byte-for-byte:

```bash
cargo run --example gen_smoke_corpus      # rewrites smoke/*.edf + prints the [[file]] blocks
```

Verify and grade against it:

```bash
cargo run --bin openecs -- verify-corpus --corpus-manifest corpora/ecs-smoke.toml
cargo run --bin openecs -- grade --codec gzip --corpus-manifest corpora/ecs-smoke.toml --out /tmp/sub.json
```

## Full public corpora (pull locally, pin yourself)

Real corpora are **not redistributed** here (CHB-MIT, TUH/TUEG, Sleep-EDF, Siena
… carry their own licences). Download them, then pin a manifest from whatever
subset you hold — the hashes make your run reproducible without shipping the
data:

```bash
# Walk a directory of .edf files, hash each, emit a manifest on stdout.
cargo run --bin openecs -- emit-corpus-manifest \
    --root /mnt/4tb/data/Archive/edf/chbmit --name chbmit --version 1.0.0 \
    > corpora/ecs-chbmit.toml
```

The Eagle benchmark drivers (`tools/bench_chbmit.py`, `tools/bench_tueg_subsets.py`)
document where each corpus lives on disk and how to obtain it. Generated
full-corpus manifests contain machine-local paths and are **not** committed.
