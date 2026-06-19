//! `openecs` — CLI for the OpenECS universal EEG-compression benchmark
//! (OpenECS v1.0; see `SPEC/OpenECS-v1.0.md`).
//!
//! Subcommands (run `openecs <cmd> --help` for each):
//!
//! - `grade` — grade ONE codec over a corpus / EDF / synthetic fixture.
//! - `bench` — grade a codec **and baselines**, ranked, with confidence
//!   intervals + significance, a progress bar, and optional HTML report.
//! - `verify-corpus` — check a corpus manifest's SHA-256 + shape pins.
//! - `emit-corpus-manifest` — hash a directory of EDFs into a pinned manifest.
//!
//! Legacy form (preserved): `openecs <store|gzip|quantize> [FILE.edf]` grades
//! the built-in fixture or an EDF and prints a single report.
//!
//! Exit codes: 0 pass · 1 below-floor · 2 unknown codec · 3 EDF read error ·
//! 4 corpus integrity/shape failure · 5 manifest load/parse error.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};

use open_eeg_codec_standard::adapter::{deserialize, serialize, Codec, Gzip, Store};
use open_eeg_codec_standard::corpus::{self, sha256_hex, CorpusManifest};
use open_eeg_codec_standard::report::{CodecIdentity, CorpusIdentity, EcsReport, EcsSubmission};
use open_eeg_codec_standard::subprocess::write_edf_bytes;
use open_eeg_codec_standard::{charts, edf, harness, manifest, report_html, stats, term};

// ─────────────────────────────── demo codec ────────────────────────────────

/// A deliberately-lossy demo codec (÷STEP / ×STEP). Not bit-exact — exercises
/// the lossy battery. Ships in the CLI as a demonstration.
struct Quantize {
    step: i64,
}

impl Codec for Quantize {
    fn name(&self) -> &str {
        "quantize"
    }
    fn declared_lossless(&self) -> bool {
        false
    }
    fn encode(&self, signal: &[Vec<i64>], _fs: f64) -> Vec<u8> {
        let q: Vec<Vec<i64>> = signal
            .iter()
            .map(|chan| chan.iter().map(|&s| s / self.step).collect())
            .collect();
        serialize(&q)
    }
    fn decode(&self, blob: &[u8]) -> Vec<Vec<i64>> {
        deserialize(blob)
            .into_iter()
            .map(|chan| chan.into_iter().map(|s| s * self.step).collect())
            .collect()
    }
}

/// A deterministic synthetic multichannel EEG-like signal.
fn synthetic_signal(n_chan: usize, n: usize, fs: f64) -> Vec<Vec<i64>> {
    use std::f64::consts::PI;
    (0..n_chan)
        .map(|c| {
            let amp = 1.0 + 0.3 * c as f64;
            (0..n)
                .map(|i| {
                    let t = i as f64 / fs;
                    let v = amp
                        * (40.0
                            + 120.0 * (2.0 * PI * 2.0 * t).sin()
                            + 80.0 * (2.0 * PI * 6.0 * t).sin()
                            + 60.0 * (2.0 * PI * 10.0 * t).sin()
                            + 30.0 * (2.0 * PI * 20.0 * t).sin()
                            + 15.0 * (2.0 * PI * 40.0 * t).sin());
                    v.round() as i64
                })
                .collect()
        })
        .collect()
}

// ──────────────────────────────── clap model ───────────────────────────────

#[derive(Parser)]
#[command(
    name = "openecs",
    version,
    about = "OpenECS — the universal, vendor-neutral EEG-compression benchmark",
    long_about = "Grade any EEG codec (any language, via the file-based CLI contract) over a \
                  hash-pinned corpus and get a single comparable verdict. See SPEC/OpenECS-v1.0.md.\n\n\
                  Legacy form (preserved): openecs <store|gzip|quantize> [FILE.edf]"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Grade ONE codec over a corpus (or a single EDF / the synthetic fixture).
    Grade(GradeArgs),
    /// Grade a codec AND baselines — ranked, with CIs, significance, a progress bar.
    Bench(BenchArgs),
    /// Verify a corpus manifest's SHA-256 + shape pins (no grading).
    VerifyCorpus(CorpusArg),
    /// Walk a directory of *.edf files and print a pinned corpus manifest.
    EmitCorpusManifest(EmitArgs),
}

