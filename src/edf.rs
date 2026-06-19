//! Minimal pure-Rust EDF (European Data Format) reader.
//!
//! This is a deliberately small, dependency-free reader: just enough of
//! the EDF spec to pull the **digital** (raw int16 ADC) samples out of a
//! `.edf` file so the OpenECS benchmark CLI can grade real recordings instead
//! of only the built-in synthetic fixture. The OpenECS codec operates on the
//! integer sample domain, so we return the digital samples verbatim as
//! `i64` and never apply the physical-units affine scaling.
//!
//! ## EDF layout (the subset we parse)
//!
//! A 256-byte ASCII main header, then `ns` × 256-byte signal headers laid
//! out **field-by-field across all signals** (all labels, then all
//! transducers, …), then the data records. Each data record holds, for
//! each signal in order, `n_samples_per_record` little-endian `int16`
//! samples. The per-signal sampling rate is
//! `n_samples_per_record / record_duration_sec`.
//!
//! Channels labelled `"EDF Annotations"` (the EDF+ timestamp/event track)
//! are not signal data and are dropped.
//!
//! ## Uniform-rate policy
//!
//! EDF permits a different rate per signal. OpenECS grades a single `fs`, so
//! this reader **requires the kept signals to share one rate**: it takes
//! the first kept (non-annotation) signal's `fs` as the reference and
//! returns an [`std::io::Error`] if any other kept signal disagrees. This
//! is the simplest contract that never silently mixes rates.
//!
//! ## Robustness
//!
//! Every read is bounds-checked. A short or truncated file — header cut
//! off, a field too short to hold its ASCII number, a data section
//! smaller than the declared record count — yields an
//! [`std::io::Error`], never a panic.

use std::io::{self, Error, ErrorKind, Read};
use std::path::Path;

/// Decoded EDF signal: the digital (raw integer ADC) samples, one channel
/// per kept signal, plus the shared sampling rate and the channel labels.
#[derive(Clone, Debug)]
pub struct EdfSignal {
    /// Shared sampling rate in Hz (all kept channels agree on this).
    pub fs: f64,
    /// Raw digital int16 samples, widened to `i64`, one `Vec` per channel.
    pub channels: Vec<Vec<i64>>,
    /// Trimmed signal labels, parallel to `channels`.
    pub labels: Vec<String>,
}

/// Size in bytes of the fixed main header and of each signal header.
const HEADER_BLOCK: usize = 256;
/// The EDF+ annotation track label (trimmed). These channels are dropped.
const ANNOTATION_LABEL: &str = "EDF Annotations";

/// Read an ASCII field of `len` bytes starting at `*pos`, advancing `*pos`.
/// Returns the raw bytes; bounds-checked against `buf`.
fn take<'a>(buf: &'a [u8], pos: &mut usize, len: usize) -> io::Result<&'a [u8]> {
    let end = pos
        .checked_add(len)
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "EDF header offset overflow"))?;
    if end > buf.len() {
        return Err(Error::new(
            ErrorKind::UnexpectedEof,
            "EDF header truncated: not enough bytes for field",
        ));
    }
    let field = &buf[*pos..end];
    *pos = end;
    Ok(field)
}

