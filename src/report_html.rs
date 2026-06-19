//! Self-contained HTML report — the shareable artifact (host-side DX).
//!
//! [`render`] turns one or more [`EcsSubmission`]s (one per codec) into a
//! single HTML page with inline CSS and inline SVG charts ([`crate::charts`]):
//! a codec-comparison table, a pooled-CR bar chart, a rate–distortion scatter,
//! and per-codec per-file tables. No external assets, no JavaScript — open the
//! file anywhere.

use crate::charts;
use crate::report::{EcsReport, EcsSubmission};
use crate::stats::{self, Ci};

/// HTML-escape a string for safe embedding in text / attribute context.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Tier code → an accessible badge color.
fn grade_color(grade: char) -> &'static str {
    match grade {
        'L' => "#1a7f37", // green
        'N' => "#2da44e", // bright green
        'C' => "#0969da", // blue
        'M' => "#9a6700", // amber
        'A' => "#8250df", // purple
        _ => "#cf222e",   // red
    }
}

fn grade_label(grade: char) -> String {
    if grade == '\0' {
        "— below floor".to_string()
    } else {
        format!("ECS-{grade}")
    }
}

/// Fixed seed so bootstrap CIs in the report are reproducible across renders.
const CI_SEED: u64 = 0x1c5_5eed;

/// Bootstrap 95% CI of per-file Pearson R for a codec's reports.
fn r_ci(reports: &[EcsReport]) -> Ci {
    let rs: Vec<f64> = reports.iter().map(|r| r.r).collect();
    stats::bootstrap_ci(&rs, 0.95, 2000, CI_SEED)
}

/// Render a complete HTML report for the given per-codec submissions.
pub fn render(submissions: &[EcsSubmission]) -> String {
    let corpus = submissions
        .first()
        .map(|s| format!("{} v{}", s.corpus.name, s.corpus.version))
        .unwrap_or_else(|| "(no corpus)".to_string());
    let spec = submissions.first().map(|s| s.spec_version.clone()).unwrap_or_else(|| "1.0".into());

    // Charts: pooled CR per codec + per-file R–D scatter per codec.
    let cr_rows: Vec<(String, f64)> = submissions
        .iter()
        .map(|s| (s.codec.name.clone(), s.summary.mean_cr))
        .collect();
    let rd_series: Vec<(String, Vec<(f64, f64)>)> = submissions
        .iter()
        .map(|s| {
            (
                s.codec.name.clone(),
                s.reports.iter().map(|r| (r.cr, r.prd)).collect(),
            )
        })
        .collect();
    let cr_svg = charts::svg_codec_bars("Pooled compression ratio by codec", "CR", &cr_rows);
    let rd_svg = charts::svg_rd_scatter(&rd_series);

    let mut h = String::new();
    h.push_str("<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\n");
    h.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    h.push_str(&format!("<title>OpenECS report — {}</title>\n", esc(&corpus)));
    h.push_str(STYLE);
    h.push_str("</head>\n<body>\n<main>\n");
    h.push_str(&format!(
        "<h1>OpenECS report <span class=\"sub\">v{}</span></h1>\n<p class=\"meta\">corpus: <b>{}</b> · {} codec(s)</p>\n",
        esc(&spec),
        esc(&corpus),
        submissions.len()
    ));

    // ── Codec comparison table. ────────────────────────────────────────
    h.push_str("<h2>Codecs</h2>\n<table class=\"cmp\">\n<thead><tr>\
        <th>codec</th><th>grade</th><th>pooled CR</th><th>mean R (95% CI)</th>\
        <th>mean PRD</th><th>bit-exact</th><th>files</th></tr></thead>\n<tbody>\n");
    // Rank: worst grade first (stronger tier), then pooled CR desc.
    let mut order: Vec<usize> = (0..submissions.len()).collect();
    order.sort_by(|&a, &b| {
        let (sa, sb) = (&submissions[a].summary, &submissions[b].summary);
        grade_rank(sa.worst_grade)
            .cmp(&grade_rank(sb.worst_grade))
            .then(sb.mean_cr.partial_cmp(&sa.mean_cr).unwrap_or(std::cmp::Ordering::Equal))
    });
    for &i in &order {
        let s = &submissions[i];
        let ci = r_ci(&s.reports);
        let g = s.summary.worst_grade;
        h.push_str(&format!(
            "<tr><td class=\"codec\">{}</td>\
             <td><span class=\"badge\" style=\"background:{}\">{}</span></td>\
             <td>{:.2}:1</td><td>{:.4} <span class=\"ci\">[{:.4}, {:.4}]</span></td>\
             <td>{:.3}%</td><td>{}</td><td>{}</td></tr>\n",
            esc(&s.codec.name),
            grade_color(g),
            esc(&grade_label(g)),
            s.summary.mean_cr,
            ci.mean,
            ci.lo,
            ci.hi,
            s.summary.mean_prd,
            if s.summary.all_bit_exact { "yes" } else { "no" },
            s.summary.n_files,
        ));
    }
    h.push_str("</tbody></table>\n");

    // ── Charts. ────────────────────────────────────────────────────────
    h.push_str("<h2>Charts</h2>\n<div class=\"charts\">\n");
    h.push_str(&format!("<figure>{cr_svg}</figure>\n"));
    h.push_str(&format!("<figure>{rd_svg}</figure>\n"));
    h.push_str("</div>\n");

    // ── Per-codec per-file detail. ─────────────────────────────────────
    h.push_str("<h2>Per-file</h2>\n");
    for &i in &order {
        let s = &submissions[i];
        h.push_str(&format!("<h3>{}</h3>\n<table class=\"files\">\n\
            <thead><tr><th>#</th><th>grade</th><th>CR</th><th>PRD%</th><th>R</th>\
            <th>SNR dB</th><th>MiB/s</th></tr></thead>\n<tbody>\n", esc(&s.codec.name)));
        for (j, r) in s.reports.iter().enumerate() {
            h.push_str(&format!(
                "<tr><td>{}</td><td><span class=\"badge sm\" style=\"background:{}\">{}</span></td>\
                 <td>{:.2}</td><td>{:.3}</td><td>{:.4}</td><td>{:.1}</td><td>{:.2}</td></tr>\n",
                j + 1,
                grade_color(r.grade),
                esc(&grade_label(r.grade)),
                r.cr,
                r.prd,
                r.r,
                r.snr_db,
                r.throughput_mibs,
            ));
        }
        h.push_str("</tbody></table>\n");
    }

    h.push_str("<footer>Generated by <code>openecs</code> · OpenECS v");
    h.push_str(&esc(&spec));
    h.push_str(" · grades are signal-fidelity + resource only (task concordance is advisory).</footer>\n");
    h.push_str("</main>\n</body></html>\n");
    h
}