#[derive(Args)]
struct GradeArgs {
    /// Codec manifest (TOML) for an external codec (any language).
    #[arg(long, value_name = "C.toml")]
    codec_manifest: Option<PathBuf>,
    /// Built-in codec when no manifest is given.
    #[arg(long, default_value = "store", value_name = "store|gzip|quantize")]
    codec: String,
    /// Corpus manifest (TOML) to grade over (hash-pinned).
    #[arg(long, value_name = "K.toml")]
    corpus_manifest: Option<PathBuf>,
    /// Grade a single EDF file instead of a corpus.
    #[arg(long, value_name = "FILE.edf")]
    edf: Option<PathBuf>,
    /// Write the results submission JSON here.
    #[arg(long, value_name = "sub.json")]
    out: Option<PathBuf>,
    /// Write a self-contained HTML report here.
    #[arg(long, value_name = "report.html")]
    report: Option<PathBuf>,
    /// Dump paired orig_/recon_ EDFs here (for the task-concordance tool).
    #[arg(long, value_name = "DIR")]
    dump_recon: Option<PathBuf>,
    /// Show ASCII charts in the terminal.
    #[arg(long)]
    charts: bool,
    /// Disable colored output.
    #[arg(long)]
    no_color: bool,
    /// Timed round trips per file for the median-throughput measurement.
    #[arg(long, default_value_t = 1, value_name = "N")]
    repeat: usize,
}

#[derive(Args)]
struct BenchArgs {
    /// Codec manifest (TOML) for the codec under test (any language).
    #[arg(long, value_name = "C.toml")]
    codec_manifest: Option<PathBuf>,
    /// Built-in codec under test when no manifest is given.
    #[arg(long, value_name = "store|gzip|quantize")]
    codec: Option<String>,
    /// Corpus manifest (TOML) to benchmark over (hash-pinned). Required.
    #[arg(long, value_name = "K.toml")]
    corpus_manifest: PathBuf,
    /// Comma-separated built-in baselines to grade alongside the codec.
    #[arg(long, value_delimiter = ',', default_value = "store,gzip", value_name = "a,b")]
    baselines: Vec<String>,
    /// Timed round trips per file for the median-throughput measurement.
    #[arg(long, default_value_t = 3, value_name = "N")]
    repeat: usize,
    /// Write the codec-under-test submission JSON here.
    #[arg(long, value_name = "sub.json")]
    out: Option<PathBuf>,
    /// Write a self-contained HTML report (all codecs) here.
    #[arg(long, value_name = "report.html")]
    report: Option<PathBuf>,
    /// Show ASCII charts in the terminal.
    #[arg(long)]
    charts: bool,
    /// Disable colored output.
    #[arg(long)]
    no_color: bool,
}

#[derive(Args)]
struct CorpusArg {
    /// Corpus manifest (TOML) to verify.
    #[arg(long, value_name = "K.toml")]
    corpus_manifest: PathBuf,
}

#[derive(Args)]
struct EmitArgs {
    /// Root directory to walk for *.edf files.
    #[arg(long, value_name = "DIR")]
    root: PathBuf,
    /// Corpus name to stamp.
    #[arg(long, default_value = "corpus")]
    name: String,
    /// Corpus version to stamp.
    #[arg(long, default_value = "1.0.0")]
    version: String,
}

// ─────────────────────────────── dispatch ──────────────────────────────────

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    const SUBS: &[&str] = &[
        "grade",
        "bench",
        "verify-corpus",
        "emit-corpus-manifest",
        "help",
        "-h",
        "--help",
        "-V",
        "--version",
    ];
    let is_clap = args.get(1).map(|s| SUBS.contains(&s.as_str())).unwrap_or(false);
    if !is_clap {
        // Legacy `openecs <codec> [file.edf]` (or no args -> synthetic).
        return cmd_legacy(&args[1..]);
    }
    match Cli::parse().cmd {
        Cmd::Grade(a) => cmd_grade(a),
        Cmd::Bench(a) => cmd_bench(a),
        Cmd::VerifyCorpus(a) => cmd_verify_corpus(&a.corpus_manifest),
        Cmd::EmitCorpusManifest(a) => cmd_emit_manifest(&a),
    }
}

/// True iff stdout is an interactive terminal (progress bars / spinners on).
fn interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

// ─────────────────────────────── codec build ───────────────────────────────