/// Trim trailing/leading ASCII whitespace from an EDF field and return it
/// as a `String`. EDF pads fields with spaces; we treat the field as ASCII
/// (lossy-decode any stray non-ASCII so we never panic on bad bytes).
fn field_str(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

/// Parse an EDF ASCII numeric field into an `i64`.
fn parse_i64(bytes: &[u8], what: &str) -> io::Result<i64> {
    let s = field_str(bytes);
    s.parse::<i64>().map_err(|_| {
        Error::new(
            ErrorKind::InvalidData,
            format!("EDF: malformed integer field {what:?}: {s:?}"),
        )
    })
}

/// Parse an EDF ASCII numeric field into an `f64`.
fn parse_f64(bytes: &[u8], what: &str) -> io::Result<f64> {
    let s = field_str(bytes);
    s.parse::<f64>().map_err(|_| {
        Error::new(
            ErrorKind::InvalidData,
            format!("EDF: malformed float field {what:?}: {s:?}"),
        )
    })
}

/// Parse a `.edf` file into an [`EdfSignal`].
///
/// Parses the header, reads every data record, deinterleaves the
/// per-record int16 blocks into one channel per signal, drops
/// `"EDF Annotations"` channels, and enforces a single shared sampling
/// rate across the kept channels (see the module docs). Returns an
/// [`std::io::Error`] on any I/O failure, malformed header, rate
/// disagreement, or truncation — never panics.
pub fn read_edf<P: AsRef<Path>>(path: P) -> io::Result<EdfSignal> {
    let mut buf = Vec::new();
    std::fs::File::open(path.as_ref())?.read_to_end(&mut buf)?;

    // ── Main header (256 bytes, field-by-field). ──────────────────────
    if buf.len() < HEADER_BLOCK {
        return Err(Error::new(
            ErrorKind::UnexpectedEof,
            "EDF file shorter than 256-byte main header",
        ));
    }
    let mut pos = 0usize;
    let _version = take(&buf, &mut pos, 8)?;
    let _patient = take(&buf, &mut pos, 80)?;
    let _recording = take(&buf, &mut pos, 80)?;
    let _startdate = take(&buf, &mut pos, 8)?;
    let _starttime = take(&buf, &mut pos, 8)?;
    let header_bytes = parse_i64(take(&buf, &mut pos, 8)?, "header_bytes")?;
    let _reserved = take(&buf, &mut pos, 44)?;
    let n_records = parse_i64(take(&buf, &mut pos, 8)?, "n_data_records")?;
    let record_duration = parse_f64(take(&buf, &mut pos, 8)?, "record_duration_sec")?;
    let ns = parse_i64(take(&buf, &mut pos, 4)?, "n_signals")?;

    if ns < 0 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "EDF: negative signal count",
        ));
    }
    let ns = ns as usize;
    if ns == 0 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "EDF: file declares zero signals",
        ));
    }
    if record_duration <= 0.0 || !record_duration.is_finite() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "EDF: non-positive record duration",
        ));
    }
    // EDF allows n_data_records == -1 ("unknown"); we require a concrete
    // count because we read records up front rather than streaming.
    if n_records < 0 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "EDF: unknown/negative data-record count not supported",
        ));
    }
    let n_records = n_records as usize;

    // The declared header length must cover the main header plus one
    // 256-byte block per signal. Trust the field but sanity-check it.
    let expected_header = HEADER_BLOCK
        .checked_add(ns.checked_mul(HEADER_BLOCK).ok_or_else(|| {
            Error::new(ErrorKind::InvalidData, "EDF: signal count overflows header")
        })?)
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "EDF: header size overflow"))?;
    if header_bytes < expected_header as i64 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "EDF: declared header_bytes smaller than ns*256+256",
        ));
    }
    let header_bytes = header_bytes as usize;

    // ── Signal headers: each field is stored as ns contiguous entries. ─
    // labels(16) transducer(80) phys_dim(8) phys_min(8) phys_max(8)
    // dig_min(8) dig_max(8) prefilter(80) n_samples(8) reserved(32)
    let labels_raw = take(&buf, &mut pos, ns * 16)?.to_vec();
    let _transducer = take(&buf, &mut pos, ns * 80)?;
    let _phys_dim = take(&buf, &mut pos, ns * 8)?;
    let _phys_min = take(&buf, &mut pos, ns * 8)?;
    let _phys_max = take(&buf, &mut pos, ns * 8)?;
    let _dig_min = take(&buf, &mut pos, ns * 8)?;
    let _dig_max = take(&buf, &mut pos, ns * 8)?;
    let _prefilter = take(&buf, &mut pos, ns * 80)?;
    let nsamp_raw = take(&buf, &mut pos, ns * 8)?.to_vec();
    let _sig_reserved = take(&buf, &mut pos, ns * 32)?;

    let mut labels = Vec::with_capacity(ns);
    let mut samples_per_record = Vec::with_capacity(ns);
    for i in 0..ns {
        labels.push(field_str(&labels_raw[i * 16..i * 16 + 16]));
        let n = parse_i64(&nsamp_raw[i * 8..i * 8 + 8], "n_samples_per_record")?;
        if n < 0 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "EDF: negative n_samples_per_record",
            ));
        }
        samples_per_record.push(n as usize);
    }

    // ── Data section: one record = sum_i(samples_per_record[i]) int16. ─
    let record_samples: usize = samples_per_record
        .iter()
        .try_fold(0usize, |acc, &n| acc.checked_add(n))
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "EDF: record sample count overflow"))?;
    let record_bytes = record_samples
        .checked_mul(2)
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "EDF: record byte size overflow"))?;
    let data_bytes = record_bytes
        .checked_mul(n_records)
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "EDF: data section size overflow"))?;

    // Data starts at the declared header length (which we verified covers
    // the fixed + signal headers). Some writers pad; honour header_bytes.
    let data_start = header_bytes;
    let data_end = data_start
        .checked_add(data_bytes)
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "EDF: data section end overflow"))?;
    if data_end > buf.len() {
        return Err(Error::new(
            ErrorKind::UnexpectedEof,
            "EDF: data section truncated (fewer bytes than declared records)",
        ));
    }

    // Allocate one channel buffer per signal, sized to its full length.
    let mut channels: Vec<Vec<i64>> = samples_per_record
        .iter()
        .map(|&n| Vec::with_capacity(n.saturating_mul(n_records)))
        .collect();

    // Deinterleave: walk records, and within each record walk signals,
    // reading samples_per_record[i] little-endian int16 into channel i.
    let mut cursor = data_start;
    for _rec in 0..n_records {
        for (sig, &n) in samples_per_record.iter().enumerate() {
            let chan = &mut channels[sig];
            for _ in 0..n {
                // cursor + 2 <= data_end <= buf.len() by construction, but
                // index defensively so a logic slip can never panic.
                let lo = buf[cursor];
                let hi = buf[cursor + 1];
                let val = i16::from_le_bytes([lo, hi]) as i64;
                chan.push(val);
                cursor += 2;
            }
        }
    }

    // ── Drop annotation channels, then enforce a single shared rate. ──
    let mut out_channels = Vec::new();
    let mut out_labels = Vec::new();
    let mut ref_fs: Option<f64> = None;

    for i in 0..ns {
        if labels[i] == ANNOTATION_LABEL {
            continue;
        }
        let fs = samples_per_record[i] as f64 / record_duration;
        match ref_fs {
            None => ref_fs = Some(fs),
            Some(r) => {
                if (fs - r).abs() > 1e-9 {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        format!(
                            "EDF: non-uniform sampling rates ({r} Hz vs {fs} Hz on \
                             {label:?}); OpenECS requires one shared fs",
                            label = labels[i]
                        ),
                    ));
                }
            }
        }
        out_channels.push(std::mem::take(&mut channels[i]));
        out_labels.push(labels[i].clone());
    }

    let fs = ref_fs.ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "EDF: no signal channels (all dropped as annotations)",
        )
    })?;

    Ok(EdfSignal {
        fs,
        channels: out_channels,
        labels: out_labels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write an ASCII value left-justified into a fixed-width field of
    /// `width` bytes, space-padded — exactly how EDF stores header fields.
    fn ascii_field(out: &mut Vec<u8>, value: &str, width: usize) {
        let bytes = value.as_bytes();
        assert!(bytes.len() <= width, "test field {value:?} exceeds width");
        out.extend_from_slice(bytes);
        out.extend(std::iter::repeat(b' ').take(width - bytes.len()));
    }

    /// Build a minimal but spec-valid EDF byte image in memory.
    ///
    /// `samples` is one `Vec<i16>` per signal, length = n_records *
    /// samples_per_record[i]. All signals here share the same
    /// samples_per_record and record_duration = 1.0, so fs is uniform.
    fn build_edf(labels: &[&str], samples_per_record: usize, records: &[Vec<i16>]) -> Vec<u8> {
        let ns = labels.len();
        let n_records = if ns == 0 { 0 } else { records.len() / ns };
        let header_bytes = HEADER_BLOCK + ns * HEADER_BLOCK;

        let mut buf = Vec::new();
        // Main header.
        ascii_field(&mut buf, "0", 8); // version
        ascii_field(&mut buf, "X X X X", 80); // patient
        ascii_field(&mut buf, "Startdate", 80); // recording
        ascii_field(&mut buf, "01.01.26", 8); // startdate
        ascii_field(&mut buf, "00.00.00", 8); // starttime
        ascii_field(&mut buf, &header_bytes.to_string(), 8); // header bytes
        ascii_field(&mut buf, "", 44); // reserved
        ascii_field(&mut buf, &n_records.to_string(), 8); // n_data_records
        ascii_field(&mut buf, "1", 8); // record_duration_sec = 1.0
        ascii_field(&mut buf, &ns.to_string(), 4); // n_signals

        // Signal headers, field-by-field across all signals.
        for &l in labels {
            ascii_field(&mut buf, l, 16);
        }
        for _ in 0..ns {
            ascii_field(&mut buf, "AgAgCl", 80); // transducer
        }
        for _ in 0..ns {
            ascii_field(&mut buf, "uV", 8); // phys_dim
        }
        for _ in 0..ns {
            ascii_field(&mut buf, "-32768", 8); // phys_min
        }
        for _ in 0..ns {
            ascii_field(&mut buf, "32767", 8); // phys_max
        }
        for _ in 0..ns {
            ascii_field(&mut buf, "-32768", 8); // dig_min
        }
        for _ in 0..ns {
            ascii_field(&mut buf, "32767", 8); // dig_max
        }
        for _ in 0..ns {
            ascii_field(&mut buf, "HP:0.1Hz", 80); // prefilter
        }
        for _ in 0..ns {
            ascii_field(&mut buf, &samples_per_record.to_string(), 8); // n_samples
        }
        for _ in 0..ns {
            ascii_field(&mut buf, "", 32); // signal reserved
        }
        assert_eq!(buf.len(), header_bytes, "header block size mismatch");

        // Data records: records[] is already in (record, signal) order;
        // each entry is the int16 block for that (record, signal).
        for block in records {
            for &s in block {
                buf.extend_from_slice(&s.to_le_bytes());
            }
        }
        buf
    }

    /// Write `bytes` to a uniquely-named tempfile under the system temp dir.
    fn write_tempfile(tag: &str, bytes: &[u8]) -> std::path::PathBuf {
        // Unique enough for a test: pid + a monotonic-ish nanos stamp + tag.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let name = format!("lqs_edf_test_{}_{}_{}.edf", std::process::id(), nanos, tag);
        let path = std::env::temp_dir().join(name);
        let mut f = std::fs::File::create(&path).expect("create tempfile");
        f.write_all(bytes).expect("write tempfile");
        f.flush().expect("flush tempfile");
        path
    }

    #[test]
    fn roundtrip_two_signals_two_records() {
        // 2 signals, samples_per_record = 3, 2 records, record_duration 1s
        // -> fs = 3 Hz, each channel 6 samples.
        // Data order per spec: record 0 [sig0, sig1], record 1 [sig0, sig1].
        let sig0_r0: Vec<i16> = vec![1, 2, 3];
        let sig1_r0: Vec<i16> = vec![-1, -2, -3];
        let sig0_r1: Vec<i16> = vec![100, 200, 300];
        let sig1_r1: Vec<i16> = vec![-100, i16::MIN, i16::MAX];
        let records = vec![
            sig0_r0.clone(),
            sig1_r0.clone(),
            sig0_r1.clone(),
            sig1_r1.clone(),
        ];
        let bytes = build_edf(&["Fp1", "Fp2"], 3, &records);
        let path = write_tempfile("rt2x2", &bytes);

        let result = read_edf(&path);
        // Clean up before asserting so a failure can't leak the tempfile.
        let _ = std::fs::remove_file(&path);
        let edf = result.expect("read_edf must succeed on valid fixture");

        assert_eq!(edf.channels.len(), 2, "two kept channels");
        assert_eq!(edf.labels, vec!["Fp1".to_string(), "Fp2".to_string()]);
        assert_eq!(edf.fs, 3.0, "fs = samples_per_record / duration");

        // Channel 0 = sig0 across both records; channel 1 = sig1.
        let expect0: Vec<i64> = sig0_r0
            .iter()
            .chain(sig0_r1.iter())
            .map(|&v| v as i64)
            .collect();
        let expect1: Vec<i64> = sig1_r0
            .iter()
            .chain(sig1_r1.iter())
            .map(|&v| v as i64)
            .collect();
        assert_eq!(edf.channels[0], expect0, "channel 0 digital samples");
        assert_eq!(edf.channels[1], expect1, "channel 1 digital samples");
    }

    #[test]
    fn drops_annotation_channel() {
        // 2 signals: one real, one "EDF Annotations" — the annotation
        // track is dropped, leaving exactly one channel.
        let real_r0: Vec<i16> = vec![10, 20];
        let anno_r0: Vec<i16> = vec![0, 0];
        let real_r1: Vec<i16> = vec![30, 40];
        let anno_r1: Vec<i16> = vec![0, 0];
        let records = vec![real_r0, anno_r0, real_r1, anno_r1];
        let bytes = build_edf(&["C3", "EDF Annotations"], 2, &records);
        let path = write_tempfile("anno", &bytes);

        let result = read_edf(&path);
        let _ = std::fs::remove_file(&path);
        let edf = result.expect("read_edf must succeed");

        assert_eq!(edf.channels.len(), 1, "annotation channel dropped");
        assert_eq!(edf.labels, vec!["C3".to_string()]);
        assert_eq!(edf.fs, 2.0);
        assert_eq!(edf.channels[0], vec![10i64, 20, 30, 40]);
    }

    #[test]
    fn truncated_data_is_error_not_panic() {
        let records = vec![vec![1i16, 2, 3], vec![4, 5, 6]];
        let mut bytes = build_edf(&["Fz"], 3, &records);
        // Chop off the last few data bytes to truncate the data section.
        bytes.truncate(bytes.len() - 5);
        let path = write_tempfile("trunc", &bytes);

        let result = read_edf(&path);
        let _ = std::fs::remove_file(&path);
        assert!(result.is_err(), "truncated data must return io::Error");
    }

    #[test]
    fn short_header_is_error_not_panic() {
        let path = write_tempfile("short", &[b'0'; 10]);
        let result = read_edf(&path);
        let _ = std::fs::remove_file(&path);
        assert!(result.is_err(), "sub-header file must return io::Error");
    }

    #[test]
    fn nonuniform_rates_rejected() {
        // Two real signals with different samples_per_record would yield
        // different fs. build_edf uses one shared samples_per_record, so we
        // hand-build a mismatch here.
        let ns = 2;
        let header_bytes = HEADER_BLOCK + ns * HEADER_BLOCK;
        let mut buf = Vec::new();
        ascii_field(&mut buf, "0", 8);
        ascii_field(&mut buf, "X", 80);
        ascii_field(&mut buf, "R", 80);
        ascii_field(&mut buf, "01.01.26", 8);
        ascii_field(&mut buf, "00.00.00", 8);
        ascii_field(&mut buf, &header_bytes.to_string(), 8);
        ascii_field(&mut buf, "", 44);
        ascii_field(&mut buf, "1", 8); // 1 record
        ascii_field(&mut buf, "1", 8); // duration 1s
        ascii_field(&mut buf, &ns.to_string(), 4);
        ascii_field(&mut buf, "A", 16);
        ascii_field(&mut buf, "B", 16);
        for _ in 0..ns {
            ascii_field(&mut buf, "T", 80);
        }
        for _ in 0..ns {
            ascii_field(&mut buf, "uV", 8);
        }
        for _ in 0..ns {
            ascii_field(&mut buf, "-1", 8);
        }
        for _ in 0..ns {
            ascii_field(&mut buf, "1", 8);
        }
        for _ in 0..ns {
            ascii_field(&mut buf, "-1", 8);
        }
        for _ in 0..ns {
            ascii_field(&mut buf, "1", 8);
        }
        for _ in 0..ns {
            ascii_field(&mut buf, "P", 80);
        }
        ascii_field(&mut buf, "2", 8); // sig0: 2 samples/record -> 2 Hz
        ascii_field(&mut buf, "4", 8); // sig1: 4 samples/record -> 4 Hz
        for _ in 0..ns {
            ascii_field(&mut buf, "", 32);
        }
        // Data: 1 record, sig0 (2 int16) then sig1 (4 int16).
        for s in [1i16, 2] {
            buf.extend_from_slice(&s.to_le_bytes());
        }
        for s in [3i16, 4, 5, 6] {
            buf.extend_from_slice(&s.to_le_bytes());
        }
        let path = write_tempfile("nonuniform", &buf);

        let result = read_edf(&path);
        let _ = std::fs::remove_file(&path);
        assert!(result.is_err(), "non-uniform fs must be rejected");
    }
}
