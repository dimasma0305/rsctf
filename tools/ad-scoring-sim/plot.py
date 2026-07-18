#!/usr/bin/env python3
"""Render the stakeholder view of the tracked A&D scoring simulation."""

from __future__ import annotations

import argparse
import io
import json
import math
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.patches import FancyBboxPatch
from matplotlib.ticker import PercentFormatter
from PIL import Image
from PIL.PngImagePlugin import PngInfo


ROOT = Path(__file__).resolve().parent
DEFAULT_INPUT = ROOT / "results.json"
DEFAULT_OUTPUT = ROOT / "graphs" / "synthetic-20-team-epoch-comebacks.png"

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

MANUAL_BALANCED = "manual-equal-balanced"
POSITIVE_DEFENSE = "maturity-positive-defense"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, default=DEFAULT_INPUT)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    return parser.parse_args()


def finite(value: object, label: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
        raise ValueError(f"{label} must be a finite number")
    return float(value)


def load_results(path: Path) -> dict:
    with path.open("r", encoding="utf-8") as handle:
        results = json.load(handle)

    metadata = results["metadata"]
    if metadata["teamCount"] != 20 or metadata["epochs"] != 6:
        raise ValueError("stakeholder graph requires the tracked 20-team, six-epoch run")
    if metadata["resultsSchemaVersion"] != 5 or not metadata["manualDefenseIsPairwise"]:
        raise ValueError("stakeholder graph requires official pairwise scoring results")
    if metadata["officialScoringMode"] != "EpochBalanced":
        raise ValueError("stakeholder graph requires the deployed official policy")
    if not metadata["officialBoardDeterminesRankAndAwards"]:
        raise ValueError("stakeholder graph requires the official ranking policy")
    if metadata["manualRarityCoefficient"] != 0.25:
        raise ValueError("stakeholder graph requires the tracked rarity coefficient")
    if not metadata["scoringRosterFrozenAtStartRound"]:
        raise ValueError("stakeholder graph requires a frozen scoring roster")
    if not metadata["outsideFrozenRosterCapturesExcluded"]:
        raise ValueError("stakeholder graph requires frozen-roster captures")
    if not metadata["attackDenominatorSharedByFrozenRoster"]:
        raise ValueError("stakeholder graph requires a common attack denominator")
    if metadata["fallbackTcpProbeQualifiesDefense"]:
        raise ValueError("fallback TCP probes must not qualify defense")

    rows = results["slaResponse"]["rows"]
    for formula in (MANUAL_BALANCED, POSITIVE_DEFENSE):
        selected = [
            row
            for row in rows
            if row["formula"] == formula and row["scenario"] == "exclusive-attacker"
        ]
        if {finite(row["uptime"], "uptime") for row in selected} != {0, 0.25, 0.5, 0.75, 1}:
            raise ValueError(f"incomplete stakeholder SLA response for {formula}")
        for row in selected:
            retention = finite(row["retentionPct"], "retention")
            if retention < 0 or retention > 100:
                raise ValueError("SLA retention must be in [0, 100]")

    collusion = results["collusionFunnelControl"]
    if collusion["directedMisses"] != 19 or collusion["observationalDefense"] <= 0:
        raise ValueError("unexpected collusion funnel control")
    if collusion["manualBalancedDefense"] <= 0:
        raise ValueError("manual outcome defense must expose withholding credit")

    live = results["liveEpochTotalControl"]
    if live["duringPlay"]["settledTotal"] == live["duringPlay"]["projectedTotal"]:
        raise ValueError("live control must distinguish settled and projected totals")
    if live["atGameEnd"]["settledTotal"] != live["atGameEnd"]["projectedTotal"]:
        raise ValueError("game-end control must finalize the partial tail")
    if live["duringPlay"]["epochWeights"] != [1, 0.125]:
        raise ValueError("live control must retain the one-eighth tail weight")

    examples = results["stakeholderExamples"]
    if [row["label"] for row in examples] != [
        "Balanced team",
        "Attack-heavy team",
        "Attack only",
    ]:
        raise ValueError("unexpected stakeholder example set")
    for row in examples:
        for field in ("attack", "defense", "arithmeticScore", "balancedScore"):
            finite(row[field], f"stakeholder example {field}")

    return results


def rounded_box(
    canvas: plt.Axes,
    x: float,
    y: float,
    width: float,
    height: float,
    face: str,
    edge: str = "none",
    radius: float = 0.012,
    linewidth: float = 1.0,
) -> None:
    canvas.add_patch(
        FancyBboxPatch(
            (x, y),
            width,
            height,
            boxstyle=f"round,pad=0.006,rounding_size={radius}",
            facecolor=face,
            edgecolor=edge,
            linewidth=linewidth,
            transform=canvas.transAxes,
            clip_on=False,
        )
    )


def token_card(
    canvas: plt.Axes,
    x: float,
    width: float,
    face: str,
    color: str,
    title: str,
    body: str,
    dark: bool = False,
) -> None:
    y, height = 0.674, 0.078
    rounded_box(canvas, x, y, width, height, face, edge=color if not dark else face,
                radius=0.011, linewidth=1.3)
    text_color = PAPER if dark else INK
    canvas.text(x + width / 2, y + 0.052, title, color=color, fontsize=11.5,
                fontweight="bold", ha="center", va="center")
    canvas.text(x + width / 2, y + 0.021, body, color=text_color, fontsize=8.8,
                ha="center", va="center", linespacing=1.15)


def sla_rows(results: dict, formula: str) -> list[dict]:
    return sorted(
        (
            row
            for row in results["slaResponse"]["rows"]
            if row["formula"] == formula and row["scenario"] == "exclusive-attacker"
        ),
        key=lambda row: row["uptime"],
    )


def panel_header(canvas: plt.Axes, x: float, number: str, title: str, subtitle: str) -> None:
    rounded_box(canvas, x, 0.545, 0.032, 0.038, INK, radius=0.01)
    canvas.text(x + 0.016, 0.564, number, color=PAPER, fontsize=11.5, fontweight="bold",
                ha="center", va="center")
    canvas.text(x + 0.044, 0.567, title, color=INK, fontsize=13.5, fontweight="bold",
                va="center")
    canvas.text(x, 0.525, subtitle, color=MUTED, fontsize=9.5, va="center")


def draw_header(canvas: plt.Axes) -> None:
    canvas.text(0.055, 0.95, "Score accepted outcomes, pair by pair", color=INK, fontsize=27,
                fontweight="bold", va="top")
    canvas.text(0.055, 0.902, "Official 40/40/20 EpochBalanced policy", color=TEAL,
                fontsize=13, fontweight="bold", va="top")
    canvas.text(
        0.055,
        0.862,
        "Deterministic policy examples  |  accepted-flag outcomes are synthetic  |  "
        "full sensitivity analysis remains in REPORT.md",
        color=MUTED,
        fontsize=9.7,
        va="top",
    )

    rounded_box(canvas, 0.745, 0.876, 0.2, 0.078, INK, radius=0.016)
    canvas.text(0.765, 0.934, "OFFICIAL SCOREBOARD", color=GOLD_PALE, fontsize=10.5,
                fontweight="bold", va="center")
    canvas.text(
        0.765,
        0.9,
        "Use normal flag submissions.\nTeams keep their exploit tools.\nDetermines rank and awards.",
        color=PAPER,
        fontsize=8.6,
        fontweight="bold",
        va="center",
        linespacing=1.2,
    )


def draw_formula(canvas: plt.Axes) -> None:
    rounded_box(canvas, 0.055, 0.625, 0.89, 0.19, PANEL, edge=GRID, radius=0.018)
    canvas.text(
        0.075,
        0.788,
        "How one service contributes to each team's 100-point epoch ceiling",
        color=INK,
        fontsize=13.5,
        fontweight="bold",
        va="center",
    )
    rounded_box(canvas, 0.76, 0.772, 0.165, 0.032, GOLD_PALE, radius=0.009)
    canvas.text(0.8425, 0.788, "6 full epochs  |  weight 1 each", color=GOLD,
                fontsize=8.6, fontweight="bold", ha="center", va="center")

    token_card(canvas, 0.075, 0.155, BLUE_PALE, BLUE, "40% ATTACK", "accepted coverage + bounded\ncapturer-count rarity")
    canvas.text(0.2375, 0.713, "+", color=INK, fontsize=14, fontweight="bold",
                ha="center", va="center")
    token_card(canvas, 0.245, 0.155, TEAL_PALE, TEAL, "40% DEFENSE", "pairwise protected outcomes\nfrom exact healthy checks")
    canvas.text(0.4075, 0.713, "+", color=INK, fontsize=14, fontweight="bold",
                ha="center", va="center")
    token_card(canvas, 0.415, 0.17, GOLD_PALE, GOLD, "20% INTERACTION", "sqrt(attack x defense)\nrewards doing both")
    canvas.text(0.61, 0.713, "x", color=INK, fontsize=15, fontweight="bold",
                ha="center", va="center")
    token_card(canvas, 0.63, 0.135, BLUE_PALE, BLUE, "LOCAL SLA", "availability +\ncorrectness")
    canvas.text(0.786, 0.713, "=", color=INK, fontsize=15, fontweight="bold",
                ha="center", va="center")
    token_card(canvas, 0.805, 0.12, INK, PAPER, "EPOCH SHARE", "bounded by the\nteam's ceiling", dark=True)

    rounded_box(canvas, 0.075, 0.632, 0.155, 0.032, BLUE_PALE, radius=0.007)
    canvas.text(
        0.1525,
        0.648,
        "WHEN M>=4: rarity = (M-k)/M\nA = min(1, C + 0.25H)",
        color=BLUE,
        fontsize=6.8,
        fontweight="bold",
        ha="center",
        va="center",
        linespacing=1.1,
    )


def draw_pairwise_panel(canvas: plt.Axes) -> None:
    x, y, width, height = 0.055, 0.175, 0.283, 0.425
    rounded_box(canvas, x, y, width, height, PANEL, edge=GRID, radius=0.018)
    panel_header(canvas, 0.075, "1", "Defense is pairwise", "One rare bypass removes one pair, not the whole flag.")

    rounded_box(canvas, 0.075, 0.455, 0.243, 0.05, GOLD_PALE, radius=0.01)
    canvas.text(0.1965, 0.48, "20 teams: M = 19 opponents  |  one capturer: k = 1",
                color=INK, fontsize=9.2, fontweight="bold", ha="center", va="center")

    rounded_box(canvas, 0.075, 0.315, 0.113, 0.115, CORAL_PALE, edge=CORAL, radius=0.012)
    canvas.text(0.1315, 0.4, "Binary flag", color=CORAL, fontsize=10.5,
                fontweight="bold", ha="center")
    canvas.text(0.1315, 0.357, "1 capture erases\nall flag defense", color=INK,
                fontsize=9.6, ha="center", va="center", linespacing=1.25)
    canvas.text(0.1315, 0.328, "0% retained", color=CORAL, fontsize=9.2,
                fontweight="bold", ha="center")

    rounded_box(canvas, 0.202, 0.315, 0.116, 0.115, TEAL_PALE, edge=TEAL, radius=0.012)
    canvas.text(0.26, 0.4, "Pairwise", color=TEAL, fontsize=10.5,
                fontweight="bold", ha="center")
    canvas.text(0.26, 0.357, "18 protected\nof 19 pairs", color=INK,
                fontsize=9.6, ha="center", va="center", linespacing=1.25)
    canvas.text(0.26, 0.328, "94.7% retained", color=TEAL, fontsize=9.2,
                fontweight="bold", ha="center")

    rounded_box(canvas, 0.075, 0.205, 0.243, 0.078, TEAL_PALE, edge=TEAL, radius=0.012)
    canvas.text(
        0.1965,
        0.244,
        "Unstolen is observational, not proof of an attempt.\n"
        "DEF needs exact planted-flag checker confirmation;\n"
        "fallback TCP probes create no opportunities.",
        color=INK,
        fontsize=8.2,
        fontweight="bold",
        ha="center",
        va="center",
        linespacing=1.25,
    )


def draw_sla_panel(canvas: plt.Axes, figure: plt.Figure, results: dict) -> None:
    x, y, width, height = 0.359, 0.175, 0.283, 0.425
    rounded_box(canvas, x, y, width, height, PANEL, edge=GRID, radius=0.018)
    panel_header(canvas, 0.379, "2", "SLA is predictable", "The whole local score scales with service health.")

    manual_all = sla_rows(results, MANUAL_BALANCED)
    unsafe_all = sla_rows(results, POSITIVE_DEFENSE)
    manual = [row for row in manual_all if row["uptime"] in {0, 0.5, 1}]
    unsafe = [row for row in unsafe_all if row["uptime"] in {0, 0.5, 1}]

    axis = figure.add_axes([0.395, 0.285, 0.22, 0.205], facecolor="none")
    axis.plot([100 * row["uptime"] for row in unsafe], [row["retentionPct"] for row in unsafe],
              color=CORAL, linewidth=2.2, linestyle="--", marker="s", markersize=5,
              label="Unsafe miss-based", zorder=2)
    axis.plot([100 * row["uptime"] for row in manual],
              [row["retentionPct"] for row in manual], color=TEAL, linewidth=3.2,
              marker="o", markersize=6, label="Official EpochBalanced", zorder=3)
    axis.text(22, 8, "Model retains 0%", color=TEAL, fontsize=8.2,
              fontweight="bold", ha="center",
              bbox={"boxstyle": "round,pad=0.2", "facecolor": PANEL, "edgecolor": "none"})
    axis.text(22, 29, "Unsafe retains 37%", color=CORAL, fontsize=8.2,
              fontweight="bold", ha="center",
              bbox={"boxstyle": "round,pad=0.2", "facecolor": PANEL, "edgecolor": "none"})
    axis.set_xlim(-4, 104)
    axis.set_ylim(-4, 106)
    axis.set_xticks([0, 50, 100])
    axis.set_yticks([0, 50, 100])
    axis.xaxis.set_major_formatter(PercentFormatter(100))
    axis.yaxis.set_major_formatter(PercentFormatter(100))
    axis.set_xlabel("Local SLA", color=INK, fontsize=9, fontweight="bold", labelpad=2)
    axis.set_ylabel("Score retained", color=INK, fontsize=9, fontweight="bold", labelpad=2)
    axis.grid(color=GRID, linewidth=0.8, alpha=0.9)
    axis.spines[["top", "right", "left"]].set_visible(False)
    axis.tick_params(axis="both", colors=INK, labelsize=8.5, length=0)
    axis.legend(loc="lower center", bbox_to_anchor=(0.5, 1.01), ncol=2, frameon=False,
                fontsize=7.4, handlelength=2, columnspacing=1.2)

    rounded_box(canvas, 0.382, 0.205, 0.237, 0.052, TEAL_PALE, radius=0.01)
    canvas.text(0.5005, 0.231, "48 skill points  x  75% SLA  =  36 local points",
                color=INK, fontsize=9.2, fontweight="bold", ha="center", va="center")


def draw_balance_panel(canvas: plt.Axes, results: dict) -> None:
    x, y, width, height = 0.663, 0.175, 0.282, 0.425
    rounded_box(canvas, x, y, width, height, PANEL, edge=GRID, radius=0.018)
    panel_header(canvas, 0.683, "3", "Balance is rewarded", "Skill points before SLA; maximum is 100.")

    canvas.text(0.683, 0.485, "TEAM PROFILE", color=MUTED, fontsize=8.6,
                fontweight="bold")
    canvas.text(0.828, 0.485, "50/50\nCONTROL", color=BLUE, fontsize=8.2,
                fontweight="bold", ha="center", va="center", linespacing=1.05)
    canvas.text(0.902, 0.485, "40/40/20\nOFFICIAL", color=TEAL, fontsize=8.2,
                fontweight="bold", ha="center", va="center", linespacing=1.05)

    row_y = [0.42, 0.34, 0.26]
    for row, center_y in zip(results["stakeholderExamples"], row_y):
        label = row["label"].replace(" team", "")
        canvas.text(0.683, center_y + 0.014,
                    f"{label} {row['attack'] * 100:.0f}/{row['defense'] * 100:.0f}",
                    color=INK, fontsize=9.5, fontweight="bold", va="center")
        canvas.text(0.683, center_y - 0.014, "attack / defense", color=MUTED,
                    fontsize=8.5, va="center")
        rounded_box(canvas, 0.803, center_y - 0.03, 0.05, 0.06, BLUE_PALE,
                    edge=BLUE, radius=0.01)
        rounded_box(canvas, 0.877, center_y - 0.03, 0.05, 0.06, TEAL_PALE,
                    edge=TEAL, radius=0.01)
        canvas.text(0.828, center_y, f"{row['arithmeticScore']:.0f}", color=BLUE,
                    fontsize=15, fontweight="bold", ha="center", va="center")
        canvas.text(0.902, center_y, f"{row['balancedScore']:.0f}", color=TEAL,
                    fontsize=15, fontweight="bold", ha="center", va="center")

    rounded_box(canvas, 0.683, 0.184, 0.244, 0.04, GOLD_PALE, radius=0.009)
    canvas.text(
        0.805,
        0.204,
        "Balanced teams stay level;\none-sided teams fall smoothly, not to zero.",
        color=INK,
        fontsize=8.4,
        fontweight="bold",
        ha="center",
        va="center",
        linespacing=1.1,
    )


def draw_footer(canvas: plt.Axes, results: dict) -> None:
    rounded_box(canvas, 0.055, 0.055, 0.89, 0.082, CORAL_PALE, edge=CORAL, radius=0.013)
    canvas.text(0.075, 0.116, "EVIDENCE RULE", color=CORAL, fontsize=9.5,
                fontweight="bold", va="center")
    canvas.text(
        0.075,
        0.083,
        "Accepted flags qualify offense even on checker failure; exact healthy checks alone qualify DEF. "
        "Low k never proves a patch bypass.\n"
        "The startRound roster gives every frozen opponent one common denominator; rarity adds <=25% of C "
        "and <=20 percentage points after clamping.\n"
        "Full epochs weight 1; tail r/n stays fractional. UI details latest 3; totals use all; weights 0.8-1.2.",
        color=INK,
        fontsize=8.0,
        fontweight="bold",
        va="center",
        linespacing=1.15,
    )
    canvas.text(
        0.055,
        0.018,
        f"Source: tools/ad-scoring-sim/results.json  |  seed {results['metadata']['seed']}  |  "
        "technical analysis and 1,000 paired sensitivity trials: REPORT.md",
        color=MUTED,
        fontsize=8.5,
        va="center",
    )


def render(results: dict, output: Path) -> None:
    plt.rcParams.update({
        "font.family": "DejaVu Sans",
        "figure.facecolor": PAPER,
        "savefig.facecolor": PAPER,
    })
    figure = plt.figure(figsize=(16, 9), dpi=150, facecolor=PAPER)
    canvas = figure.add_axes([0, 0, 1, 1], facecolor=PAPER)
    canvas.set_xlim(0, 1)
    canvas.set_ylim(0, 1)
    canvas.axis("off")

    draw_header(canvas)
    draw_formula(canvas)
    draw_pairwise_panel(canvas)
    draw_sla_panel(canvas, figure, results)
    draw_balance_panel(canvas, results)
    draw_footer(canvas, results)

    output.parent.mkdir(parents=True, exist_ok=True)
    buffer = io.BytesIO()
    figure.savefig(buffer, format="png", dpi=150, facecolor=PAPER, transparent=False)
    buffer.seek(0)
    with Image.open(buffer) as rendered:
        rgba = rendered.convert("RGBA")
        opaque = Image.new("RGB", rgba.size, PAPER)
        opaque.paste(rgba, mask=rgba.getchannel("A"))
        metadata = PngInfo()
        metadata.add_text("Title", "Stakeholder view of official EpochBalanced A&D scoring")
        metadata.add_text(
            "Description",
            "Accepted flags, pairwise protected outcomes, local SLA, balanced attack and defense, "
            "weighted partial tails, and settled versus projected totals",
        )
        opaque.save(output, format="PNG", pnginfo=metadata)
    plt.close(figure)


def main() -> None:
    args = parse_args()
    results = load_results(args.input)
    render(results, args.output)
    print(args.output)


if __name__ == "__main__":
    main()