/// Build a built-in codec by name (Sync, for the parallel grader).
fn build_builtin(name: &str) -> Option<(Box<dyn Codec + Sync>, CodecIdentity)> {
    let codec: Box<dyn Codec + Sync> = match name {
        "store" => Box::new(Store),
        "gzip" => Box::new(Gzip),
        "quantize" => Box::new(Quantize { step: 8 }),
        _ => return None,
    };
    Some((codec, CodecIdentity { name: name.to_string(), manifest_sha256: None }))
}

/// Resolve the codec under test: a manifest-defined external codec takes
/// precedence over a built-in name.
fn resolve_target(
    codec_manifest: Option<&Path>,
    builtin: &str,
) -> Result<(Box<dyn Codec + Sync>, CodecIdentity), ExitCode> {
    if let Some(path) = codec_manifest {
        let m = match manifest::load_codec_manifest(path) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("error: {e}");
                return Err(ExitCode::from(5));
            }
        };
        let codec = match m.into_adapter() {
            Some(c) => c,
            None => {
                eprintln!(
                    "error: codec '{}' command could not be resolved (set $ECS_CODEC_<NAME>_BIN or fix `cmd`)",
                    m.codec.name
                );
                return Err(ExitCode::from(5));
            }
        };
        let sha = std::fs::read(path).map(|b| sha256_hex(&b)).ok();
        let id = CodecIdentity { name: codec.name().to_string(), manifest_sha256: sha };
        return Ok((Box::new(codec), id));
    }
    build_builtin(builtin).ok_or_else(|| {
        eprintln!("unknown codec '{builtin}'; valid: store | gzip | quantize, or --codec-manifest");
        ExitCode::from(2)
    })
}

// ─────────────────────────────── corpus grade ──────────────────────────────

/// Grade a codec over a corpus manifest with a labelled progress bar.
fn grade_corpus_with_progress(
    codec: &(dyn Codec + Sync),
    manifest: &CorpusManifest,
    base: &Path,
    repeats: usize,
    label: &str,
    show: bool,
) -> Result<(Vec<EcsReport>, harness::CorpusSummary), corpus::CorpusError> {
    let pb = if show {
        let pb = ProgressBar::new(manifest.file.len() as u64);
        pb.set_style(
            ProgressStyle::with_template("{prefix:>12} [{bar:28.cyan/blue}] {pos:>4}/{len} {eta:>5}")
                .unwrap()
                .progress_chars("█▉ "),
        );
        pb.set_prefix(label.to_string());
        pb
    } else {
        ProgressBar::hidden()
    };
    let res = corpus::grade_manifest_parallel(manifest, base, codec, repeats, || pb.inc(1));
    pb.finish_and_clear();
    res
}

fn manifest_base(path: &Path) -> &Path {
    path.parent().unwrap_or_else(|| Path::new("."))
}

// ─────────────────────────────────── grade ─────────────────────────────────

fn cmd_grade(a: GradeArgs) -> ExitCode {
    let color = term::colors_on() && !a.no_color;
    let (codec, codec_id) = match resolve_target(a.codec_manifest.as_deref(), &a.codec) {
        Ok(v) => v,
        Err(code) => return code,
    };

    // Corpus path.
    if let Some(corpus_path) = &a.corpus_manifest {
        let manifest = match corpus::load_corpus_manifest(corpus_path) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(5);
            }
        };
        let base = manifest_base(corpus_path);
        let (reports, summary) = match grade_corpus_with_progress(
            codec.as_ref(),
            &manifest,
            base,
            a.repeat,
            &codec_id.name,
            interactive(),
        ) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error: corpus integrity/shape: {e}");
                return ExitCode::from(4);
            }
        };
        println!("{}", term::render_corpus_summary(&summary, color));
        let ranked: Vec<&EcsReport> = reports.iter().collect();
        println!("\n{}", term::render_leaderboard(&ranked, color));
        if a.charts {
            let pts: Vec<(f32, f32)> =
                reports.iter().map(|r| (r.cr as f32, r.prd as f32)).collect();
            println!("{}", charts::ascii_rd_scatter(&pts));
        }
        if let Some(dir) = &a.dump_recon {
            // Reload signals (verify) just for the recon dump.
            if let Ok(files) = corpus::verify_and_load(&manifest, base) {
                if let Err(e) = dump_recon(codec.as_ref(), &files, &manifest.name, dir) {
                    eprintln!("warning: --dump-recon: {e}");
                } else {
                    println!("dumped originals + reconstructions to {}", dir.display());
                }
            }
        }
        let submission = EcsSubmission::new(
            codec_id,
            CorpusIdentity { name: manifest.name.clone(), version: manifest.version.clone() },
            reports,
            summary.clone(),
        );
        write_outputs(&[submission], a.out.as_deref(), a.report.as_deref());
        return exit_for_grade(summary.worst_grade);
    }

    // Single EDF or synthetic fixture.
    let (signal, fs, dataset) = match &a.edf {
        Some(path) => match edf::read_edf(path) {
            Ok(e) => (e.channels, e.fs, path.display().to_string()),
            Err(err) => {
                eprintln!("error: failed to read EDF '{}': {err}", path.display());
                return ExitCode::from(3);
            }
        },
        None => (synthetic_signal(4, 512, 256.0), 256.0, "(synthetic)".to_string()),
    };
    let mut report = harness::run_measured(codec.as_ref(), &signal, fs, a.repeat);
    report.dataset = dataset.clone();
    println!("{}", term::render_report(&report, color));
    if a.charts {
        println!("\n{}", charts::ascii_per_band(&report, 24));
    }
    if a.out.is_some() || a.report.is_some() {
        let (reports, summary) = harness::run_corpus(codec.as_ref(), &[(signal, fs)]);
        let mut reports = reports;
        reports.iter_mut().for_each(|r| r.dataset = dataset.clone());
        let submission = EcsSubmission::new(
            codec_id,
            CorpusIdentity { name: dataset, version: "n/a".to_string() },
            reports,
            summary,
        );
        write_outputs(&[submission], a.out.as_deref(), a.report.as_deref());
    }
    exit_for_grade(report.grade)
}

