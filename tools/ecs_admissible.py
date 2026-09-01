#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Plot measured codec results against the OpenECS admissible regions.

WHAT THIS ANSWERS. `openecs grade` tells you the tier a codec reached. It does
not tell you *how far* a failing codec is from the next tier, or which of the
three criteria is the one actually blocking it. That distinction is the whole
difference between "keep tuning the entropy coder" and "the architecture is
wrong", and reading it off a table of numbers is exactly where people guess.

WHY THE LADDER IS NOT IN THIS FILE. `src/levels.rs` is the single source of
truth for every floor. This script reads `SPEC/levels.json`, the committed
export of that table, which `tests/spec_thresholds.rs` fails on if it drifts.
Re-typing a floor here would create a second number for the same fact, and the
copy would keep grading against last year's spec after the next revision. If the
export is missing or malformed this script REFUSES to run rather than assume a
ladder -- a chart drawn against invented floors is worse than no chart.

THE HONEST LIMIT OF THE PICTURE. A tier is three simultaneous constraints -- R,
PRD and CR -- and a 2-D plot has room for two. The chart draws the (CR, R)
admissible rectangles and encodes the PRD verdict in the MARKER: a filled point
clears every criterion of some tier, a hollow point does not. The full
three-criterion verdict is in `verdict.json`, which is the artefact you should
actually parse. The chart is for seeing the shape; the JSON is for deciding.

USAGE

    # interactive -- asks for points, then writes the package
    ./tools/ecs_admissible.py

    # from OpenECS submission JSON (the standard's own output)
    ./tools/ecs_admissible.py --submission run.json --out ecs-admissible/

    # from literal measurements, repeatable
    ./tools/ecs_admissible.py --point 0.711,73.2,111,"E1 L8" \\
                              --point 0.589,117.7,210,"E1 L3"

OUTPUT PACKAGE (--out, default ./ecs-admissible)

    chart.svg      the plot, self-contained, no JS, theme-neutral
    verdict.json   THE parsable artefact: every point x every tier x every
                   criterion, with signed gaps
    summary.txt    the same thing for a human or a commit message
    MANIFEST.json  sha256 of each file, plus which ladder was graded against

EXIT CODES
    0  at least one input point clears at least one tier
    1  no point clears any tier (a real verdict, not an error)
    2  usage or input error
    3  the ladder could not be read -- fail-closed, nothing was written
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import subprocess
import sys
from dataclasses import dataclass, asdict
from datetime import datetime, timezone
from pathlib import Path

SCRIPT_VERSION = "1.0.0"
REPO = Path(__file__).resolve().parents[1]
LEVELS_JSON = REPO / "SPEC" / "levels.json"

# Tier accent colours. Chosen per-tier rather than per-verdict so the same tier
# reads the same in the chart, the legend and a second chart made next week.
TIER_COLOR = {
    "L": "#5B6B70",
    "N": "#4C7A8C",
    "C": "#1B6E7D",
    "M": "#3D7A50",
    "A": "#9C6B1E",
}


# ─────────────────────────────── the ladder ────────────────────────────────


class LadderError(RuntimeError):
    """The tier ladder could not be established. Never recoverable by guessing."""


def load_levels(refresh: bool) -> tuple[list[dict], dict]:
    """Return (levels, provenance). Fail closed; never fall back to a literal."""
    if refresh:
        # Regenerating needs the binary, which needs a toolchain. That is a fine
        # requirement for an explicit --refresh-levels, and a bad one for the
        # default path, which is why it is opt-in.
        try:
            out = subprocess.run(
                ["cargo", "run", "--quiet", "--bin", "openecs", "--", "levels"],
                cwd=REPO, capture_output=True, text=True, timeout=900, check=True,
            ).stdout
            LEVELS_JSON.write_text(out)
        except (OSError, subprocess.SubprocessError) as e:
            raise LadderError(f"--refresh-levels failed: {e}") from e

    if not LEVELS_JSON.exists():
        raise LadderError(
            f"{LEVELS_JSON} is missing. Regenerate it with\n"
            f"    cargo run --bin openecs -- levels > SPEC/levels.json\n"
            f"or re-run this script with --refresh-levels."
        )
    try:
        doc = json.loads(LEVELS_JSON.read_text())
    except (OSError, json.JSONDecodeError) as e:
        raise LadderError(f"{LEVELS_JSON} is unreadable: {e}") from e

    if doc.get("schema") != "openecs.levels/v1":
        raise LadderError(
            f"{LEVELS_JSON} has schema {doc.get('schema')!r}, expected "
            f"'openecs.levels/v1'. Refusing to grade against a ladder whose "
            f"shape this script does not understand."
        )
    levels = doc.get("levels")
    if not isinstance(levels, list) or not levels:
        raise LadderError(f"{LEVELS_JSON} carries no levels.")
    for lv in levels:
        for key in ("level", "name", "min_r", "max_prd", "min_cr"):
            if key not in lv:
                raise LadderError(f"level {lv.get('name')!r} is missing {key!r}.")
    return levels, {
        "schema": doc["schema"],
        "crate_version": doc.get("crate_version", "unknown"),
        "sha256": hashlib.sha256(LEVELS_JSON.read_bytes()).hexdigest(),
        "path": str(LEVELS_JSON.relative_to(REPO)),
    }


