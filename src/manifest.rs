//! Codec manifest loader (SPEC/OpenECS-v1.0.md §7).
//!
//! A codec manifest (TOML) describes how to invoke a conformant external
//! codec. [`load_codec_manifest`] parses it and [`CodecManifest::into_adapter`]
//! turns it into a runnable [`ExternalCodec`]. This is host-side config: the
//! grading hot path never touches it.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::adapters_external::{
    default_decode_args, default_encode_args, resolve_cmd, ExternalCodec, InputFormat, OutputFormat,
};
use crate::adapters_external::DEFAULT_TIMEOUT_SECS;
use crate::subprocess::SampleDtype;

/// Default `spec_version` when a manifest omits it.
fn default_spec_version() -> String {
    crate::SPEC_VERSION.to_string()
}

/// Default per-invocation timeout (seconds) when a manifest omits it.
fn default_timeout_secs() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

/// A parsed codec manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct CodecManifest {
    /// OpenECS spec version the manifest targets.
    #[serde(default = "default_spec_version")]
    pub spec_version: String,
    /// The codec definition.
    pub codec: CodecSpec,
}

/// The `[codec]` table of a manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct CodecSpec {
    /// Report identifier.
    pub name: String,
    /// Binary / script to invoke.
    pub cmd: String,
    /// The codec author's lossless claim (verified, not trusted).
    pub declared_lossless: bool,
    /// Fixed tokens inserted before the encode/decode subcommand.
    #[serde(default)]
    pub prefix_args: Vec<String>,
    /// Width of the raw decode stream. Default `i32`.
    #[serde(default)]
    pub sample_dtype: SampleDtype,
    /// Encode-input format. Default `edf`.
    #[serde(default)]
    pub input_format: InputFormat,
    /// Decode-output format. Default `raw`.
    #[serde(default)]
    pub output_format: OutputFormat,
    /// Per-invocation timeout in seconds. Default 600.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// Optional explicit encode arg template (else the default contract).
    pub encode_args: Option<Vec<String>>,
    /// Optional explicit decode arg template (else the default contract).
    pub decode_args: Option<Vec<String>>,
    /// Extra environment merged over the `ECS_*` variables.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// An error loading or validating a codec manifest.
#[derive(Debug)]
pub enum ManifestError {
    /// The manifest file could not be read.
    Io(std::io::Error),
    /// The manifest is not valid TOML / has the wrong shape.
    Parse(toml::de::Error),
    /// The manifest's spec major version is not implemented by this grader.
    UnsupportedVersion(String),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Io(e) => write!(f, "reading codec manifest: {e}"),
            ManifestError::Parse(e) => write!(f, "parsing codec manifest: {e}"),
            ManifestError::UnsupportedVersion(v) => write!(
                f,
                "codec manifest spec_version {v:?} has a major this grader (OpenECS {}) does not implement",
                crate::SPEC_VERSION
            ),
        }
    }
}

impl std::error::Error for ManifestError {}

/// Load and validate a codec manifest from a TOML file.
///
/// Refuses (with [`ManifestError::UnsupportedVersion`]) a manifest whose
/// spec **major** differs from this grader's (spec §11).
pub fn load_codec_manifest<P: AsRef<Path>>(path: P) -> Result<CodecManifest, ManifestError> {
    let text = std::fs::read_to_string(path).map_err(ManifestError::Io)?;
    let manifest: CodecManifest = toml::from_str(&text).map_err(ManifestError::Parse)?;
    // Accept this major or older (a newer grader reads older manifests); refuse
    // a newer major it does not implement.
    match crate::spec_major(&manifest.spec_version) {
        Some(m) if m <= crate::SPEC_MAJOR => Ok(manifest),
        _ => Err(ManifestError::UnsupportedVersion(manifest.spec_version)),
    }
}

