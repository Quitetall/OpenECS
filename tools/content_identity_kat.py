#!/usr/bin/env python3
"""Reference implementation + known-answer vectors for the OpenECS corpus
content identity (SPEC/OpenECS-v1.1.md §8.2).

Standard library only, so an implementer in any language has a second,
independent construction to compare against — the Rust crate is the reference
grader; this file is the cross-check. The vectors it emits are frozen in
`tests/golden/content_identity_kat.json` and re-derived by
`tests/content_identity_kat.rs`, which compares the exact pre-hash byte strings
(not just the digests) so a disagreement names the field that differs.

    python3 tools/content_identity_kat.py                 # print the vectors
    python3 tools/content_identity_kat.py --write PATH    # regenerate the golden
    python3 tools/content_identity_kat.py --check PATH    # recompute + compare

The byte layout, verbatim from the spec:

    payload_digest = SHA-256(
        "OpenECS-payload\\0",
        u8 element_type = 1,          # i64
        u8 byte_order   = 0,          # little-endian
        u8 layout       = 0,          # channel-major
        u32le n_chan, u64le n_samples,
        samples as i64le, channel 0 first )

    content_digest = SHA-256(
        "OpenECS-corpus-identity\\0",
        u32le version = 1,
        lp(name), lp(version), u32le file_count,
        for each file sorted byte-wise on path:
            lp(path), u32le n_chan, u64le n_samples,
            u64le fs_num, u64le fs_den, payload_digest )

    lp(s) = u32le byte length, then the UTF-8 bytes.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import struct
import sys
from fractions import Fraction

PAYLOAD_DOMAIN = b"OpenECS-payload\0"
CONTENT_DOMAIN = b"OpenECS-corpus-identity\0"
CONTENT_IDENTITY_VERSION = 1
ELEMENT_TYPE_I64 = 1
BYTE_ORDER_LITTLE = 0
LAYOUT_CHANNEL_MAJOR = 0


def lp(s: str) -> bytes:
    raw = s.encode("utf-8")
    return struct.pack("<I", len(raw)) + raw


def exact_rational(fs: float) -> tuple[int, int]:
    """The exact value of a double as a rational in lowest terms (§8.2.3)."""
    frac = Fraction(fs)  # exact for any finite double
    if frac <= 0:
        raise ValueError(f"sample rate must be positive, got {fs}")
    if frac.numerator >= 1 << 64 or frac.denominator >= 1 << 64:
        raise ValueError(f"sample rate {fs} does not fit u64/u64")
    return frac.numerator, frac.denominator


def payload_preimage(channels: list[list[int]]) -> bytes:
    n_chan = len(channels)
    n_samples = len(channels[0]) if channels else 0
    if any(len(c) != n_samples for c in channels):
        raise ValueError("ragged signal")
    out = bytearray(PAYLOAD_DOMAIN)
    out += bytes([ELEMENT_TYPE_I64, BYTE_ORDER_LITTLE, LAYOUT_CHANNEL_MAJOR])
    out += struct.pack("<IQ", n_chan, n_samples)
    for channel in channels:
        for sample in channel:
            out += struct.pack("<q", sample)
    return bytes(out)


def payload_digest(channels: list[list[int]]) -> bytes:
    return hashlib.sha256(payload_preimage(channels)).digest()


def content_preimage(name: str, version: str, files: list[dict]) -> bytes:
    """`files`: dicts with path, n_chan, n_samples, fs_num, fs_den, payload_digest (bytes)."""
    ordered = sorted(files, key=lambda f: f["path"].encode("utf-8"))
    paths = [f["path"] for f in ordered]
    if len(set(paths)) != len(paths):
        raise ValueError("a path is pinned twice")
    out = bytearray(CONTENT_DOMAIN)
    out += struct.pack("<I", CONTENT_IDENTITY_VERSION)
    out += lp(name) + lp(version)
    out += struct.pack("<I", len(ordered))
    for f in ordered:
        if f["n_chan"] == 0 or f["n_samples"] == 0:
            raise ValueError(f"{f['path']} has an empty shape")
        out += lp(f["path"])
        out += struct.pack("<IQQQ", f["n_chan"], f["n_samples"], f["fs_num"], f["fs_den"])
        out += f["payload_digest"]
    return bytes(out)


def content_digest(name: str, version: str, files: list[dict]) -> bytes:
    return hashlib.sha256(content_preimage(name, version, files)).digest()


# ── the known-answer corpus ──────────────────────────────────────────────────
#
# Small enough to hex-dump, shaped to exercise every field: two files whose
# manifest order is NOT their byte-wise path order ("b.edf" before "a/…"
# because '/' (0x2F) sorts before 'b' (0x62) — and "a/x.edf" before "b.edf"),
# a non-integer rate (250.5 = 501/2), a rate that is not dyadic in decimal
# (0.1 → its exact double), signed extremes among the samples, and a
# multi-byte UTF-8 name.

KAT_MANIFEST = {
    "name": "kat-corpus-é",
    "version": "1.2.3",
    "files": [
        {
            "path": "b.edf",
            "fs": 250.5,
            "channels": [[1, -1, 0], [9223372036854775807, -9223372036854775808, 7]],
        },
        {
            "path": "a/x.edf",
            "fs": 0.1,
            "channels": [[42]],
        },
        {
            "path": "a/y.edf",
            "fs": 256.0,
            "channels": [[0, 0], [0, 0], [0, 0], [1, 2]],
        },
    ],
}


def build_vectors() -> dict:
    files_out = []
    identity_files = []
    for f in KAT_MANIFEST["files"]:
        fs_num, fs_den = exact_rational(f["fs"])
        pre = payload_preimage(f["channels"])
        dig = hashlib.sha256(pre).digest()
        files_out.append(
            {
                "path": f["path"],
                "fs": f["fs"],
                "fs_num": fs_num,
                "fs_den": fs_den,
                "n_chan": len(f["channels"]),
                "n_samples": len(f["channels"][0]),
                "channels": f["channels"],
                "payload_preimage_hex": pre.hex(),
                "payload_digest": dig.hex(),
            }
        )
        identity_files.append(
            {
                "path": f["path"],
                "n_chan": len(f["channels"]),
                "n_samples": len(f["channels"][0]),
                "fs_num": fs_num,
                "fs_den": fs_den,
                "payload_digest": dig,
            }
        )
    pre = content_preimage(KAT_MANIFEST["name"], KAT_MANIFEST["version"], identity_files)
    return {
        "schema": "openecs.content-identity-kat/v1",
        "spec": "SPEC/OpenECS-v1.1.md §8.2",
        "generator": "tools/content_identity_kat.py",
        "content_identity_version": CONTENT_IDENTITY_VERSION,
        "name": KAT_MANIFEST["name"],
        "version": KAT_MANIFEST["version"],
        "file": files_out,
        "content_preimage_hex": pre.hex(),
        "content_digest": hashlib.sha256(pre).hexdigest(),
    }


def render(vectors: dict) -> str:
    return json.dumps(vectors, indent=2, ensure_ascii=False) + "\n"


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--write", metavar="PATH", help="write the vectors to PATH")
    ap.add_argument("--check", metavar="PATH", help="recompute and compare against PATH")
    args = ap.parse_args(argv)
    vectors = build_vectors()
    if args.check:
        with open(args.check, encoding="utf-8") as fh:
            frozen = json.load(fh)
        if frozen != vectors:
            print(f"{args.check}: vectors differ from this implementation", file=sys.stderr)
            return 1
        print(f"{args.check}: {len(vectors['file'])} payload vectors + 1 corpus vector agree")
        return 0
    if args.write:
        with open(args.write, "w", encoding="utf-8") as fh:
            fh.write(render(vectors))
        print(f"wrote {args.write}")
        return 0
    sys.stdout.write(render(vectors))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