// ─────────────────────────────────── bench ─────────────────────────────────

fn cmd_bench(a: BenchArgs) -> ExitCode {
    let color = term::colors_on() && !a.no_color;
    let manifest = match corpus::load_corpus_manifest(&a.corpus_manifest) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(5);
        }
    };
    let base = manifest_base(&a.corpus_manifest);

    let (target, target_id) =
        match resolve_target(a.codec_manifest.as_deref(), a.codec.as_deref().unwrap_or("store")) {
            Ok(v) => v,
            Err(code) => return code,
        };

    // Codec set: the target first, then the requested baselines (de-duped).
    let mut codecs: Vec<(Box<dyn Codec + Sync>, CodecIdentity)> = vec![(target, target_id.clone())];
    for name in &a.baselines {
        if *name == target_id.name {
            continue;
        }
        if let Some(c) = build_builtin(name) {
            codecs.push(c);
        } else {
            eprintln!("warning: skipping unknown baseline '{name}'");
        }
    }

    println!(
        "OpenECS bench — {} codecs over {} v{} ({} files, repeat={})\n",
        codecs.len(),
        manifest.name,
        manifest.version,
        manifest.file.len(),
        a.repeat
    );

    let mut subs: Vec<EcsSubmission> = Vec::new();
    for (codec, id) in &codecs {
        let (reports, summary) = match grade_corpus_with_progress(
            codec.as_ref(),
            &manifest,
            base,
            a.repeat,
            &id.name,
            interactive(),
        ) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error: corpus integrity/shape ({}): {e}", id.name);
                return ExitCode::from(4);
            }
        };
        subs.push(EcsSubmission::new(
            id.clone(),
            CorpusIdentity { name: manifest.name.clone(), version: manifest.version.clone() },
            reports,
            summary,
        ));
    }

    print!("{}", render_bench_table(&subs, &target_id.name, color));

    if a.charts {
        let series: Vec<(String, Vec<(f64, f64)>)> = subs
            .iter()
            .map(|s| (s.codec.name.clone(), s.reports.iter().map(|r| (r.cr, r.prd)).collect()))
            .collect();
        let pts: Vec<(f32, f32)> =
            series.iter().flat_map(|(_, p)| p.iter().map(|(x, y)| (*x as f32, *y as f32))).collect();
        println!("\n{}", charts::ascii_rd_scatter(&pts));
    }

    // Submission JSON = the codec under test (subs[0]); HTML = all codecs.
    let target_sub = subs.first().cloned().into_iter().collect::<Vec<_>>();
    write_outputs(&target_sub, a.out.as_deref(), None);
    if let Some(report_path) = &a.report {
        let html = report_html::render(&subs);
        if let Err(e) = std::fs::write(report_path, html) {
            eprintln!("error: writing report '{}': {e}", report_path.display());
        } else {
            println!("wrote HTML report: {}", report_path.display());
        }
    }

    exit_for_grade(subs.first().map(|s| s.summary.worst_grade).unwrap_or('\0'))
}

