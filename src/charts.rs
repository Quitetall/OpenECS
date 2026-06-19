//! Charts — graphs for the read-out (host-side DX).
//!
//! Two output targets, both dependency-backed (no hand-rolled plotting):
//!
//! - **SVG** via [`plotters`] — embedded inline in the HTML report
//!   ([`crate::report`]'s `to_html`). Self-contained markup, no JS.
//! - **Terminal** via [`textplots`] (braille R–D scatter) plus a small
//!   block-bar renderer for the per-band view.
//!
//! Every function returns a `String`, so they are pure and testable and the
//! caller decides where the bytes go.

use plotters::prelude::*;
use textplots::{Chart, Plot, Shape};

use crate::report::EcsReport;

/// A categorical color ramp for per-codec series (RGB).
const PALETTE: [(u8, u8, u8); 6] = [
    (31, 119, 180),  // blue
    (255, 127, 14),  // orange
    (44, 160, 44),   // green
    (214, 39, 40),   // red
    (148, 103, 189), // purple
    (140, 86, 75),   // brown
];

/// Pick a palette color by index (wraps).
fn color(i: usize) -> RGBColor {
    let (r, g, b) = PALETTE[i % PALETTE.len()];
    RGBColor(r, g, b)
}

// ─────────────────────────── terminal (ASCII) ──────────────────────────────

/// A per-band R + PRD block-bar view of one report (for `--charts`).
pub fn ascii_per_band(rep: &EcsReport, width: usize) -> String {
    let mut s = String::new();
    s.push_str(&format!("per-band fidelity — {} on {}\n", rep.codec, rep.dataset));
    for b in &rep.per_band {
        // R in [0,1] directly; PRD shown as a fraction of a 40% reference.
        let r_bar = crate::term::bar(b.r.clamp(0.0, 1.0), width);
        s.push_str(&format!("  {:<9} R {:>6.3} {}\n", b.band, b.r, r_bar));
    }
    s
}

/// A braille rate–distortion scatter (compression ratio vs PRD%) of per-file
/// points, rendered with [`textplots`]. Higher CR + lower PRD is better
/// (top-left). Lossless points (PRD 0) line the left edge.
pub fn ascii_rd_scatter(points: &[(f32, f32)]) -> String {
    if points.is_empty() {
        return "(no points to plot)\n".to_string();
    }
    let x_max = points.iter().map(|p| p.0).fold(1.0_f32, f32::max) * 1.1;
    let shape = Shape::Points(points);
    let mut chart = Chart::new(100, 50, 0.0, x_max);
    let plot = chart.lineplot(&shape);
    plot.axis();
    plot.figures();
    format!("CR (x) vs PRD% (y) — top-left is best\n{plot}\n")
}

// ─────────────────────────────── SVG ───────────────────────────────────────

/// Render `f` onto a fresh SVG canvas and return the markup. Drawing errors
/// (which only arise from malformed ranges) yield an empty-but-valid SVG so
/// the report still renders.
fn svg_canvas(
    w: u32,
    h: u32,
    f: impl FnOnce(&DrawingArea<SVGBackend, plotters::coord::Shift>) -> Result<(), Box<dyn std::error::Error>>,
) -> String {
    let mut buf = String::new();
    {
        let root = SVGBackend::with_string(&mut buf, (w, h)).into_drawing_area();
        // Best-effort: a fill/draw/present error yields an empty-but-valid SVG
        // rather than aborting the whole report. `root` borrows `buf`, so it
        // must drop (end of this block) before `buf` is returned.
        let _ = root.fill(&WHITE);
        let _ = f(&root);
        let _ = root.present();
    }
    buf
}