# ──────────────────────────────── the points ────────────────────────────────


@dataclass
class Point:
    label: str
    r: float
    prd: float
    cr: float
    source: str


def _f(v, what: str) -> float:
    try:
        return float(v)
    except (TypeError, ValueError) as e:
        raise ValueError(f"{what}: {v!r} is not a number") from e


def from_submission(path: Path) -> list[Point]:
    """Read an OpenECS submission JSON. Accepts a submission or a bare report."""
    doc = json.loads(path.read_text())
    reports = doc.get("reports")
    if reports is None:
        reports = [doc] if "r" in doc and "cr" in doc else None
    if not reports:
        raise ValueError(
            f"{path} has no `reports` array and is not itself a report "
            f"(no `r`/`cr` keys). Is this an OpenECS submission?"
        )
    pts = []
    for i, rep in enumerate(reports):
        for key in ("r", "prd", "cr"):
            if key not in rep:
                raise ValueError(f"{path} report[{i}] is missing {key!r}")
        label = rep.get("codec") or "codec"
        ds = rep.get("dataset")
        pts.append(Point(
            label=f"{label}·{ds}" if ds else label,
            r=_f(rep["r"], "r"), prd=_f(rep["prd"], "prd"), cr=_f(rep["cr"], "cr"),
            source=str(path),
        ))
    return pts


def from_spec(spec: str) -> Point:
    """`R,PRD,CR[,label]` — the form you type when you already have numbers."""
    parts = [p.strip() for p in spec.split(",")]
    if len(parts) < 3:
        raise ValueError(f"--point wants R,PRD,CR[,label]; got {spec!r}")
    label = parts[3].strip('"') if len(parts) > 3 else f"R={parts[0]}"
    return Point(label=label, r=_f(parts[0], "R"), prd=_f(parts[1], "PRD"),
                 cr=_f(parts[2], "CR"), source="--point")


def from_csv(path: Path) -> list[Point]:
    """CSV with r,prd,cr[,label] headers (case-insensitive)."""
    pts = []
    with path.open(newline="") as fh:
        for i, row in enumerate(csv.DictReader(fh)):
            low = {(k or "").strip().lower(): v for k, v in row.items()}
            for key in ("r", "prd", "cr"):
                if key not in low:
                    raise ValueError(f"{path} row {i}: no {key!r} column")
            pts.append(Point(label=low.get("label") or f"row{i}",
                             r=_f(low["r"], "r"), prd=_f(low["prd"], "prd"),
                             cr=_f(low["cr"], "cr"), source=str(path)))
    return pts


def prompt_points() -> list[Point]:
    """Interactive entry. Only reached when nothing else supplied a point."""
    print("OpenECS admissibility — enter measurements, blank line to finish.")
    print("Format: R,PRD,CR[,label]      e.g.  0.711,73.2,111,E1 at L=8\n")
    pts = []
    while True:
        try:
            line = input(f"point {len(pts) + 1}> ").strip()
        except EOFError:
            break
        if not line:
            break
        try:
            pts.append(from_spec(line))
        except ValueError as e:
            print(f"  ! {e}", file=sys.stderr)
    return pts


# ─────────────────────────────── the grading ────────────────────────────────


