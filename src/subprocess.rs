//! Shared subprocess + file-IO primitives for the file-driven codec
//! adapters.
//!
//! The generic external-codec adapter ([`crate::adapters_external`], which
//! drives any conformant codec via the file-based CLI contract of
//! `SPEC/OpenECS-v1.0.md` §6) — and any vendor adapter that shells out to a
//! specific codec binary — need the same handful of building blocks:
//!
//! - [`ScratchDir`] — a uniquely-named temp directory removed on drop, so
//!   concurrent encode/decode calls never collide and cleanup is one
//!   `remove_dir_all`.
//! - [`write_edf_bytes`] — render a per-channel integer signal as a
//!   minimal, spec-valid EDF byte image (the default encode-input format).
//! - [`reshape_channel_major`] — split a flat little-endian integer stream
//!   (the default decode-output format) into one `Vec<i64>` per channel,
//!   validating the byte length against the declared shape first.
//! - [`SampleDtype`] — the integer width of the raw decode stream.
//!
//! This module is pure `std` (no new dependencies beyond `serde` for the
//! small [`SampleDtype`] enum, which the manifest layer deserializes). It
//! is *not* on the grading hot path — the metric/grade core stays
//! dependency-light.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Size in bytes of the fixed EDF main header and of each signal header.
const EDF_HEADER_BLOCK: usize = 256;

/// Integer width of the raw, channel-major decode stream a conformant
/// codec writes (and that [`reshape_channel_major`] reads back).
///
/// EEG digital ADC samples are 16-bit, but a lossless codec round-tripping
/// through wider intermediate domains (e.g. `lml`'s int32 output) may emit
/// a wider stream; the dtype is declared per codec in its manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SampleDtype {
    /// 16-bit signed little-endian.
    I16,
    /// 32-bit signed little-endian.
    I32,
    /// 64-bit signed little-endian.
    I64,
}

impl SampleDtype {
    /// Width of one sample in bytes (2, 4, or 8).
    pub fn width(self) -> usize {
        match self {
            SampleDtype::I16 => 2,
            SampleDtype::I32 => 4,
            SampleDtype::I64 => 8,
        }
    }

    /// The canonical lowercase token (`"i16"`, `"i32"`, `"i64"`) — the form
    /// passed to a codec via the `--dtype` flag and `ECS_DTYPE` env var.
    pub fn as_token(self) -> &'static str {
        match self {
            SampleDtype::I16 => "i16",
            SampleDtype::I32 => "i32",
            SampleDtype::I64 => "i64",
        }
    }
}

impl Default for SampleDtype {
    /// `i32` — wide enough for the EEG i16 digital domain and for `lml`'s
    /// int32 decode stream, the conservative default for a new codec.
    fn default() -> Self {
        SampleDtype::I32
    }
}

/// A scratch directory under the system temp dir, removed on drop.
///
/// File-driven codecs write a handful of sidecar files (the input, the
/// blob, the output, plus any codec-private state) next to their output;
/// isolating each invocation in its own directory keeps concurrent adapter
/// calls from colliding and makes cleanup a single `remove_dir_all`.
pub struct ScratchDir {
    /// Absolute path to the created directory.
    pub path: PathBuf,
}

impl ScratchDir {
    /// Create a uniquely-named scratch directory (`pid` + nanos + seq +
    /// `tag`). The monotonic counter disambiguates two calls within the
    /// same nanosecond, so parallel adapter invocations never collide.
    pub fn new(tag: &str) -> std::io::Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let name = format!("lqs_{}_{}_{}_{}", tag, std::process::id(), nanos, seq);
        let path = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    /// Join a file name onto the scratch directory.
    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        // Best-effort cleanup; a leaked tempdir must never panic a test.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Write an ASCII value left-justified into a fixed-width EDF field,
/// space-padded. Returns `Err(())` if the value overflows the field.
fn edf_field(out: &mut Vec<u8>, value: &str, width: usize) -> Result<(), ()> {
    let bytes = value.as_bytes();
    if bytes.len() > width {
        return Err(());
    }
    out.extend_from_slice(bytes);
    out.resize(out.len() + (width - bytes.len()), b' ');
    Ok(())
}

/// Format a float for an EDF ASCII field as compactly as possible.
///
/// EDF numeric fields are ASCII; integers render without a decimal point
/// and fractional values keep just enough digits to round-trip the rate.
fn format_edf_number(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        let s = format!("{x:.6}");
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        trimmed.to_string()
    }
}

