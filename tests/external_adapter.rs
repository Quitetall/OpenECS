//! Integration test: the universal plug-in claim.
//!
//! Wrap an OFF-THE-SHELF compressor as an external subprocess codec via the
//! file-based CLI contract — writing NO Rust for the codec — and grade it to
//! ECS-L. This is the load-bearing proof that "any codec, any language" can
//! be benchmarked. Tests gate on the availability of `sh` / `gzip` /
//! `python3` and skip cleanly so CI is green on a minimal host.

use std::path::{Path, PathBuf};
use std::process::Command;

use open_eeg_codec_standard::adapter::Codec;
use open_eeg_codec_standard::adapters_external::{ExternalCodec, InputFormat, OutputFormat};
use open_eeg_codec_standard::harness;
use open_eeg_codec_standard::manifest::load_codec_manifest;
use open_eeg_codec_standard::subprocess::{SampleDtype, ScratchDir};

/// True iff `sh -c "<probe>"` exits 0 (the tool is usable on this host).
fn have(probe: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(probe)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A deterministic, EEG-shaped, i16-range, rectangular signal.
fn signal() -> Vec<Vec<i64>> {
    use std::f64::consts::PI;
    let fs = 256.0;
    let n = 1024;
    (0..4)
        .map(|c| {
            let amp = 1.0 + 0.25 * c as f64;
            (0..n)
                .map(|i| {
                    let t = i as f64 / fs;
                    let v = amp
                        * (90.0 * (2.0 * PI * 3.0 * t).sin()
                            + 55.0 * (2.0 * PI * 10.0 * t).sin()
                            + 25.0 * (2.0 * PI * 22.0 * t).sin());
                    v.round() as i64
                })
                .collect()
        })
        .collect()
}

/// Write `body` as an executable-by-`sh` script in `dir`, return its path.
fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).expect("write codec script");
    p
}

/// A gzip codec over the ECS0 container: encode gzips the input, decode
/// gunzips it. Pure byte transform — the adapter handles (de)serialization,
/// so the codec needs no EDF or shape logic. Ignores the trailing
/// `--channels …` decode flags ($1=subcmd, $2=in, $3=out).
const GZIP_ECS0_SH: &str = r#"case "$1" in
  encode) gzip -c "$2" > "$3" ;;
  decode) gunzip -c "$2" > "$3" ;;
  *) exit 2 ;;
esac
"#;

#[test]
fn gzip_subprocess_via_builder_grades_l() {
    if !have("gzip --version") {
        eprintln!("SKIP gzip_subprocess_via_builder_grades_l: no `gzip`/`sh` on this host");
        return;
    }
    let dir = ScratchDir::new("ext_test_builder").expect("scratch");
    let script = write_script(&dir.path, "gzip_codec.sh", GZIP_ECS0_SH);

    // cmd = `sh`, prefix = [script]: `sh <script> encode <in> <out>`. lqs0
    // in AND out, so the codec is a pure gzip of the reference container.
    let codec = ExternalCodec::new("gzip-ext", "sh")
        .with_prefix_args(vec![script.to_string_lossy().into_owned()])
        .with_declared_lossless(true)
        .with_formats(InputFormat::Ecs0, OutputFormat::Ecs0);

    let sig = signal();

    // Direct round trip is bit-exact.
    let blob = codec.encode(&sig, 256.0);
    assert!(!blob.is_empty(), "external gzip codec produced no blob");
    assert_eq!(codec.decode(&blob), sig, "external gzip codec round trip");

    // The harness grades it ECS-L.
    let rep = harness::run(&codec, &sig, 256.0);
    assert!(rep.bit_exact, "external gzip codec must be bit-exact");
    assert_eq!(rep.grade, 'L', "bit-exact codec grades ECS-L");
    assert_eq!(rep.prd, 0.0);
    assert_eq!(rep.r, 1.0);
}

