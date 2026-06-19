//! Terminal rendering — colored, boxed, chart-y read-outs (host-side DX).
//!
//! This is the "nice read-out on the terminal" layer. It consumes the plain
//! data types from [`crate::report`] / [`crate::harness`] and renders them
//! with color (via the `console` crate, which auto-detects a tty and honours
//! `NO_COLOR`), unicode frames, grade badges, sparklines, and bar charts. The
//! data modules stay free of presentation concerns; all of that lives here.
//!
//! Every renderer takes an explicit `color: bool` so output is deterministic
//! and testable (pass `false` for plain text). [`colors_on`] is the sensible
//! default a CLI computes once.

use console::{measure_text_width, style};

use crate::harness::CorpusSummary;
use crate::report::EcsReport;

/// Sparkline block ramp, lowest → highest.
const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Whether color should be used by default on this host (tty + not `NO_COLOR`).
pub fn colors_on() -> bool {
    console::colors_enabled()
}

/// Colorize `s` with a `console` style closure when `color`, else return it
/// plain. `force_styling(true)` makes the `color` flag authoritative — the
/// caller (the CLI) already decided based on the tty / `NO_COLOR`, so we must
/// not let `console`'s own auto-detection override an explicit `true` (it
/// would suppress ANSI when stdout is not a tty, e.g. under `cargo test`).
fn paint(s: &str, color: bool, f: impl Fn(console::StyledObject<&str>) -> console::StyledObject<&str>) -> String {
    if color {
        f(style(s).force_styling(true)).to_string()
    } else {
        s.to_string()
    }
}

/// The display color for a tier grade (used for badges + leaderboard rows).
fn grade_paint(grade: char, color: bool, text: &str) -> String {
    if !color {
        return text.to_string();
    }
    let st = style(text).force_styling(true).bold();
    match grade {
        'L' => st.green(),
        'N' => st.green().bright(),
        'C' => st.cyan(),
        'M' => st.yellow(),
        'A' => st.magenta(),
        _ => st.red(),
    }
    .to_string()
}

/// A one-token grade badge, e.g. `ECS-L` / `— below floor`, colored by tier.
pub fn grade_badge(grade: char, color: bool) -> String {
    let label = match grade {
        '\0' => "— below floor".to_string(),
        g => format!("ECS-{g}"),
    };
    grade_paint(grade, color, &label)
}

/// A compact, fixed-width-friendly grade label (`ECS-L` / `—`) colored by
/// tier — for table cells where `— below floor` would break the column.
pub fn grade_short(grade: char, color: bool) -> String {
    let label = if grade == '\0' {
        "—".to_string()
    } else {
        format!("ECS-{grade}")
    };
    grade_paint(grade, color, &label)
}

/// A unicode sparkline of `vals` mapped onto their own min–max range.
///
/// A flat series renders as a mid-level line. Non-finite values are treated
/// as the series minimum so the call never panics.
pub fn sparkline(vals: &[f64]) -> String {
    if vals.is_empty() {
        return String::new();
    }
    let finite: Vec<f64> = vals.iter().map(|v| if v.is_finite() { *v } else { f64::NEG_INFINITY }).collect();
    let lo = finite.iter().cloned().filter(|v| v.is_finite()).fold(f64::INFINITY, f64::min);
    let hi = finite.iter().cloned().filter(|v| v.is_finite()).fold(f64::NEG_INFINITY, f64::max);
    if !lo.is_finite() || !hi.is_finite() {
        return BLOCKS[0].to_string().repeat(vals.len());
    }
    let span = hi - lo;
    finite
        .iter()
        .map(|&v| {
            if span <= 0.0 || !v.is_finite() {
                BLOCKS[BLOCKS.len() / 2]
            } else {
                let idx = (((v - lo) / span) * (BLOCKS.len() - 1) as f64).round() as usize;
                BLOCKS[idx.min(BLOCKS.len() - 1)]
            }
        })
        .collect()
}