/// Build a minimal, spec-valid EDF byte image for `signal` at rate `fs`.
///
/// One data record holds every channel's samples (`record_duration`
/// chosen so the stored rate matches `fs`). Returns `None` when the signal
/// cannot be expressed as EDF digital samples: non-i16 values, ragged
/// channels, or an empty / zero-length signal. The layout mirrors
/// [`crate::edf::read_edf`]'s parser exactly, so the EDF this writes reads
/// back identically.
pub fn write_edf_bytes(signal: &[Vec<i64>], fs: f64) -> Option<Vec<u8>> {
    let ns = signal.len();
    if ns == 0 {
        return None;
    }
    // All channels must share one length (EDF single-rate requirement).
    let spr = signal[0].len();
    if spr == 0 || signal.iter().any(|c| c.len() != spr) {
        return None;
    }
    // Every sample must fit signed 16-bit (the EDF digital domain).
    if signal
        .iter()
        .flat_map(|c| c.iter())
        .any(|&s| s < i16::MIN as i64 || s > i16::MAX as i64)
    {
        return None;
    }
    if !fs.is_finite() || fs <= 0.0 {
        return None;
    }

    // One record covers the whole signal: record_duration = spr / fs so the
    // per-signal rate (spr / duration) reconstructs `fs`.
    let record_duration = spr as f64 / fs;
    let dur_str = format_edf_number(record_duration);
    if dur_str.len() > 8 {
        return None;
    }

    let header_bytes = EDF_HEADER_BLOCK + ns * EDF_HEADER_BLOCK;
    let header_str = header_bytes.to_string();
    if header_str.len() > 8 || ns.to_string().len() > 4 || spr.to_string().len() > 8 {
        return None;
    }

    let mut buf = Vec::with_capacity(header_bytes + ns * spr * 2);

    // ── Main header. ───────────────────────────────────────────────────
    edf_field(&mut buf, "0", 8).ok()?; // version
    edf_field(&mut buf, "OpenECS X X X", 80).ok()?; // patient
    edf_field(&mut buf, "Startdate X", 80).ok()?; // recording
    edf_field(&mut buf, "01.01.26", 8).ok()?; // startdate
    edf_field(&mut buf, "00.00.00", 8).ok()?; // starttime
    edf_field(&mut buf, &header_str, 8).ok()?; // header_bytes
    edf_field(&mut buf, "", 44).ok()?; // reserved
    edf_field(&mut buf, "1", 8).ok()?; // n_data_records = 1
    edf_field(&mut buf, &dur_str, 8).ok()?; // record_duration_sec
    edf_field(&mut buf, &ns.to_string(), 4).ok()?; // n_signals

    // ── Signal headers, field-by-field across all signals. ─────────────
    for i in 0..ns {
        edf_field(&mut buf, &format!("ch{i}"), 16).ok()?; // label
    }
    for _ in 0..ns {
        edf_field(&mut buf, "AgAgCl", 80).ok()?; // transducer
    }
    for _ in 0..ns {
        edf_field(&mut buf, "uV", 8).ok()?; // phys_dim
    }
    for _ in 0..ns {
        edf_field(&mut buf, "-32768", 8).ok()?; // phys_min
    }
    for _ in 0..ns {
        edf_field(&mut buf, "32767", 8).ok()?; // phys_max
    }
    for _ in 0..ns {
        edf_field(&mut buf, "-32768", 8).ok()?; // dig_min
    }
    for _ in 0..ns {
        edf_field(&mut buf, "32767", 8).ok()?; // dig_max
    }
    for _ in 0..ns {
        edf_field(&mut buf, "", 80).ok()?; // prefilter
    }
    for _ in 0..ns {
        edf_field(&mut buf, &spr.to_string(), 8).ok()?; // n_samples_per_record
    }
    for _ in 0..ns {
        edf_field(&mut buf, "", 32).ok()?; // signal reserved
    }
    debug_assert_eq!(buf.len(), header_bytes, "EDF header block size mismatch");

    // ── Data: one record, signals in order, each `spr` little-endian i16.
    for chan in signal {
        for &s in chan {
            buf.extend_from_slice(&(s as i16).to_le_bytes());
        }
    }

    Some(buf)
}

