//! Generic external-codec adapter — grade ANY conformant codec, in ANY
//! language, with no Rust.
//!
//! Where a vendor adapter hard-wires a specific codec binary, this module
//! drives an *arbitrary* codec through the file-based CLI contract of
//! `SPEC/OpenECS-v1.0.md` §6: a codec is any executable exposing
//!
//! ```text
//! <cmd> [prefix…] encode <in_path>  <out_path>
//! <cmd> [prefix…] decode <in_path>  <out_path> --channels N --samples M --rate FS --dtype DT
//! ```
//!
//! The codec is described by a [`crate::manifest::CodecManifest`] (TOML);
//! [`ExternalCodec`] is the runtime form. Because it implements the
//! standard [`Codec`] trait, the harness grades it identically to a
//! built-in adapter.
//!
//! ## The shape problem and the envelope
//!
//! The [`Codec`] trait's `decode(&[u8]) -> Vec<Vec<i64>>` receives only the
//! opaque blob — it is not handed the original shape. A generic codec also
//! cannot be assumed to expose an `info` subcommand. The adapter therefore
//! recovers the shape **statelessly** by prepending a tiny private envelope
//! to the blob it returns from [`encode`]:
//!
//! ```text
//! magic     : 4 bytes = b"ECSX"
//! n_chan    : u32 LE
//! n_samples : u32 LE   (per-channel sample count; 0 only valid for ecs0 output)
//! dtype     : u8       (0=i16, 1=i32, 2=i64)
//! fs        : f64 LE   (sample rate, metadata for the codec's --rate)
//! payload   : the codec's own opaque bytes
//! ```
//!
//! The harness treats the whole thing as opaque and feeds it straight back
//! to [`decode`], where the header is stripped to recover `(N, M, dtype,
//! fs)`. The codec never sees the envelope; it is an implementation detail
//! of this grader, **not** part of the normative codec contract.
//!
//! ## Failure semantics
//!
//! Any failure — unsupported signal shape, spawn error, non-zero exit, a
//! missing / short / wrong-length output, or a timeout — makes [`encode`]
//! return an empty blob and [`decode`] return an empty signal, so the
//! harness's L-tier gate reports a failed claim rather than panicking
//! (matching the vendor adapter contract).
//!
//! [`encode`]: Codec::encode
//! [`decode`]: Codec::decode

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::adapter::{deserialize, serialize, Codec};
use crate::subprocess::{reshape_channel_major, write_edf_bytes, SampleDtype, ScratchDir};

/// Magic prefix of the adapter-private envelope (see module docs).
const ENVELOPE_MAGIC: &[u8; 4] = b"ECSX";
/// Envelope header length: magic(4) + n_chan(4) + n_samples(4) + dtype(1) + fs(8).
const ENVELOPE_HEADER_LEN: usize = 21;
/// Default per-invocation timeout when the manifest does not set one.
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;

/// Format of the file the grader hands the codec on `encode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputFormat {
    /// A minimal, spec-valid EDF file (the default; digital i16 samples).
    #[default]
    Edf,
    /// The ECS0 reference container (ragged / empty channels survive).
    Ecs0,
}

/// Format of the file the codec writes on `decode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// Flat, channel-major, little-endian integers of width `sample_dtype`
    /// (the default).
    #[default]
    Raw,
    /// The ECS0 reference container (carries its own shape).
    Ecs0,
}