def grade(pt: Point, levels: list[dict]) -> dict:
    """Every criterion of every tier, with SIGNED gaps.

    The gap is the point of this function. A boolean tells you that you failed;
    the gap tells you by how much and therefore which lever is worth pulling.
    Positive gap = margin in hand, negative = the shortfall.
    """
    tiers = {}
    for lv in levels:
        code = lv["level"]
        r_ok = pt.r >= lv["min_r"]
        prd_ok = pt.prd <= lv["max_prd"]
        cr_ok = pt.cr >= lv["min_cr"]
        blocking = [n for n, ok in (("R", r_ok), ("PRD", prd_ok), ("CR", cr_ok)) if not ok]
        tiers[code] = {
            "name": lv["name"],
            "pass": r_ok and prd_ok and cr_ok,
            "blocking": blocking,
            "r": {"value": pt.r, "floor": lv["min_r"],
                  "gap": round(pt.r - lv["min_r"], 6), "pass": r_ok},
            "prd": {"value": pt.prd, "ceiling": lv["max_prd"],
                    "gap": round(lv["max_prd"] - pt.prd, 6), "pass": prd_ok},
            "cr": {"value": pt.cr, "floor": lv["min_cr"],
                   "gap": round(pt.cr - lv["min_cr"], 6), "pass": cr_ok},
        }
    # Best tier = the strictest one fully cleared. Ladder order in levels.json is
    # strictest-first, so the first pass wins.
    best = next((c for c in (lv["level"] for lv in levels) if tiers[c]["pass"]), None)
    return {"point": asdict(pt), "best_tier": best, "tiers": tiers}


# ──────────────────────────────── the chart ─────────────────────────────────