/// Split a flat little-endian, channel-major integer stream into one
/// `Vec<i64>` per channel.
///
/// `raw` holds `n_chan * per_chan` integers, each `dtype.width()` bytes,
/// signed and little-endian, laid out channel-major (all of channel 0, then
/// all of channel 1, …). The byte length is validated against the declared
/// shape **before** any split: a stream that does not match
/// `n_chan * per_chan * width` returns `None`, which the adapter surfaces to
/// the L-tier gate as a failed round trip rather than a panic.
pub fn reshape_channel_major(
    raw: &[u8],
    n_chan: usize,
    per_chan: usize,
    dtype: SampleDtype,
) -> Option<Vec<Vec<i64>>> {
    let width = dtype.width();
    let total = n_chan.checked_mul(per_chan)?.checked_mul(width)?;
    if raw.len() != total {
        return None;
    }

    let mut out = Vec::with_capacity(n_chan);
    let mut pos = 0usize;
    for _ in 0..n_chan {
        let mut chan = Vec::with_capacity(per_chan);
        for _ in 0..per_chan {
            let v = match dtype {
                SampleDtype::I16 => {
                    i16::from_le_bytes([raw[pos], raw[pos + 1]]) as i64
                }
                SampleDtype::I32 => {
                    i32::from_le_bytes([raw[pos], raw[pos + 1], raw[pos + 2], raw[pos + 3]])
                        as i64
                }
                SampleDtype::I64 => i64::from_le_bytes([
                    raw[pos],
                    raw[pos + 1],
                    raw[pos + 2],
                    raw[pos + 3],
                    raw[pos + 4],
                    raw[pos + 5],
                    raw[pos + 6],
                    raw[pos + 7],
                ]),
            };
            chan.push(v);
            pos += width;
        }
        out.push(chan);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<Vec<i64>> {
        vec![vec![0, 1, -1, 1000, -1000], vec![5, 6, 7, 8, 9]]
    }

    #[test]
    fn scratchdir_is_unique_and_cleans_up() {
        let a = ScratchDir::new("t").expect("scratch a");
        let b = ScratchDir::new("t").expect("scratch b");
        assert_ne!(a.path, b.path, "two scratch dirs must not collide");
        let p = a.path.clone();
        assert!(p.is_dir());
        drop(a);
        assert!(!p.exists(), "scratch dir removed on drop");
        drop(b);
    }

    #[test]
    fn write_edf_rejects_unsupported_shapes() {
        assert!(write_edf_bytes(&[], 256.0).is_none());
        assert!(write_edf_bytes(&[vec![]], 256.0).is_none());
        assert!(write_edf_bytes(&[vec![1, 2], vec![1]], 256.0).is_none());
        assert!(write_edf_bytes(&[vec![i64::MAX]], 256.0).is_none());
        assert!(write_edf_bytes(&[vec![1, 2]], 0.0).is_none());
        let edf = write_edf_bytes(&fixture(), 256.0).expect("valid fixture -> EDF");
        let ns = 2usize;
        let spr = 5usize;
        assert_eq!(edf.len(), EDF_HEADER_BLOCK * (1 + ns) + ns * spr * 2);
        assert_eq!(&edf[..8], b"0       ", "EDF version field");
    }

    #[test]
    fn reshape_round_trips_each_dtype() {
        for dt in [SampleDtype::I16, SampleDtype::I32, SampleDtype::I64] {
            let sig = vec![vec![0i64, 1, -1, 100], vec![-100, 2, 3, 4]];
            // Build the channel-major LE stream the codec would emit.
            let mut raw = Vec::new();
            for chan in &sig {
                for &s in chan {
                    match dt {
                        SampleDtype::I16 => raw.extend_from_slice(&(s as i16).to_le_bytes()),
                        SampleDtype::I32 => raw.extend_from_slice(&(s as i32).to_le_bytes()),
                        SampleDtype::I64 => raw.extend_from_slice(&s.to_le_bytes()),
                    }
                }
            }
            let back = reshape_channel_major(&raw, 2, 4, dt).expect("valid stream");
            assert_eq!(back, sig, "round trip for {dt:?}");
        }
    }

    #[test]
    fn reshape_rejects_wrong_length() {
        // 2 chan * 4 samp * 4 bytes = 32 expected; give 30.
        let raw = vec![0u8; 30];
        assert!(reshape_channel_major(&raw, 2, 4, SampleDtype::I32).is_none());
        // Overflow in the shape multiply is None, not a panic.
        assert!(reshape_channel_major(&[], usize::MAX, usize::MAX, SampleDtype::I64).is_none());
    }

    #[test]
    fn dtype_width_and_token() {
        assert_eq!(SampleDtype::I16.width(), 2);
        assert_eq!(SampleDtype::I32.width(), 4);
        assert_eq!(SampleDtype::I64.width(), 8);
        assert_eq!(SampleDtype::default(), SampleDtype::I32);
        assert_eq!(SampleDtype::I16.as_token(), "i16");
    }
}