/// A codec driven through the file-based CLI contract.
///
/// Construct directly (for tests) via [`ExternalCodec::new`] then the
/// builder setters, or from a manifest via
/// [`crate::manifest::CodecManifest::into_adapter`].
#[derive(Clone, Debug)]
pub struct ExternalCodec {
    /// Report identifier.
    name: String,
    /// Resolved binary / script path. `Command::new` resolves a bare name
    /// against `PATH`.
    cmd: PathBuf,
    /// Fixed tokens inserted before the `encode` / `decode` subcommand.
    prefix_args: Vec<String>,
    /// `encode` argument template (placeholders substituted per call).
    encode_args: Vec<String>,
    /// `decode` argument template (placeholders substituted per call).
    decode_args: Vec<String>,
    /// Extra environment merged over the `ECS_*` variables.
    env: Vec<(String, String)>,
    /// The codec author's lossless claim (verified, not trusted).
    declared_lossless: bool,
    /// Width of the raw decode stream.
    sample_dtype: SampleDtype,
    /// Format of the encode-input file.
    input_format: InputFormat,
    /// Format of the decode-output file.
    output_format: OutputFormat,
    /// Per-invocation timeout.
    timeout: Duration,
}

/// Default `encode` argument template: `encode {input} {output}`.
pub fn default_encode_args() -> Vec<String> {
    ["encode", "{input}", "{output}"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Default `decode` argument template:
/// `decode {input} {output} --channels {channels} --samples {samples}
/// --rate {rate} --dtype {dtype}`.
pub fn default_decode_args() -> Vec<String> {
    [
        "decode",
        "{input}",
        "{output}",
        "--channels",
        "{channels}",
        "--samples",
        "{samples}",
        "--rate",
        "{rate}",
        "--dtype",
        "{dtype}",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

impl ExternalCodec {
    /// A new codec with the default templates and formats.
    pub fn new(name: impl Into<String>, cmd: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            cmd: cmd.into(),
            prefix_args: Vec::new(),
            encode_args: default_encode_args(),
            decode_args: default_decode_args(),
            env: Vec::new(),
            declared_lossless: false,
            sample_dtype: SampleDtype::default(),
            input_format: InputFormat::default(),
            output_format: OutputFormat::default(),
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }

    /// Set the fixed prefix args (before the subcommand).
    pub fn with_prefix_args(mut self, args: Vec<String>) -> Self {
        self.prefix_args = args;
        self
    }

    /// Override the `encode` / `decode` argument templates.
    pub fn with_templates(mut self, encode: Vec<String>, decode: Vec<String>) -> Self {
        self.encode_args = encode;
        self.decode_args = decode;
        self
    }

    /// Set extra environment variables (merged over the `ECS_*` set).
    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = env;
        self
    }

    /// Set the lossless claim.
    pub fn with_declared_lossless(mut self, v: bool) -> Self {
        self.declared_lossless = v;
        self
    }

    /// Set the raw decode-stream integer width.
    pub fn with_sample_dtype(mut self, d: SampleDtype) -> Self {
        self.sample_dtype = d;
        self
    }

    /// Set the encode-input and decode-output file formats.
    pub fn with_formats(mut self, input: InputFormat, output: OutputFormat) -> Self {
        self.input_format = input;
        self.output_format = output;
        self
    }

    /// Set the per-invocation timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The resolved command path.
    pub fn cmd(&self) -> &PathBuf {
        &self.cmd
    }
}

/// Map a [`SampleDtype`] to its one-byte envelope code.
fn dtype_code(d: SampleDtype) -> u8 {
    match d {
        SampleDtype::I16 => 0,
        SampleDtype::I32 => 1,
        SampleDtype::I64 => 2,
    }
}

/// Inverse of [`dtype_code`].
fn code_dtype(c: u8) -> Option<SampleDtype> {
    match c {
        0 => Some(SampleDtype::I16),
        1 => Some(SampleDtype::I32),
        2 => Some(SampleDtype::I64),
        _ => None,
    }
}

/// Prepend the adapter-private envelope onto a codec's blob.
fn wrap_envelope(
    payload: &[u8],
    n_chan: u32,
    n_samples: u32,
    dtype: SampleDtype,
    fs: f64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(ENVELOPE_HEADER_LEN + payload.len());
    out.extend_from_slice(ENVELOPE_MAGIC);
    out.extend_from_slice(&n_chan.to_le_bytes());
    out.extend_from_slice(&n_samples.to_le_bytes());
    out.push(dtype_code(dtype));
    out.extend_from_slice(&fs.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Strip the envelope, returning `(n_chan, n_samples, dtype, fs, payload)`.
/// `None` on a missing magic or a truncated header.
fn parse_envelope(blob: &[u8]) -> Option<(usize, usize, SampleDtype, f64, &[u8])> {
    if blob.len() < ENVELOPE_HEADER_LEN || &blob[0..4] != ENVELOPE_MAGIC {
        return None;
    }
    let n_chan = u32::from_le_bytes([blob[4], blob[5], blob[6], blob[7]]) as usize;
    let n_samples = u32::from_le_bytes([blob[8], blob[9], blob[10], blob[11]]) as usize;
    let dtype = code_dtype(blob[12])?;
    let fs = f64::from_le_bytes([
        blob[13], blob[14], blob[15], blob[16], blob[17], blob[18], blob[19], blob[20],
    ]);
    Some((n_chan, n_samples, dtype, fs, &blob[ENVELOPE_HEADER_LEN..]))
}

/// Substitute `{placeholder}` tokens in an argument template.
fn substitute(args: &[String], map: &[(&str, String)]) -> Vec<String> {
    args.iter()
        .map(|a| {
            let mut s = a.clone();
            for (k, v) in map {
                if s.contains(k) {
                    s = s.replace(k, v);
                }
            }
            s
        })
        .collect()
}

impl ExternalCodec {
    /// Build the per-call substitution map.
    fn subst_map(
        &self,
        input: &str,
        output: &str,
        n_chan: usize,
        n_samples: usize,
        fs: f64,
    ) -> Vec<(&'static str, String)> {
        vec![
            ("{input}", input.to_string()),
            ("{output}", output.to_string()),
            ("{channels}", n_chan.to_string()),
            ("{samples}", n_samples.to_string()),
            ("{rate}", format!("{fs}")),
            ("{dtype}", self.sample_dtype.as_token().to_string()),
        ]
    }

    /// Spawn `cmd` with the resolved args + merged env, enforcing the
    /// timeout. Returns `true` only on a clean (`exit 0`) completion.
    /// stdout/stderr are discarded so a chatty codec does not pollute the
    /// benchmark output.
    fn run(
        &self,
        args: &[String],
        n_chan: usize,
        n_samples: usize,
        fs: f64,
        workdir: &std::path::Path,
    ) -> bool {
        let mut cmd = Command::new(&self.cmd);
        cmd.args(&self.prefix_args)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // The shape metadata is also exposed as env, for codecs that
            // prefer it over argv (Python codecs commonly do).
            .env("ECS_CHANNELS", n_chan.to_string())
            .env("ECS_SAMPLES", n_samples.to_string())
            .env("ECS_RATE", format!("{fs}"))
            .env("ECS_DTYPE", self.sample_dtype.as_token())
            .env("ECS_WORKDIR", workdir);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(_) => return false,
        };
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status.success(),
                Ok(None) => {
                    if start.elapsed() >= self.timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        return false;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
            }
        }
    }

    /// Encode `signal` at rate `fs`, returning the enveloped blob or
    /// `Vec::new()` on any failure.
    fn try_encode(&self, signal: &[Vec<i64>], fs: f64) -> Vec<u8> {
        let n_chan = signal.len();
        if n_chan == 0 || n_chan > u32::MAX as usize {
            return Vec::new();
        }
        // Per-channel sample count. The default `raw` decode requires a
        // uniform (rectangular) signal so the flat stream reshapes; the
        // `lqs0` output carries its own shape, so a ragged signal is only
        // representable when the codec emits lqs0.
        let n_samples = signal[0].len();
        let uniform = signal.iter().all(|c| c.len() == n_samples);
        if self.output_format == OutputFormat::Raw && (!uniform || n_samples == 0) {
            return Vec::new();
        }
        if n_samples > u32::MAX as usize {
            return Vec::new();
        }

        // Build the encode-input file.
        let input_bytes = match self.input_format {
            InputFormat::Edf => match write_edf_bytes(signal, fs) {
                Some(b) => b,
                None => return Vec::new(),
            },
            InputFormat::Ecs0 => serialize(signal),
        };

        let dir = match ScratchDir::new("ext_enc") {
            Ok(d) => d,
            Err(_) => return Vec::new(),
        };
        let in_name = match self.input_format {
            InputFormat::Edf => "in.edf",
            InputFormat::Ecs0 => "in.ecs0",
        };
        let in_path = dir.join(in_name);
        let out_path = dir.join("blob.bin");
        if std::fs::write(&in_path, &input_bytes).is_err() {
            return Vec::new();
        }

        let map = self.subst_map(
            &in_path.to_string_lossy(),
            &out_path.to_string_lossy(),
            n_chan,
            n_samples,
            fs,
        );
        let args = substitute(&self.encode_args, &map);
        if !self.run(&args, n_chan, n_samples, fs, &dir.path) {
            return Vec::new();
        }

        let payload = match std::fs::read(&out_path) {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };
        wrap_envelope(&payload, n_chan as u32, n_samples as u32, self.sample_dtype, fs)
    }

    /// Decode an enveloped blob back to the per-channel signal, or
    /// `Vec::new()` on any failure.
    fn try_decode(&self, blob: &[u8]) -> Vec<Vec<i64>> {
        let (n_chan, n_samples, dtype, fs, payload) = match parse_envelope(blob) {
            Some(parts) => parts,
            None => return Vec::new(),
        };

        let dir = match ScratchDir::new("ext_dec") {
            Ok(d) => d,
            Err(_) => return Vec::new(),
        };
        let in_path = dir.join("blob.bin");
        let out_name = match self.output_format {
            OutputFormat::Raw => "out.raw",
            OutputFormat::Ecs0 => "out.ecs0",
        };
        let out_path = dir.join(out_name);
        if std::fs::write(&in_path, payload).is_err() {
            return Vec::new();
        }

        let map = self.subst_map(
            &in_path.to_string_lossy(),
            &out_path.to_string_lossy(),
            n_chan,
            n_samples,
            fs,
        );
        let args = substitute(&self.decode_args, &map);
        if !self.run(&args, n_chan, n_samples, fs, &dir.path) {
            return Vec::new();
        }

        let out = match std::fs::read(&out_path) {
            Ok(o) => o,
            Err(_) => return Vec::new(),
        };
        match self.output_format {
            OutputFormat::Raw => {
                // Reshape using the dtype the envelope recorded at encode
                // time, not `self.sample_dtype`, so the two always agree.
                let _ = dtype; // recorded == self.sample_dtype by construction
                reshape_channel_major(&out, n_chan, n_samples, self.sample_dtype).unwrap_or_default()
            }
            OutputFormat::Ecs0 => deserialize(&out),
        }
    }
}

impl Codec for ExternalCodec {
    fn name(&self) -> &str {
        &self.name
    }

    fn declared_lossless(&self) -> bool {
        self.declared_lossless
    }

    fn encode(&self, signal: &[Vec<i64>], fs: f64) -> Vec<u8> {
        let rate = if fs.is_finite() && fs > 0.0 { fs } else { 1.0 };
        self.try_encode(signal, rate)
    }

    fn decode(&self, blob: &[u8]) -> Vec<Vec<i64>> {
        self.try_decode(blob)
    }
}

/// Resolve a codec command to a runnable form, or `None` if unusable.
///
/// Order: `$ECS_CODEC_<NAME>_BIN` (uppercased, `-`→`_`) if it points at a
/// file; then `cmd` itself — required to be an existing file if it contains
/// a path separator, otherwise trusted as a bare `PATH` command name that
/// `Command::new` resolves at spawn time.
pub fn resolve_cmd(name: &str, cmd: &str) -> Option<PathBuf> {
    let envvar = format!("ECS_CODEC_{}_BIN", name.to_uppercase().replace('-', "_"));
    if let Some(p) = std::env::var_os(&envvar) {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    let pb = PathBuf::from(cmd);
    if cmd.contains('/') || cmd.contains('\\') {
        if pb.is_file() {
            return Some(pb);
        }
        return None;
    }
    // Bare command name: let the OS resolve it on PATH at spawn time.
    Some(pb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trips() {
        let payload = b"arbitrary codec bytes";
        let blob = wrap_envelope(payload, 4, 256, SampleDtype::I32, 250.0);
        let (n_chan, n_samples, dtype, fs, back) =
            parse_envelope(&blob).expect("valid envelope parses");
        assert_eq!(n_chan, 4);
        assert_eq!(n_samples, 256);
        assert_eq!(dtype, SampleDtype::I32);
        assert_eq!(fs, 250.0);
        assert_eq!(back, payload);
    }

    #[test]
    fn parse_envelope_rejects_bad_input() {
        assert!(parse_envelope(b"").is_none());
        assert!(parse_envelope(b"XXXX too short").is_none());
        // Right length region but wrong magic.
        let mut bad = vec![0u8; ENVELOPE_HEADER_LEN + 4];
        bad[0] = b'N';
        assert!(parse_envelope(&bad).is_none());
        // Good magic but an invalid dtype code (3).
        let mut bad2 = wrap_envelope(b"x", 1, 1, SampleDtype::I16, 1.0);
        bad2[12] = 3;
        assert!(parse_envelope(&bad2).is_none());
    }

    #[test]
    fn decode_of_garbage_is_empty() {
        // No binary is spawned: a blob with no valid envelope short-circuits.
        let codec = ExternalCodec::new("x", "/nonexistent/codec");
        assert!(codec.decode(b"").is_empty());
        assert!(codec.decode(b"not an envelope").is_empty());
    }

    #[test]
    fn empty_signal_encodes_empty() {
        // Zero channels cannot be represented; encode short-circuits before
        // any spawn, so this is binary-free.
        let codec = ExternalCodec::new("x", "/nonexistent/codec");
        assert!(codec.encode(&[], 256.0).is_empty());
    }

    #[test]
    fn name_and_claim_are_pure() {
        let codec = ExternalCodec::new("mycodec", "python3").with_declared_lossless(true);
        assert_eq!(codec.name(), "mycodec");
        assert!(codec.declared_lossless());
    }

    #[test]
    fn default_templates_carry_placeholders() {
        let enc = default_encode_args();
        assert_eq!(enc[0], "encode");
        assert!(enc.iter().any(|a| a == "{input}"));
        assert!(enc.iter().any(|a| a == "{output}"));
        let dec = default_decode_args();
        assert_eq!(dec[0], "decode");
        for tok in ["{input}", "{output}", "{channels}", "{samples}", "{rate}", "{dtype}"] {
            assert!(dec.iter().any(|a| a == tok), "decode template missing {tok}");
        }
    }

    #[test]
    fn substitute_replaces_tokens() {
        let tmpl = default_decode_args();
        let map = vec![
            ("{input}", "/tmp/a".to_string()),
            ("{output}", "/tmp/b".to_string()),
            ("{channels}", "3".to_string()),
            ("{samples}", "8".to_string()),
            ("{rate}", "256".to_string()),
            ("{dtype}", "i32".to_string()),
        ];
        let out = substitute(&tmpl, &map);
        assert!(out.contains(&"/tmp/a".to_string()));
        assert!(out.contains(&"3".to_string()));
        assert!(out.contains(&"i32".to_string()));
        assert!(!out.iter().any(|a| a.contains('{')), "no placeholder left");
    }

    #[test]
    fn resolve_cmd_bare_name_is_trusted() {
        // A bare name is trusted (PATH-resolved at spawn).
        assert_eq!(resolve_cmd("c", "python3"), Some(PathBuf::from("python3")));
        // A path that does not exist is rejected.
        assert!(resolve_cmd("c", "/definitely/not/here/codec").is_none());
    }
}