def render_svg(points: list[Point], verdicts: list[dict], levels: list[dict]) -> str:
    W, H = 760, 430
    L, R_, T, B = 74, 26, 26, 66          # margins
    pw, ph = W - L - R_, H - T - B

    # Plot only tiers with a finite, plottable CR floor. L and N sit at CR≈1 and
    # would flatten the log axis into uselessness for the lossy tiers this chart
    # is for; they are still graded and still in verdict.json.
    plot_tiers = [lv for lv in levels if lv["min_cr"] >= 10.0]

    crs = [p.cr for p in points] + [lv["min_cr"] for lv in plot_tiers]
    crs = [c for c in crs if c > 0] or [1.0, 400.0]
    x_lo, x_hi = min(crs) * 0.75, max(crs) * 1.35
    rs = [p.r for p in points] + [lv["min_r"] for lv in plot_tiers]
    y_lo = min(0.55, min(rs) - 0.05) if rs else 0.4
    y_hi = 1.0

    def px(cr: float) -> float:
        f = (math.log10(max(cr, 1e-9)) - math.log10(x_lo)) / (math.log10(x_hi) - math.log10(x_lo))
        return L + min(max(f, 0.0), 1.0) * pw

    def py(r: float) -> float:
        f = (y_hi - r) / (y_hi - y_lo)
        return T + min(max(f, 0.0), 1.0) * ph

    o: list[str] = []
    a = o.append
    a(f'<svg viewBox="0 0 {W} {H}" xmlns="http://www.w3.org/2000/svg" '
      f'font-family="ui-monospace,Menlo,Consolas,monospace" role="img" '
      f'aria-label="Measured codec results against OpenECS admissible regions">')
    a('<rect width="100%" height="100%" fill="#ffffff"/>')

    # admissible rectangles, loosest CR first so tighter labels land on top
    for lv in sorted(plot_tiers, key=lambda l: l["min_cr"]):
        c = TIER_COLOR.get(lv["level"], "#666")
        x0, y0 = px(lv["min_cr"]), py(y_hi)
        a(f'<rect x="{x0:.1f}" y="{y0:.1f}" width="{L + pw - x0:.1f}" '
          f'height="{py(lv["min_r"]) - y0:.1f}" fill="{c}" fill-opacity="0.075" '
          f'stroke="{c}" stroke-opacity="0.55" stroke-dasharray="4 3"/>')
        a(f'<text x="{x0 + 7:.1f}" y="{py(lv["min_r"]) - 8:.1f}" font-size="11" '
          f'font-weight="700" fill="{c}">{lv["level"]} · {lv["name"]}</text>')
        a(f'<text x="{x0 + 7:.1f}" y="{py(lv["min_r"]) - 21:.1f}" font-size="9" '
          f'fill="{c}" fill-opacity="0.85">R≥{lv["min_r"]:g} CR≥{lv["min_cr"]:g}× '
          f'PRD≤{lv["max_prd"]:g}</text>')

    # axes
    a(f'<line x1="{L}" y1="{T + ph}" x2="{L + pw}" y2="{T + ph}" stroke="#333" stroke-width="1.2"/>')
    a(f'<line x1="{L}" y1="{T}" x2="{L}" y2="{T + ph}" stroke="#333" stroke-width="1.2"/>')

    step = 0.05 if (y_hi - y_lo) <= 0.5 else 0.1
    tick = math.ceil(y_lo / step) * step
    while tick <= y_hi + 1e-9:
        yy = py(tick)
        a(f'<line x1="{L - 4:.1f}" y1="{yy:.1f}" x2="{L}" y2="{yy:.1f}" stroke="#333"/>')
        a(f'<text x="{L - 8:.1f}" y="{yy + 3.5:.1f}" font-size="10" fill="#555" '
          f'text-anchor="end">{tick:.2f}</text>')
        tick += step

    dec = math.floor(math.log10(x_lo))
    while 10 ** dec <= x_hi:
        for m in (1, 2, 5):
            v = m * 10 ** dec
            if x_lo <= v <= x_hi:
                xx = px(v)
                a(f'<line x1="{xx:.1f}" y1="{T + ph}" x2="{xx:.1f}" y2="{T + ph + 4}" stroke="#333"/>')
                a(f'<text x="{xx:.1f}" y="{T + ph + 17}" font-size="10" fill="#555" '
                  f'text-anchor="middle">{v:g}×</text>')
        dec += 1

    a(f'<text x="{L + pw / 2:.1f}" y="{H - 30}" font-size="11" fill="#444" '
      f'text-anchor="middle">compression ratio (log scale)</text>')
    a(f'<text x="16" y="{T + ph / 2:.1f}" font-size="11" fill="#444" text-anchor="middle" '
      f'transform="rotate(-90 16 {T + ph / 2:.1f})">fullband R</text>')

    # points
    for pt, v in zip(points, verdicts):
        x, y = px(pt.cr), py(pt.r)
        cleared = v["best_tier"] is not None
        col = TIER_COLOR.get(v["best_tier"], "#B23A2C") if cleared else "#B23A2C"
        if cleared:
            a(f'<circle cx="{x:.1f}" cy="{y:.1f}" r="5.5" fill="{col}"/>')
        else:
            a(f'<circle cx="{x:.1f}" cy="{y:.1f}" r="5.5" fill="#ffffff" '
              f'stroke="{col}" stroke-width="2.2"/>')
        a(f'<title>{_esc(pt.label)}: R={pt.r:.4g} PRD={pt.prd:.4g} CR={pt.cr:.4g}× '
          f'→ {v["best_tier"] or "no tier"}</title>')
        a(f'<text x="{x:.1f}" y="{y - 11:.1f}" font-size="9.5" fill="#222" '
          f'text-anchor="middle">{_esc(pt.label[:26])}</text>')

    a(f'<text x="{L}" y="{H - 10}" font-size="9.5" fill="#666">'
      f'filled = clears every criterion of a tier · hollow = does not '
      f'(PRD is graded but not an axis — see verdict.json)</text>')
    a("</svg>")
    return "\n".join(o)


def _esc(s: str) -> str:
    return (s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
             .replace('"', "&quot;"))


# ──────────────────────────────── the package ───────────────────────────────


def summary_text(verdicts: list[dict], levels: list[dict]) -> str:
    codes = [lv["level"] for lv in levels]
    w = max((len(v["point"]["label"]) for v in verdicts), default=5)
    w = min(max(w, 5), 34)
    out = ["OpenECS admissibility", "=" * 21, ""]
    out.append(f"{'point'.ljust(w)}  {'R':>7} {'PRD':>7} {'CR':>9}   " +
               "  ".join(c.center(5) for c in codes) + "   best")
    out.append("-" * (w + 34 + 7 * len(codes)))
    for v in verdicts:
        p = v["point"]
        cells = []
        for c in codes:
            t = v["tiers"][c]
            cells.append(("PASS" if t["pass"] else ",".join(t["blocking"])).center(5))
        out.append(f"{p['label'][:w].ljust(w)}  {p['r']:7.4g} {p['prd']:7.4g} "
                   f"{p['cr']:8.4g}×   " + "  ".join(cells) +
                   f"   {v['best_tier'] or '—'}")
    out += ["", "A cell shows the criteria BLOCKING that tier, so an entry reading",
            "'PRD' fails on distortion alone and an entry reading 'R,PRD,CR' fails",
            "on all three. Signed gaps per criterion are in verdict.json.", ""]

    blocked = [c for v in verdicts for t in v["tiers"].values() for c in t["blocking"]]
    if blocked:
        counts = {k: blocked.count(k) for k in ("R", "PRD", "CR")}
        worst = max(counts, key=lambda k: counts[k])
        out.append(f"Most frequently blocking criterion: {worst} "
                   f"({counts[worst]} of {len(blocked)} blocked criteria).")
    return "\n".join(out) + "\n"