/// A horizontal bar for `frac` ∈ [0,1] of the given character width.
pub fn bar(frac: f64, width: usize) -> String {
    let frac = frac.clamp(0.0, 1.0);
    let filled = (frac * width as f64).round() as usize;
    let filled = filled.min(width);
    let mut s = String::with_capacity(width * 3);
    for _ in 0..filled {
        s.push('█');
    }
    for _ in filled..width {
        s.push('░');
    }
    s
}

/// Frame `body` lines in a rounded unicode box under `title`. Padding uses
/// `console::measure_text_width` so embedded ANSI color codes don't skew the
/// alignment.
pub fn boxed(title: &str, body: &[String], color: bool) -> String {
    let title_w = measure_text_width(title);
    let content_w = body.iter().map(|l| measure_text_width(l)).max().unwrap_or(0);
    let inner = content_w.max(title_w).max(20);

    let titled = paint(title, color, |s| s.bold());
    let mut out = String::new();
    // Top border carries the title: ╭─ title ──…─╮
    out.push_str("╭─ ");
    out.push_str(&titled);
    out.push(' ');
    let used = 3 + title_w + 1;
    let total = inner + 4; // "╭─ " (3) + content + trailing pad to align
    for _ in used..total.max(used) {
        out.push('─');
    }
    out.push_str("╮\n");
    for line in body {
        let pad = inner - measure_text_width(line);
        out.push_str("│ ");
        out.push_str(line);
        for _ in 0..pad {
            out.push(' ');
        }
        out.push_str(" │\n");
    }
    out.push('╰');
    for _ in 0..inner + 2 {
        out.push('─');
    }
    out.push('╯');
    out
}

/// Render one report as a colored, boxed read-out with a per-band R sparkline.
pub fn render_report(rep: &EcsReport, color: bool) -> String {
    let badge = grade_badge(rep.grade, color);
    let compliant = if rep.passed() {
        paint("COMPLIANT", color, |s| s.green().bold())
    } else {
        paint("NON-COMPLIANT", color, |s| s.red().bold())
    };
    let dim = |s: &str| paint(s, color, |x| x.dim());

    let mut body = Vec::new();
    body.push(format!("{}   {}   {}", dim("grade"), badge, compliant));
    body.push(format!(
        "{}  {:>9.2}:1    {}  {:>7.3}%    {}  {:>7.4}",
        dim("CR  "),
        rep.cr,
        dim("PRD"),
        rep.prd,
        dim("R"),
        rep.r
    ));
    body.push(format!(
        "{}  {:>7.2} dB   {}  {:>9.2} MiB/s   {}  {:>6.2} MiB",
        dim("SNR "),
        rep.snr_db,
        dim("thrpt"),
        rep.throughput_mibs,
        dim("peak"),
        rep.peak_bytes as f64 / (1024.0 * 1024.0),
    ));
    // Per-band R sparkline (named bands only, in canonical order).
    let band_r: Vec<f64> = rep.per_band.iter().map(|b| b.r).collect();
    if !band_r.is_empty() {
        let names: String = rep
            .per_band
            .iter()
            .map(|b| b.band.chars().next().unwrap_or('?'))
            .collect();
        body.push(format!("{}  {}  {}", dim("band R"), sparkline(&band_r), dim(&names)));
    }
    if !rep.violations.is_empty() {
        body.push(dim("to climb a tier:"));
        for v in &rep.violations {
            body.push(format!("  {} {}", paint("·", color, |s| s.red()), v));
        }
    }

    let title = format!(
        "OpenECS report · {} · {} ({} file{})",
        rep.codec,
        rep.dataset,
        rep.n_files,
        if rep.n_files == 1 { "" } else { "s" }
    );
    boxed(&title, &body, color)
}

