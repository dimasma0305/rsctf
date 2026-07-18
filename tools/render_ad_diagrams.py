#!/usr/bin/env python3
"""Render the code-native diagrams used by the A&D player guide.

Run from the repository root:

    python3 tools/render_ad_diagrams.py
"""

from __future__ import annotations

import argparse
import io
import textwrap
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.axes import Axes
from matplotlib.figure import Figure
from matplotlib.patches import FancyArrowPatch, FancyBboxPatch
from PIL import Image
from PIL.PngImagePlugin import PngInfo


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_OUTPUT_DIR = ROOT / "docs" / "public" / "diagrams"

PAPER = "#f5f0e6"
PANEL = "#fffaf0"
INK = "#172733"
MUTED = "#5d5a55"
GRID = "#d8d0c2"
CORAL = "#a93429"
CORAL_PALE = "#f4d8cf"
TEAL = "#096763"
TEAL_PALE = "#d8ebe6"
BLUE = "#315f9d"
BLUE_PALE = "#dce5f1"
GOLD = "#825808"
GOLD_PALE = "#f0e2bf"
WHITE = "#fffdf8"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=DEFAULT_OUTPUT_DIR,
        help="directory for generated PNG files",
    )
    return parser.parse_args()


def new_canvas() -> tuple[Figure, Axes]:
    plt.rcParams.update(
        {
            "font.family": "DejaVu Sans",
            "text.color": INK,
            "axes.facecolor": PAPER,
            "figure.facecolor": PAPER,
        }
    )
    figure, axis = plt.subplots(figsize=(16, 9), dpi=150)
    axis.set_xlim(0, 1)
    axis.set_ylim(0, 1)
    axis.axis("off")
    return figure, axis


def save_opaque_png(figure: Figure, output: Path, title: str) -> None:
    buffer = io.BytesIO()
    figure.savefig(
        buffer,
        format="png",
        dpi=150,
        facecolor=PAPER,
        transparent=False,
        bbox_inches=None,
    )
    buffer.seek(0)
    with Image.open(buffer) as rendered:
        rgba = rendered.convert("RGBA")
        opaque = Image.new("RGB", rgba.size, PAPER)
        opaque.paste(rgba, mask=rgba.getchannel("A"))
        metadata = PngInfo()
        metadata.add_text("Title", title)
        metadata.add_text("Author", "rsctf")
        opaque.save(output, format="PNG", pnginfo=metadata)


def rounded_box(
    axis: Axes,
    x: float,
    y: float,
    width: float,
    height: float,
    *,
    face: str,
    edge: str = GRID,
    linewidth: float = 1.4,
    radius: float = 0.018,
) -> FancyBboxPatch:
    patch = FancyBboxPatch(
        (x, y),
        width,
        height,
        boxstyle=f"round,pad=0.008,rounding_size={radius}",
        facecolor=face,
        edgecolor=edge,
        linewidth=linewidth,
        transform=axis.transAxes,
    )
    axis.add_patch(patch)
    return patch


def add_text(
    axis: Axes,
    x: float,
    y: float,
    value: str,
    *,
    size: float,
    color: str = INK,
    weight: str = "normal",
    family: str = "DejaVu Sans",
    ha: str = "left",
    va: str = "center",
    linespacing: float = 1.25,
) -> None:
    axis.text(
        x,
        y,
        value,
        fontsize=size,
        color=color,
        fontweight=weight,
        fontfamily=family,
        ha=ha,
        va=va,
        linespacing=linespacing,
        transform=axis.transAxes,
    )


def wrapped(value: str, width: int) -> str:
    return textwrap.fill(value, width=width, break_long_words=False)


def arrow(
    axis: Axes,
    start: tuple[float, float],
    end: tuple[float, float],
    *,
    color: str = INK,
    width: float = 1.8,
    style: str = "arc3",
) -> None:
    axis.add_patch(
        FancyArrowPatch(
            start,
            end,
            arrowstyle="-|>",
            mutation_scale=15,
            linewidth=width,
            color=color,
            connectionstyle=style,
            transform=axis.transAxes,
        )
    )


def header(
    axis: Axes,
    title: str,
    subtitle: str,
    *,
    badge: str,
    badge_face: str,
    badge_color: str,
) -> None:
    add_text(
        axis,
        0.045,
        0.925,
        title,
        size=27,
        weight="bold",
        family="DejaVu Serif",
    )
    add_text(axis, 0.047, 0.875, subtitle, size=12.5, color=MUTED)
    rounded_box(
        axis,
        0.79,
        0.885,
        0.165,
        0.055,
        face=badge_face,
        edge=badge_color,
        linewidth=1.2,
        radius=0.024,
    )
    add_text(
        axis,
        0.8725,
        0.9125,
        badge,
        size=9.5,
        color=badge_color,
        weight="bold",
        ha="center",
    )


def evidence_card(
    axis: Axes,
    x: float,
    symbol: str,
    title: str,
    primary: str,
    secondary: str,
    *,
    accent: str,
    face: str,
) -> None:
    rounded_box(axis, x, 0.63, 0.27, 0.205, face=face, edge=accent, linewidth=1.6)
    rounded_box(axis, x + 0.018, 0.758, 0.048, 0.052, face=accent, edge=accent, radius=0.022)
    add_text(axis, x + 0.042, 0.784, symbol, size=11, color=WHITE, weight="bold", ha="center")
    add_text(axis, x + 0.078, 0.784, title, size=11, color=accent, weight="bold")
    add_text(axis, x + 0.018, 0.714, wrapped(primary, 34), size=9.7, weight="bold")
    add_text(axis, x + 0.018, 0.662, wrapped(secondary, 44), size=8.8, color=MUTED)


