//! Known-answer vectors for the corpus content identity
//! (SPEC/OpenECS-v1.1.md §8.2), frozen in `tests/golden/content_identity_kat.json`
//! by the independent stdlib-Python construction in
//! `tools/content_identity_kat.py`.
//!
//! The test recomputes the exact **pre-hash byte strings** from the structured
//! inputs and compares them hex-for-hex before comparing digests, so a
//! disagreement names the field that differs rather than only "the hash moved".

use std::path::PathBuf;

use open_eeg_codec_standard::corpus::{
    content_digest, content_preimage, exact_rational, hex_lower, payload_digest, payload_preimage,
    ContentIdentity, CorpusFileEntry, CorpusManifest, Digest32, CONTENT_IDENTITY_VERSION,
};

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/content_identity_kat.json")
}

fn golden() -> serde_json::Value {
    let text = std::fs::read_to_string(golden_path()).expect("read golden");
    serde_json::from_str(&text).expect("golden is JSON")
}

fn channels_of(file: &serde_json::Value) -> Vec<Vec<i64>> {
    file["channels"]
        .as_array()
        .expect("channels")
        .iter()
        .map(|channel| {
            channel
                .as_array()
                .expect("channel")
                .iter()
                .map(|sample| sample.as_i64().expect("i64 sample"))
                .collect()
        })
        .collect()
}

fn digest_from_hex(hex: &str) -> Digest32 {
    assert_eq!(hex.len(), 64);
    let mut out = [0_u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("hex byte");
    }
    out
}

#[test]
fn golden_names_the_construction_it_freezes() {
    let g = golden();
    assert_eq!(g["schema"], "openecs.content-identity-kat/v1");
    assert_eq!(
        g["content_identity_version"].as_u64(),
        Some(u64::from(CONTENT_IDENTITY_VERSION))
    );
    assert_eq!(g["file"].as_array().map(Vec::len), Some(3));
}

#[test]
fn payload_preimages_and_digests_match_the_vectors() {
    let g = golden();
    for file in g["file"].as_array().expect("file array") {
        let channels = channels_of(file);
        let path = file["path"].as_str().expect("path");
        let preimage = payload_preimage(&channels).expect("dense");
        assert_eq!(
            hex_lower(&preimage),
            file["payload_preimage_hex"].as_str().expect("hex"),
            "{path}: payload pre-image bytes"
        );
        assert_eq!(
            hex_lower(&payload_digest(&channels).expect("digest")),
            file["payload_digest"].as_str().expect("digest"),
            "{path}: payload digest"
        );
        assert_eq!(channels.len() as u64, file["n_chan"].as_u64().unwrap());
        assert_eq!(
            channels[0].len() as u64,
            file["n_samples"].as_u64().unwrap()
        );
    }
}

#[test]
fn exact_rationals_match_the_vectors() {
    let g = golden();
    for file in g["file"].as_array().expect("file array") {
        let fs = file["fs"].as_f64().expect("fs");
        assert_eq!(
            exact_rational(fs).expect("rational"),
            (
                file["fs_num"].as_u64().expect("fs_num"),
                file["fs_den"].as_u64().expect("fs_den")
            ),
            "fs {fs}"
        );
    }
}

#[test]
fn content_preimage_and_digest_match_the_vectors() {
    let g = golden();
    let files = g["file"].as_array().expect("file array");
    // Manifest order is the golden's order — deliberately NOT sorted by
    // path, so the sort inside the construction is exercised.
    let entries: Vec<CorpusFileEntry> = files
        .iter()
        .map(|file| CorpusFileEntry {
            path: file["path"].as_str().unwrap().to_string(),
            sha256: "0".repeat(64), // not part of the identity
            fs: file["fs"].as_f64().unwrap(),
            fs_num: Some(file["fs_num"].as_u64().unwrap()),
            fs_den: Some(file["fs_den"].as_u64().unwrap()),
            n_chan: file["n_chan"].as_u64().unwrap() as usize,
            n_samples: file["n_samples"].as_u64().unwrap() as usize,
        })
        .collect();
    let manifest = CorpusManifest {
        spec_version: "1.1".to_string(),
        name: g["name"].as_str().unwrap().to_string(),
        version: g["version"].as_str().unwrap().to_string(),
        content_identity: None,
        abir_identity: None,
        file: entries,
    };
    let payload_digests: Vec<Digest32> = files
        .iter()
        .map(|file| digest_from_hex(file["payload_digest"].as_str().unwrap()))
        .collect();

    let preimage = content_preimage(&manifest, &payload_digests).expect("preimage");
    assert_eq!(
        hex_lower(&preimage),
        g["content_preimage_hex"].as_str().unwrap(),
        "corpus pre-image bytes"
    );
    let expected = g["content_digest"].as_str().unwrap();
    assert_eq!(
        hex_lower(&content_digest(&manifest, &payload_digests).expect("digest")),
        expected
    );
    assert_eq!(
        ContentIdentity::compute(&manifest, &payload_digests).expect("identity"),
        ContentIdentity {
            version: CONTENT_IDENTITY_VERSION,
            digest: expected.to_string()
        }
    );

    // Derived from the decoded samples end to end, with the rational
    // derived from `fs` rather than declared: the same identity.
    let mut derived = manifest.clone();
    for entry in &mut derived.file {
        entry.fs_num = None;
        entry.fs_den = None;
    }
    let loaded: Vec<(Vec<Vec<i64>>, f64)> = files
        .iter()
        .map(|file| (channels_of(file), file["fs"].as_f64().unwrap()))
        .collect();
    assert_eq!(
        derived
            .content_identity_of(&loaded)
            .expect("identity")
            .digest,
        expected
    );
}