impl CodecManifest {
    /// Build a runnable [`ExternalCodec`], or `None` when the codec's
    /// command cannot be resolved on this host (so a caller can skip it
    /// cleanly, mirroring the `lml` adapter's resolve-or-skip ethos).
    pub fn into_adapter(&self) -> Option<ExternalCodec> {
        let c = &self.codec;
        let cmd = resolve_cmd(&c.name, &c.cmd)?;
        let encode = c.encode_args.clone().unwrap_or_else(default_encode_args);
        let decode = c.decode_args.clone().unwrap_or_else(default_decode_args);
        let env: Vec<(String, String)> =
            c.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        Some(
            ExternalCodec::new(&c.name, cmd)
                .with_prefix_args(c.prefix_args.clone())
                .with_templates(encode, decode)
                .with_env(env)
                .with_declared_lossless(c.declared_lossless)
                .with_sample_dtype(c.sample_dtype)
                .with_formats(c.input_format, c.output_format)
                .with_timeout(Duration::from_secs(c.timeout_secs)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::Codec; // trait methods name()/declared_lossless()

    const MINIMAL: &str = r#"
        [codec]
        name = "gzip-ext"
        cmd = "python3"
        declared_lossless = true
    "#;

    const FULL: &str = r#"
        spec_version = "1.0"
        [codec]
        name = "neural-lmq"
        cmd = "/nonexistent/codec"
        declared_lossless = false
        prefix_args = ["-m", "neural_lmq.cli"]
        sample_dtype = "i16"
        input_format = "edf"
        output_format = "ecs0"
        timeout_secs = 1800
        encode_args = ["enc", "{input}", "{output}"]
        decode_args = ["dec", "{input}", "{output}"]
        [codec.env]
        CUDA_VISIBLE_DEVICES = "0"
    "#;

    #[test]
    fn parses_minimal_with_defaults() {
        let m: CodecManifest = toml::from_str(MINIMAL).expect("minimal parses");
        assert_eq!(m.spec_version, crate::SPEC_VERSION); // serde default tracks the crate
        assert_eq!(m.codec.name, "gzip-ext");
        assert!(m.codec.declared_lossless);
        assert_eq!(m.codec.sample_dtype, SampleDtype::I32); // default
        assert_eq!(m.codec.input_format, InputFormat::Edf);
        assert_eq!(m.codec.output_format, OutputFormat::Raw);
        assert_eq!(m.codec.timeout_secs, DEFAULT_TIMEOUT_SECS);
        assert!(m.codec.encode_args.is_none());
    }

    #[test]
    fn parses_full() {
        let m: CodecManifest = toml::from_str(FULL).expect("full parses");
        assert_eq!(m.codec.sample_dtype, SampleDtype::I16);
        assert_eq!(m.codec.output_format, OutputFormat::Ecs0);
        assert_eq!(m.codec.timeout_secs, 1800);
        assert_eq!(m.codec.prefix_args, vec!["-m", "neural_lmq.cli"]);
        assert_eq!(m.codec.env.get("CUDA_VISIBLE_DEVICES").map(|s| s.as_str()), Some("0"));
    }

    #[test]
    fn into_adapter_resolves_bare_command() {
        let m: CodecManifest = toml::from_str(MINIMAL).expect("parses");
        let codec = m.into_adapter().expect("bare 'python3' resolves");
        assert_eq!(codec.name(), "gzip-ext");
        assert!(codec.declared_lossless());
    }

    #[test]
    fn into_adapter_none_when_path_missing() {
        // FULL's cmd is an absolute path that does not exist => None.
        let m: CodecManifest = toml::from_str(FULL).expect("parses");
        assert!(m.into_adapter().is_none());
    }

    #[test]
    fn malformed_toml_errors() {
        assert!(toml::from_str::<CodecManifest>("not = [valid").is_err());
        // Missing required [codec] table.
        assert!(toml::from_str::<CodecManifest>("spec_version = \"1.0\"").is_err());
    }

    #[test]
    fn unsupported_major_is_refused() {
        // Round-trip through the file loader to exercise the version gate.
        let dir = crate::subprocess::ScratchDir::new("manifest_ver").expect("scratch");
        let p = dir.join("c.toml");
        std::fs::write(
            &p,
            "spec_version = \"2.0\"\n[codec]\nname = \"x\"\ncmd = \"python3\"\ndeclared_lossless = true\n",
        )
        .expect("write");
        match load_codec_manifest(&p) {
            Err(ManifestError::UnsupportedVersion(v)) => assert_eq!(v, "2.0"),
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
    }
}