def render_epoch_scoring(output: Path) -> None:
    figure, axis = new_canvas()
    header(
        axis,
        "Outcome-based epoch scoring",
        "The official 40/40/20 policy from accepted flags, protected flag pairs, and checker SLA.",
        badge="OFFICIAL SCORING",
        badge_face=TEAL_PALE,
        badge_color=TEAL,
    )
    add_text(
        axis,
        0.8725,
        0.857,
        "Determines rank and awards",
        size=8.2,
        color=TEAL,
        weight="bold",
        ha="center",
    )

    evidence_card(
        axis,
        0.045,
        "A",
        "ATTACK COVERAGE",
        "Accepted flags + rarity up to 25% of base coverage",
        "Realized lift is <=20 percentage pts; low k never proves a patch bypass.",
        accent=BLUE,
        face=BLUE_PALE,
    )
    evidence_card(
        axis,
        0.365,
        "D",
        "DEFENSE OUTCOME",
        "Per exact healthy custom-check flag: M-k protected pairs out of M",
        "Unstolen pairs are observational; absence never proves an attempt.",
        accent=TEAL,
        face=TEAL_PALE,
    )
    evidence_card(
        axis,
        0.685,
        "R",
        "LOCAL SLA",
        "Credit over the frozen service-by-round grid",
        "Missing is zero; InternalError carries prior; isolated first error is zero.",
        accent=GOLD,
        face=GOLD_PALE,
    )

    rounded_box(axis, 0.045, 0.47, 0.91, 0.105, face=INK, edge=INK, radius=0.02)
    add_text(
        axis,
        0.065,
        0.5225,
        "40% A  +  40% D  +  20% sqrt(A x D)",
        size=15,
        color=WHITE,
        weight="bold",
    )
    add_text(axis, 0.485, 0.5225, "x  R", size=15, color=GOLD_PALE, weight="bold")
    arrow(axis, (0.565, 0.5225), (0.615, 0.5225), color=WHITE, width=1.5)
    add_text(axis, 0.632, 0.5225, "LOCAL SERVICE SCORE", size=11, color=WHITE, weight="bold")
    add_text(
        axis,
        0.94,
        0.5225,
        "0-100",
        size=12,
        color=GOLD_PALE,
        weight="bold",
        ha="right",
    )

    add_text(axis, 0.045, 0.425, "TEAM-CONTROLLED WORKFLOW", size=10, color=MUTED, weight="bold")
    rounded_box(axis, 0.045, 0.245, 0.19, 0.14, face=PANEL, edge=BLUE)
    add_text(axis, 0.063, 0.345, "MANAGE EXPLOITS", size=9.5, color=BLUE, weight="bold")
    add_text(
        axis,
        0.063,
        0.295,
        wrapped("Build and run attack tools on your own systems", 29),
        size=9.2,
        color=INK,
    )
    arrow(axis, (0.245, 0.315), (0.275, 0.315), color=MUTED, width=1.5)

    rounded_box(axis, 0.285, 0.245, 0.19, 0.14, face=PANEL, edge=GOLD)
    add_text(axis, 0.303, 0.345, "SUBMIT FLAGS", size=9.5, color=GOLD, weight="bold")
    add_text(
        axis,
        0.303,
        0.295,
        wrapped("Use the UI or game-scoped API; rsctf validates", 29),
        size=9.2,
        color=INK,
    )
    arrow(axis, (0.485, 0.315), (0.515, 0.315), color=MUTED, width=1.5)

    rounded_box(axis, 0.525, 0.245, 0.43, 0.14, face=PANEL, edge=GRID)
    result_rows = [
        (0.348, BLUE, "ACCEPTED FLAG", "qualifies shared attack coverage"),
        (0.315, CORAL, "FEW CAPTURERS", "lift <=25% of C; <=20 pct pts"),
        (0.282, TEAL, "PROTECTED PAIR", "adds observational defense"),
    ]
    for y, accent, label, meaning in result_rows:
        rounded_box(axis, 0.543, y - 0.014, 0.15, 0.027, face=accent, edge=accent, radius=0.012)
        add_text(axis, 0.618, y, label, size=7.0, color=WHITE, weight="bold", ha="center")
        add_text(axis, 0.708, y, meaning, size=8.7, color=INK)

    rounded_box(axis, 0.045, 0.145, 0.91, 0.055, face=GOLD_PALE, edge=GOLD, linewidth=1.2)
    add_text(
        axis,
        0.5,
        0.1725,
        "Complete epoch weight 1; tail r/n   |   Totals use all; UI details latest 3",
        size=10.2,
        color=GOLD,
        weight="bold",
        ha="center",
    )

    add_text(
        axis,
        0.5,
        0.097,
        "Frozen startRound roster; each offense flag gives every opponent the same denominator.",
        size=9.1,
        color=MUTED,
        ha="center",
    )
    rounded_box(axis, 0.16, 0.035, 0.68, 0.042, face=CORAL_PALE, edge=CORAL, linewidth=1.1)
    add_text(
        axis,
        0.5,
        0.056,
        "DEF needs exact checks; only an all-service first-error outage voids SLA roster-wide.",
        size=8.2,
        color=CORAL,
        weight="bold",
        ha="center",
    )

    output.parent.mkdir(parents=True, exist_ok=True)
    save_opaque_png(figure, output, "Outcome-based epoch scoring")
    plt.close(figure)


def main() -> None:
    args = parse_args()
    output_dir = args.output_dir.resolve()
    render_epoch_scoring(output_dir / "ad-scoring.png")
    print(f"rendered A&D diagrams in {output_dir}")


if __name__ == "__main__":
    main()