#[test]
fn gzip_subprocess_via_manifest_grades_l() {
    if !have("gzip --version") {
        eprintln!("SKIP gzip_subprocess_via_manifest_grades_l: no `gzip`/`sh` on this host");
        return;
    }
    let dir = ScratchDir::new("ext_test_manifest").expect("scratch");
    let script = write_script(&dir.path, "gzip_codec.sh", GZIP_ECS0_SH);

    // The full "no Rust" path: a TOML manifest describes the codec; the
    // grader loads it, builds the adapter, and grades — no codec-specific
    // Rust anywhere.
    let manifest_text = format!(
        r#"
spec_version = "1.0"
[codec]
name = "gzip-ext"
cmd = "sh"
declared_lossless = true
prefix_args = ["{}"]
input_format = "ecs0"
output_format = "ecs0"
"#,
        script.to_string_lossy()
    );
    let manifest_path = dir.join("codec.toml");
    std::fs::write(&manifest_path, manifest_text).expect("write manifest");

    let manifest = load_codec_manifest(&manifest_path).expect("manifest loads");
    let codec = manifest.into_adapter().expect("adapter resolves (`sh` on PATH)");

    let sig = signal();
    let rep = harness::run(&codec, &sig, 256.0);
    assert!(rep.bit_exact, "manifest-driven gzip codec must be bit-exact");
    assert_eq!(rep.grade, 'L');
    assert_eq!(codec.name(), "gzip-ext");
}

/// A Python store codec exercising the RAW decode path end-to-end through a
/// subprocess: encode copies the ECS0 input to the blob; decode parses ECS0
/// and emits a channel-major int32 stream, which the adapter reshapes via
/// the envelope-recorded shape. This drives the envelope + reshape machinery
/// over a real subprocess, not just the unit tests.
const STORE_RAW_PY: &str = r#"import sys, struct
def read_lqs0(path):
    b = open(path, "rb").read()
    assert b[:4] == b"ECS0", "bad magic"
    (n,) = struct.unpack_from("<I", b, 4)
    pos = 8
    chans = []
    for _ in range(n):
        (ln,) = struct.unpack_from("<I", b, pos); pos += 4
        chans.append(list(struct.unpack_from("<%dq" % ln, b, pos))); pos += 8 * ln
    return chans
cmd = sys.argv[1]
if cmd == "encode":
    open(sys.argv[3], "wb").write(open(sys.argv[2], "rb").read())  # store
elif cmd == "decode":
    chans = read_lqs0(sys.argv[2])
    out = bytearray()
    for ch in chans:
        for s in ch:
            out += struct.pack("<i", s)   # int32 LE, channel-major
    open(sys.argv[3], "wb").write(out)
else:
    sys.exit(2)
"#;

#[test]
fn raw_output_python_codec_grades_l() {
    if !have("python3 --version") {
        eprintln!("SKIP raw_output_python_codec_grades_l: no `python3` on this host");
        return;
    }
    let dir = ScratchDir::new("ext_test_raw").expect("scratch");
    let script = write_script(&dir.path, "store_raw.py", STORE_RAW_PY);

    // input lqs0 (so the codec needs no EDF parser), output raw int32 — the
    // adapter reshapes the flat stream via the envelope-recorded shape.
    let codec = ExternalCodec::new("store-raw-py", "python3")
        .with_prefix_args(vec![script.to_string_lossy().into_owned()])
        .with_declared_lossless(true)
        .with_sample_dtype(SampleDtype::I32)
        .with_formats(InputFormat::Ecs0, OutputFormat::Raw);

    let sig = signal();
    let blob = codec.encode(&sig, 256.0);
    assert!(!blob.is_empty(), "python codec produced no blob");
    assert_eq!(codec.decode(&blob), sig, "raw-path round trip");

    let rep = harness::run(&codec, &sig, 256.0);
    assert!(rep.bit_exact, "raw-output python codec must be bit-exact");
    assert_eq!(rep.grade, 'L');
}