/// Tier strength for ordering (lower = stronger).
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

/// Inline stylesheet — keeps the report a single self-contained file.
const STYLE: &str = r#"<style>
:root{color-scheme:light dark}
body{font:15px/1.5 -apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;margin:0;background:#fbfbfd;color:#1d1d1f}
main{max-width:980px;margin:0 auto;padding:32px 20px 64px}
h1{font-size:28px;margin:0 0 4px}h1 .sub{font-size:14px;color:#86868b;font-weight:500}
h2{font-size:20px;margin:32px 0 8px;border-bottom:1px solid #e5e5ea;padding-bottom:4px}
h3{font-size:15px;margin:20px 0 6px;color:#3a3a3c}
.meta{color:#6e6e73;margin:0 0 8px}
table{border-collapse:collapse;width:100%;margin:8px 0;font-variant-numeric:tabular-nums}
th,td{text-align:right;padding:6px 10px;border-bottom:1px solid #ececf0}
th{font-weight:600;color:#6e6e73;font-size:12px;text-transform:uppercase;letter-spacing:.03em}
td.codec,td:first-child,th:first-child{text-align:left}
.cmp td.codec{font-weight:600}
.badge{display:inline-block;color:#fff;border-radius:6px;padding:2px 8px;font-size:12px;font-weight:700}
.badge.sm{padding:1px 6px;font-size:11px}
.ci{color:#86868b;font-size:12px}
.charts{display:flex;flex-wrap:wrap;gap:16px}
figure{margin:0;background:#fff;border:1px solid #e5e5ea;border-radius:10px;padding:8px}
figure svg{display:block;max-width:100%;height:auto}
footer{margin-top:40px;color:#86868b;font-size:12px;border-top:1px solid #e5e5ea;padding-top:12px}
code{background:#f0f0f3;border-radius:4px;padding:1px 5px}
</style>
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{CodecIdentity, CorpusIdentity};
    use crate::adapter::Store;
    use crate::harness;

    fn submission(name: &str) -> EcsSubmission {
        let sig = vec![vec![0i64, 1, 2, 3, 100, -100], vec![5, 6, 7, 8, 9, 10]];
        let (reports, summary) = harness::run_corpus(&Store, &[(sig, 256.0)]);
        EcsSubmission::new(
            CodecIdentity { name: name.into(), manifest_sha256: None },
            CorpusIdentity { name: "ecs-smoke".into(), version: "1.0.0".into() },
            reports,
            summary,
        )
    }

    #[test]
    fn html_is_self_contained_and_has_charts() {
        let html = render(&[submission("store"), submission("gzip")]);
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("</html>"));
        assert!(html.contains("<svg"), "embedded SVG charts present");
        assert!(html.contains("store") && html.contains("gzip"));
        assert!(html.contains("ECS-L"));
        // No external resource fetches or scripts (self-contained). The only
        // URLs present are SVG/XML namespace identifiers, which are not loads.
        assert!(!html.contains("src=\"http"), "no external resource src");
        assert!(!html.contains("href=\"http"), "no external resource href");
        assert!(!html.contains("<script"), "no scripts");
    }

    #[test]
    fn empty_submissions_is_safe() {
        let html = render(&[]);
        assert!(html.contains("</html>"));
        assert!(html.contains("no corpus"));
    }
}