/// Render the bench comparison table: one row per codec, ranked worst-grade
/// first then pooled CR, with a 95% CI on mean R and a sign-test p-value for
/// the codec under test versus the strongest baseline.
fn render_bench_table(subs: &[EcsSubmission], target: &str, color: bool) -> String {
    // Rank order.
    let mut order: Vec<usize> = (0..subs.len()).collect();
    order.sort_by(|&a, &b| {
        let (sa, sb) = (&subs[a].summary, &subs[b].summary);
        grade_rank(sa.worst_grade)
            .cmp(&grade_rank(sb.worst_grade))
            .then(sb.mean_cr.partial_cmp(&sa.mean_cr).unwrap_or(std::cmp::Ordering::Equal))
    });

    // Per-file R series per codec (for CI + significance).
    let r_series: Vec<Vec<f64>> =
        subs.iter().map(|s| s.reports.iter().map(|r| r.r).collect()).collect();
    // Strongest baseline = best-ranked non-target codec.
    let best_baseline = order.iter().copied().find(|&i| subs[i].codec.name != target);

    let mut s = String::new();
    let bold = |t: &str| if color { console::style(t).bold().force_styling(true).to_string() } else { t.to_string() };
    s.push_str(&bold(
        "  rank  codec                  grade   pooled CR   mean R (95% CI)             PRD%   p vs base\n",
    ));
    s.push_str("  ────────────────────────────────────────────────────────────────────────────────────────\n");
    for (rank, &i) in order.iter().enumerate() {
        let sub = &subs[i];
        let sm = &sub.summary;
        let ci = stats::bootstrap_ci(&r_series[i], 0.95, 2000, 0x1c5_5eed);
        let pval = match best_baseline {
            Some(b) if i != b => format!("{:.4}", stats::sign_test(&r_series[i], &r_series[b])),
            _ => "  —  ".to_string(),
        };
        let name_cell = if sub.codec.name == target {
            format!("{}*", sub.codec.name)
        } else {
            sub.codec.name.clone()
        };
        // ANSI-aware padding for the colored grade cell (plain cells use
        // format-width specifiers).
        let grade_str = term::grade_short(sm.worst_grade, color);
        let grade_cell = console::pad_str(&grade_str, 6, console::Alignment::Center, None);
        s.push_str(&format!(
            "  {:>3}   {:<22} {}  {:>8.2}:1   {:.4} [{:.4},{:.4}]   {:>5.2}   {:>8}\n",
            rank + 1,
            truncate(&name_cell, 22),
            grade_cell,
            sm.mean_cr,
            ci.mean,
            ci.lo,
            ci.hi,
            sm.mean_prd,
            pval,
        ));
    }
    s.push_str("\n  * codec under test · p = paired sign-test on per-file R vs the strongest baseline\n");
    s
}

// ──────────────────────────── verify / emit ────────────────────────────────

fn cmd_verify_corpus(path: &Path) -> ExitCode {
    let manifest = match corpus::load_corpus_manifest(path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(5);
        }
    };
    let base = manifest_base(path);
    println!("verifying corpus {} v{} ({} files)", manifest.name, manifest.version, manifest.file.len());
    match corpus::verify_and_load(&manifest, base) {
        Ok(files) => {
            for (entry, (sig, _)) in manifest.file.iter().zip(&files) {
                println!(
                    "  PASS  {} ({} ch x {} samp)",
                    entry.path,
                    sig.len(),
                    sig.first().map(|c| c.len()).unwrap_or(0)
                );
            }
            println!("\nOK: all {} files verified (sha256 + shape).", files.len());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("  FAIL  {e}");
            ExitCode::from(4)
        }
    }
}