def write_package(out: Path, points, verdicts, levels, prov, argv) -> dict:
    out.mkdir(parents=True, exist_ok=True)
    now = datetime.now(timezone.utc).isoformat(timespec="seconds")

    verdict = {
        "schema": "openecs.admissible/v1",
        "generated_utc": now,
        "script_version": SCRIPT_VERSION,
        "ladder": prov,
        "invocation": argv,
        "n_points": len(points),
        "any_tier_cleared": any(v["best_tier"] for v in verdicts),
        "results": verdicts,
    }
    files = {
        "verdict.json": json.dumps(verdict, indent=2) + "\n",
        "chart.svg": render_svg(points, verdicts, levels),
        "summary.txt": summary_text(verdicts, levels),
    }
    for name, body in files.items():
        (out / name).write_text(body)

    manifest = {
        "schema": "openecs.package/v1",
        "generated_utc": now,
        "script_version": SCRIPT_VERSION,
        "ladder": prov,
        "files": {
            name: {
                "sha256": hashlib.sha256((out / name).read_bytes()).hexdigest(),
                "bytes": (out / name).stat().st_size,
            }
            for name in files
        },
    }
    (out / "MANIFEST.json").write_text(json.dumps(manifest, indent=2) + "\n")
    return verdict


# ────────────────────────────────── main ────────────────────────────────────


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        prog="ecs_admissible.py",
        description="Plot measured codec results against the OpenECS admissible regions.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    ap.add_argument("--submission", action="append", type=Path, default=[],
                    metavar="JSON", help="OpenECS submission JSON (repeatable)")
    ap.add_argument("--point", action="append", default=[], metavar="R,PRD,CR[,LABEL]",
                    help="a literal measurement (repeatable)")
    ap.add_argument("--csv", type=Path, metavar="FILE",
                    help="CSV with r,prd,cr[,label] columns")
    ap.add_argument("--out", type=Path, default=Path("ecs-admissible"),
                    metavar="DIR", help="output package directory")
    ap.add_argument("--refresh-levels", action="store_true",
                    help="regenerate SPEC/levels.json from the openecs binary first")
    ap.add_argument("--quiet", action="store_true", help="write the package, print nothing")
    args = ap.parse_args(argv)

    try:
        levels, prov = load_levels(args.refresh_levels)
    except LadderError as e:
        print(f"ecs_admissible: {e}", file=sys.stderr)
        return 3

    points: list[Point] = []
    try:
        for sub in args.submission:
            points += from_submission(sub)
        for spec in args.point:
            points.append(from_spec(spec))
        if args.csv:
            points += from_csv(args.csv)
    except (OSError, ValueError, json.JSONDecodeError) as e:
        print(f"ecs_admissible: {e}", file=sys.stderr)
        return 2

    if not points:
        if sys.stdin.isatty():
            points = prompt_points()
        if not points:
            print("ecs_admissible: no measurements given. Use --point / --submission "
                  "/ --csv, or run interactively from a terminal.", file=sys.stderr)
            return 2

    verdicts = [grade(p, levels) for p in points]
    verdict = write_package(args.out, points, verdicts, levels, prov,
                            argv if argv is not None else sys.argv[1:])

    if not args.quiet:
        print(summary_text(verdicts, levels))
        print(f"package: {args.out}/  (chart.svg · verdict.json · summary.txt · MANIFEST.json)")
        print(f"ladder:  {prov['path']} @ {prov['crate_version']} "
              f"sha256:{prov['sha256'][:12]}")

    return 0 if verdict["any_tier_cleared"] else 1


if __name__ == "__main__":
    sys.exit(main())