/// Render a ranked leaderboard table (grade-first ordering already applied by
/// the caller via [`crate::report::leaderboard`] data, but this colors it).
pub fn render_leaderboard(reports: &[&EcsReport], color: bool) -> String {
    let dim = |s: &str| paint(s, color, |x| x.dim());
    let mut s = String::new();
    s.push_str(&paint("OpenECS leaderboard (best first)\n", color, |x| x.bold()));
    s.push_str(&dim("  #  codec                  grade      CR     PRD%       R       QS\n"));
    s.push_str(&dim("  ─────────────────────────────────────────────────────────────────\n"));
    if reports.is_empty() {
        s.push_str("  (no codecs)\n");
        return s;
    }
    for (i, r) in reports.iter().enumerate() {
        let g = if r.grade == '\0' {
            grade_paint('\0', color, "  —  ")
        } else {
            grade_paint(r.grade, color, &format!("ECS-{}", r.grade))
        };
        s.push_str(&format!(
            "  {:>2} {:<22} {:>6} {:>7.2} {:>7.3} {:>8.4} {:>8.3}\n",
            i + 1,
            truncate(&r.codec, 22),
            g,
            r.cr,
            r.prd,
            r.r,
            r.qs,
        ));
    }
    s
}

/// Render the corpus roll-up line block.
pub fn render_corpus_summary(summary: &CorpusSummary, color: bool) -> String {
    let dim = |s: &str| paint(s, color, |x| x.dim());
    let worst = grade_badge(summary.worst_grade, color);
    format!(
        "{}  {}\n{}  {:.2}:1   {}  {:.4}   {}  {:.3}%   {}  {}",
        dim("worst grade"),
        worst,
        dim("pooled CR"),
        summary.mean_cr,
        dim("mean R"),
        summary.mean_r,
        dim("mean PRD"),
        summary.mean_prd,
        dim("bit-exact"),
        summary.all_bit_exact,
    )
}

/// Truncate a string to `n` display columns (used for table cells).
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::BandResult;

    fn rep() -> EcsReport {
        EcsReport {
            spec_version: "1.0".into(),
            codec: "store".into(),
            dataset: "ecs-smoke".into(),
            n_files: 3,
            bit_exact: true,
            grade: 'L',
            cr: 24.57,
            prd: 0.0,
            prdn: 0.0,
            r: 1.0,
            snr_db: 120.0,
            qs: 24.57,
            per_band: vec![
                BandResult::new("delta", 1.0, 0.0, 120.0),
                BandResult::new("gamma", 0.9, 5.0, 30.0),
            ],
            throughput_mibs: 88.5,
            peak_bytes: 2 * 1024 * 1024,
            violations: vec![],
        }
    }

    #[test]
    fn sparkline_maps_range() {
        let s = sparkline(&[0.0, 1.0, 2.0, 3.0]);
        assert_eq!(s.chars().count(), 4);
        assert_eq!(s.chars().next(), Some('▁'));
        assert_eq!(s.chars().last(), Some('█'));
        // Flat series -> mid blocks, no panic.
        assert_eq!(sparkline(&[5.0, 5.0]).chars().count(), 2);
        assert!(sparkline(&[]).is_empty());
    }

    #[test]
    fn bar_fills_proportionally() {
        assert_eq!(bar(0.0, 4), "░░░░");
        assert_eq!(bar(1.0, 4), "████");
        assert_eq!(bar(0.5, 4), "██░░");
        assert_eq!(bar(2.0, 4), "████"); // clamped
    }

    #[test]
    fn plain_report_has_no_ansi() {
        let out = render_report(&rep(), false);
        assert!(!out.contains('\u{1b}'), "color=false must emit no ANSI escapes");
        assert!(out.contains("ECS-L"));
        assert!(out.contains("store"));
        assert!(out.contains("ecs-smoke"));
        // Boxed frame present.
        assert!(out.contains('╭') && out.contains('╰'));
    }

    #[test]
    fn colored_report_has_ansi() {
        let out = render_report(&rep(), true);
        assert!(out.contains('\u{1b}'), "color=true should emit ANSI escapes");
    }

    #[test]
    fn leaderboard_plain_lists_rows() {
        let r = rep();
        let out = render_leaderboard(&[&r], false);
        assert!(out.contains("leaderboard"));
        assert!(out.contains("store"));
        assert!(!out.contains('\u{1b}'));
        assert!(render_leaderboard(&[], false).contains("(no codecs)"));
    }
}