fn cmd_emit_manifest(a: &EmitArgs) -> ExitCode {
    let mut edfs = Vec::new();
    if let Err(e) = collect_edfs(&a.root, &mut edfs) {
        eprintln!("error: walking '{}': {e}", a.root.display());
        return ExitCode::from(3);
    }
    edfs.sort();

    println!("spec_version = \"{}\"", open_eeg_codec_standard::SPEC_VERSION);
    println!("name = \"{}\"", a.name);
    println!("version = \"{}\"", a.version);
    println!();

    let mut emitted = 0usize;
    for path in &edfs {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("warning: skip {} ({e})", path.display());
                continue;
            }
        };
        let sig = match edf::read_edf(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("warning: skip {} (not readable EDF: {e})", path.display());
                continue;
            }
        };
        let n_chan = sig.channels.len();
        let n_samples = sig.channels.first().map(|c| c.len()).unwrap_or(0);
        if n_chan == 0 || !sig.channels.iter().all(|c| c.len() == n_samples) {
            eprintln!("warning: skip {} (empty or ragged channels)", path.display());
            continue;
        }
        let rel = path.strip_prefix(&a.root).unwrap_or(path);
        println!("[[file]]");
        println!("path = \"{}\"", rel.display());
        println!("sha256 = \"{}\"", sha256_hex(&bytes));
        // `{:?}` renders a whole number with a decimal (256.0, not 256).
        println!("fs = {:?}", sig.fs);
        println!("n_chan = {n_chan}");
        println!("n_samples = {n_samples}");
        println!();
        emitted += 1;
    }
    eprintln!("emitted {emitted} file entries from {}", a.root.display());
    if emitted == 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

// ──────────────────────────────── legacy ───────────────────────────────────

fn cmd_legacy(args: &[String]) -> ExitCode {
    let color = term::colors_on();
    let codec_name = args.first().cloned().unwrap_or_else(|| "store".to_string());
    let file_arg = args.get(1);

    let (signal, fs, dataset) = match file_arg {
        Some(path) => match edf::read_edf(path) {
            Ok(e) => {
                let ds = path.clone();
                (e.channels, e.fs, ds)
            }
            Err(err) => {
                eprintln!("error: failed to read EDF file '{path}': {err}");
                return ExitCode::from(3);
            }
        },
        None => (synthetic_signal(4, 512, 256.0), 256.0, "(synthetic)".to_string()),
    };

    let codec: Box<dyn Codec> = match codec_name.as_str() {
        "store" => Box::new(Store),
        "gzip" => Box::new(Gzip),
        "quantize" => Box::new(Quantize { step: 8 }),
        other => {
            eprintln!("unknown codec '{other}'; valid: store | gzip | quantize");
            eprintln!("(for the full CLI run `openecs --help`)");
            return ExitCode::from(2);
        }
    };

    let mut report = harness::run(codec.as_ref(), &signal, fs);
    report.dataset = dataset;
    println!("{}", term::render_report(&report, color));
    exit_for_grade(report.grade)
}

// ──────────────────────────────── helpers ──────────────────────────────────

fn exit_for_grade(grade: char) -> ExitCode {
    if grade != '\0' {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn grade_rank(g: char) -> u8 {
    match g {
        'L' => 0,
        'N' => 1,
        'C' => 2,
        'M' => 3,
        'A' => 4,
        _ => 5,
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Write a submission JSON (`out`) and/or an HTML report (`report`).
fn write_outputs(subs: &[EcsSubmission], out: Option<&Path>, report: Option<&Path>) {
    if let (Some(path), Some(sub)) = (out, subs.first()) {
        match std::fs::write(path, sub.to_json()) {
            Ok(()) => println!("wrote submission: {}", path.display()),
            Err(e) => eprintln!("error: writing submission '{}': {e}", path.display()),
        }
    }
    if let Some(path) = report {
        match std::fs::write(path, report_html::render(subs)) {
            Ok(()) => println!("wrote HTML report: {}", path.display()),
            Err(e) => eprintln!("error: writing report '{}': {e}", path.display()),
        }
    }
}

/// Write paired `orig_{i}.edf` / `recon_{i}.edf` for each corpus file.
fn dump_recon(
    codec: &dyn Codec,
    files: &[(Vec<Vec<i64>>, f64)],
    dataset: &str,
    dir: &Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for (i, (signal, fs)) in files.iter().enumerate() {
        let blob = codec.encode(signal, *fs);
        let recon = codec.decode(&blob);
        match (write_edf_bytes(signal, *fs), write_edf_bytes(&recon, *fs)) {
            (Some(o), Some(r)) => {
                std::fs::write(dir.join(format!("orig_{i}.edf")), o)?;
                std::fs::write(dir.join(format!("recon_{i}.edf")), r)?;
            }
            _ => eprintln!(
                "  skip recon dump for {dataset} file {i}: not EDF-expressible (ragged/out-of-range)"
            ),
        }
    }
    Ok(())
}

/// Recursively collect `*.edf` files under `dir`.
fn collect_edfs(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_edfs(&path, out)?;
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("edf"))
            .unwrap_or(false)
        {
            out.push(path);
        }
    }
    Ok(())
}