/// A bar chart of one metric per codec (e.g. pooled CR), best-on-top order is
/// the caller's. `rows` is `(label, value)`.
pub fn svg_codec_bars(title: &str, y_label: &str, rows: &[(String, f64)]) -> String {
    let n = rows.len().max(1);
    let y_max = rows.iter().map(|(_, v)| *v).fold(1.0_f64, f64::max) * 1.15;
    svg_canvas(720, 360, move |root| {
        let mut chart = ChartBuilder::on(root)
            .caption(title, ("sans-serif", 18))
            .margin(12)
            .x_label_area_size(50)
            .y_label_area_size(56)
            .build_cartesian_2d(0..n, 0.0..y_max)?;
        chart
            .configure_mesh()
            .disable_x_mesh()
            .y_desc(y_label)
            .x_labels(n)
            .x_label_formatter(&|i| rows.get(*i).map(|(l, _)| l.clone()).unwrap_or_default())
            .draw()?;
        chart.draw_series(rows.iter().enumerate().map(|(i, (_, v))| {
            let mut bar = Rectangle::new([(i, 0.0), (i + 1, *v)], color(i).filled());
            bar.set_margin(0, 0, 6, 6);
            bar
        }))?;
        Ok(())
    })
}

/// A rate–distortion scatter (CR vs PRD%), one colored series per codec.
/// `series` is `(codec_name, points)` where each point is `(cr, prd)`.
pub fn svg_rd_scatter(series: &[(String, Vec<(f64, f64)>)]) -> String {
    let x_max = series
        .iter()
        .flat_map(|(_, p)| p.iter().map(|(x, _)| *x))
        .fold(1.0_f64, f64::max)
        * 1.1;
    let y_max = series
        .iter()
        .flat_map(|(_, p)| p.iter().map(|(_, y)| *y))
        .fold(1.0_f64, f64::max)
        * 1.1;
    svg_canvas(720, 400, move |root| {
        let mut chart = ChartBuilder::on(root)
            .caption("Rate–distortion (CR vs PRD%) — top-left is best", ("sans-serif", 16))
            .margin(12)
            .x_label_area_size(40)
            .y_label_area_size(50)
            .build_cartesian_2d(0.0..x_max, 0.0..y_max)?;
        chart.configure_mesh().x_desc("compression ratio").y_desc("PRD %").draw()?;
        for (i, (name, pts)) in series.iter().enumerate() {
            let c = color(i);
            chart
                .draw_series(pts.iter().map(|(x, y)| Circle::new((*x, *y), 4, c.filled())))?
                .label(name.clone())
                .legend(move |(x, y)| Circle::new((x, y), 4, c.filled()));
        }
        chart.configure_series_labels().background_style(WHITE).border_style(BLACK).draw()?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::BandResult;

    fn rep() -> EcsReport {
        EcsReport {
            spec_version: "1.0".into(),
            codec: "store".into(),
            dataset: "d".into(),
            n_files: 1,
            bit_exact: true,
            grade: 'L',
            cr: 2.0,
            prd: 0.0,
            prdn: 0.0,
            r: 1.0,
            snr_db: 120.0,
            qs: 2.0,
            per_band: vec![
                BandResult::new("delta", 1.0, 0.0, 120.0),
                BandResult::new("gamma", 0.9, 5.0, 30.0),
            ],
            throughput_mibs: 10.0,
            peak_bytes: 1024,
            violations: vec![],
        }
    }

    #[test]
    fn svg_outputs_are_wellformed() {
        let bars = svg_codec_bars("CR by codec", "CR", &[("store".into(), 1.0), ("gzip".into(), 3.4)]);
        assert!(bars.contains("<svg"), "bar chart is SVG");
        assert!(bars.contains("</svg>"));
        let scatter = svg_rd_scatter(&[("gzip".into(), vec![(3.4, 0.0), (2.1, 1.2)])]);
        assert!(scatter.contains("<svg") && scatter.contains("</svg>"));
        // Empty series still yields a valid SVG (no panic).
        assert!(svg_rd_scatter(&[]).contains("<svg"));
    }

    #[test]
    fn ascii_charts_render() {
        let pb = ascii_per_band(&rep(), 20);
        assert!(pb.contains("delta") && pb.contains("gamma"));
        let sc = ascii_rd_scatter(&[(2.0, 0.0), (3.0, 1.5)]);
        assert!(sc.contains("CR"));
        assert!(ascii_rd_scatter(&[]).contains("no points"));
    }
}
