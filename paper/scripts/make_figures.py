#!/usr/bin/env python3
"""Generate paper figures for the Time Awareness Layer paper.

Run:  python3 paper/scripts/make_figures.py
Outputs paper/figures/*.pdf + *.png (png used by the HTML preview).
"""

import os
import statistics

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.patches import FancyBboxPatch, FancyArrowPatch

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FIG = os.path.join(ROOT, "figures")
os.makedirs(FIG, exist_ok=True)

INK = "#0b2f4f"          # deep sea ink
BLUE = "#1d5f9e"         # primary blue
CYAN = "#38b6d8"         # accent cyan
SAND = "#f2ede4"         # sand paper
GRAY = "#6b7c8f"

plt.rcParams.update({
    "font.family": "Helvetica, Arial, DejaVu Sans",
    "font.size": 11,
    "axes.edgecolor": GRAY,
    "axes.labelcolor": INK,
    "text.color": INK,
    "xtick.color": INK,
    "ytick.color": INK,
})


def box(ax, x, y, w, h, text, fc="#ffffff", ec=BLUE, lw=1.6, fs=10, bold=False):
    ax.add_patch(FancyBboxPatch(
        (x, y), w, h,
        boxstyle="round,pad=0.012,rounding_size=0.014",
        fc=fc, ec=ec, lw=lw,
    ))
    ax.text(x + w / 2, y + h / 2, text, ha="center", va="center",
            fontsize=fs, color=INK, fontweight="bold" if bold else "normal",
            wrap=True)


def arrow(ax, x1, y1, x2, y2, color=CYAN, lw=1.8):
    ax.add_patch(FancyArrowPatch(
        (x1, y1), (x2, y2), arrowstyle="-|>", mutation_scale=14,
        color=color, lw=lw, shrinkA=2, shrinkB=2,
    ))


def fig_architecture():
    fig, ax = plt.subplots(figsize=(10.2, 4.2))
    ax.set_xlim(0, 10.2)
    ax.set_ylim(0, 4.2)
    ax.axis("off")
    fig.patch.set_facecolor("white")

    # Backend / storage column
    box(ax, 0.12, 2.9, 2.05, 0.72, "Session store\n(raw history + ts)", fc="#eaf3fb", fs=9.5)
    box(ax, 0.12, 1.95, 2.05, 0.72, "Tool executor\n(results in flight)", fc="#eaf3fb", fs=9.5)
    box(ax, 0.12, 1.0, 2.05, 0.72, "Context engine\n(1M window, memory)", fc="#eaf3fb", fs=9.5)

    # Middle: Time Awareness Layer
    box(ax, 3.15, 1.45, 2.6, 1.6,
        "Time Awareness Layer\n\nL0 live clock · L1 freshness\nL3 span notes · L4 narrative",
        fc=SAND, ec=INK, lw=2.0, fs=9.8, bold=True)

    # Wire / API
    box(ax, 6.55, 2.55, 1.95, 1.05,
        "stamp_messages_for_wire\n[message_time=… age=…]\n[time_harness now=…]",
        fc="#eef8fb", fs=8.6)
    box(ax, 6.55, 1.15, 1.95, 1.05,
        "annotate_tool_result\n[data_time=… age=0min\n freshness=… horizon=…]",
        fc="#eef8fb", fs=8.6)

    # Model
    box(ax, 8.95, 1.7, 1.05, 1.15, "DeepSeek\nV4", fc=INK, ec=CYAN, fs=11, bold=True)
    ax.text(9.475, 3.05, "API", ha="center", fontsize=9, color=GRAY)

    # Arrows
    arrow(ax, 2.17, 3.26, 3.15, 2.8)
    arrow(ax, 2.17, 2.31, 3.15, 2.35)
    arrow(ax, 2.17, 1.36, 3.15, 1.8)
    arrow(ax, 5.75, 2.75, 6.55, 3.05)
    arrow(ax, 5.75, 2.0, 6.55, 1.7)
    arrow(ax, 8.5, 2.75, 8.95, 2.45)
    arrow(ax, 9.475, 1.7, 9.475, 1.0)
    ax.text(9.475, 0.9, "stamped\nresults", ha="center", fontsize=8, color=GRAY)
    arrow(ax, 8.55, 1.0, 4.4, 1.1, color=GRAY)
    ax.text(6.4, 0.78, "feedback loop: results re-enter history, ages refresh every request",
            ha="center", fontsize=8.5, color=GRAY, style="italic")

    ax.set_title("Request-time temporal grounding: storage stays raw, stamps are derived per request",
                 fontsize=11, color=INK, pad=8)
    fig.tight_layout()
    fig.savefig(os.path.join(FIG, "fig1_architecture.pdf"))
    fig.savefig(os.path.join(FIG, "fig1_architecture.png"), dpi=180)
    plt.close(fig)


def fig_results():
    fig, axes = plt.subplots(1, 2, figsize=(10.0, 3.6), width_ratios=[1.2, 1])
    fig.patch.set_facecolor("white")

    # Panel A: T1 ablation + control, per model
    ax = axes[0]
    models = ["V4 Flash", "V4 Pro"]
    baseline = [0.625, 0.444]
    harness = [0.917, 1.000]
    control = [0.264, 0.306]
    dc_flip = [0.986, 0.986]
    x = [0, 1]
    w = 0.17
    b1 = ax.bar([i - 1.5 * w for i in x], baseline, w, label="baseline", color="#b9c9d9")
    b2 = ax.bar([i - 0.5 * w for i in x], harness, w, label="with TAL", color=CYAN)
    b3 = ax.bar([i + 0.5 * w for i in x], control, w, label="control (flipped)", color="#d98d7a")
    b4 = ax.bar([i + 1.5 * w for i in x], dc_flip, w, label="decision-chain (flipped)", color=INK)
    for bars in (b1, b2, b3, b4):
        for r in bars:
            ax.text(r.get_x() + r.get_width() / 2, r.get_height() + 0.012,
                    f"{r.get_height():.0%}", ha="center", fontsize=8, color=INK)
    ax.set_xticks(x)
    ax.set_xticklabels(models, fontsize=10)
    ax.set_ylim(0, 1.18)
    ax.set_ylabel("T1 accuracy (n=72)")
    ax.set_title("(a) T1: stamps alone vs stamps + decision procedure", fontsize=10)
    ax.legend(fontsize=7.5, frameon=False, loc="upper left", ncol=1)
    ax.axhline(0.5, color=GRAY, ls=":", lw=1.0)
    ax.text(1.48, 0.52, "chance", fontsize=7.5, color=GRAY)
    ax.spines[["top", "right"]].set_visible(False)

    # Panel B: T2 self-duration ratios per model (dots = tasks, bar = median)
    ax = axes[1]
    flash_ratios = [6.32, 4.6, 1.68, 2.27, 8.53, 3.71, 3.44, 3.9, 2.96]
    pro_ratios = [2.23, 0.54, 0.56, 0.39, 1.19, 1.05, 0.41, 0.99, 0.47]
    medians = [statistics.median(flash_ratios), statistics.median(pro_ratios)]
    bars = ax.bar(range(2), medians, 0.5, color=[BLUE, INK], alpha=0.85)
    for r, v in zip(bars, medians):
        ax.text(r.get_x() + r.get_width() / 2, v + 0.18, f"median {v:.2f}×",
                ha="center", fontsize=9, color=INK)
    for xi, ratios in zip(range(2), (flash_ratios, pro_ratios)):
        jitter = [(i % 5 - 2) * 0.045 for i in range(len(ratios))]
        ax.scatter([xi + j for j in jitter], ratios, s=22, color=CYAN, alpha=0.8, zorder=3)
    ax.axhline(1.0, color=GRAY, ls=":", lw=1.2)
    ax.text(1.42, 1.12, "perfect calibration", fontsize=8, color=GRAY)
    ax.set_xticks(range(2))
    ax.set_xticklabels(models, fontsize=10)
    ax.set_ylim(0, 9.5)
    ax.set_ylabel("predicted / actual duration")
    ax.set_title("(b) T2 self-duration: opposite miscalibration", fontsize=10.5)
    ax.spines[["top", "right"]].set_visible(False)

    fig.tight_layout()
    fig.savefig(os.path.join(FIG, "fig2_results.pdf"))
    fig.savefig(os.path.join(FIG, "fig2_results.png"), dpi=180)
    plt.close(fig)


def fig_cache():
    fig, ax = plt.subplots(figsize=(5.4, 3.2))
    fig.patch.set_facecolor("white")
    lengths = ["300 chars\n(60 turns)", "1200 chars\n(60 turns)"]
    no_tal = [0.965, 0.980]
    with_tal = [0.994, 0.995]
    x = [0, 1]
    w = 0.3
    b1 = ax.bar([i - w / 2 for i in x], no_tal, w, label="no TAL", color="#b9c9d9")
    b2 = ax.bar([i + w / 2 for i in x], with_tal, w, label="with TAL", color=CYAN)
    for bars in (b1, b2):
        for r in bars:
            ax.text(r.get_x() + r.get_width() / 2, r.get_height() + 0.004,
                    f"{r.get_height():.1%}", ha="center", fontsize=9, color=INK)
    ax.set_xticks(x)
    ax.set_xticklabels(lengths, fontsize=9)
    ax.set_ylim(0.90, 1.02)
    ax.set_ylabel("share of input served\nfrom prefix cache")
    ax.set_title("Cache preservation under TAL stamps", fontsize=10.5)
    ax.legend(fontsize=8.5, frameon=False, loc="lower right")
    ax.spines[["top", "right"]].set_visible(False)
    fig.tight_layout()
    fig.savefig(os.path.join(FIG, "fig3_cache.pdf"))
    fig.savefig(os.path.join(FIG, "fig3_cache.png"), dpi=180)
    plt.close(fig)


def fig_goalmode():
    """Goal-mode batch audit: turns to completion per goal (3/3 completed)."""
    fig, ax = plt.subplots(figsize=(7.2, 2.7))
    goals = ["fib.py (script)", "stats.py + pytest", "count.sh (shell)"]
    turns = [3, 6, 5]
    bars = ax.barh(range(3), turns, 0.52, color=[CYAN, BLUE, CYAN])
    for r, v in zip(bars, turns):
        ax.text(v + 0.15, r.get_y() + r.get_height() / 2, f"{v} turns",
                va="center", fontsize=10, color=INK)
    ax.set_yticks(range(3))
    ax.set_yticklabels(goals, fontsize=10)
    ax.set_xlim(0, 7.5)
    ax.set_xlabel("auto-advance turns to completion")
    ax.set_title("Goal-mode batch audit: 3/3 completed, all verified before completion",
                 fontsize=11, color=INK)
    ax.spines[["top", "right"]].set_visible(False)
    ax.invert_yaxis()
    fig.tight_layout()
    fig.savefig(os.path.join(FIG, "fig4_goalmode.pdf"))
    fig.savefig(os.path.join(FIG, "fig4_goalmode.png"), dpi=180)
    plt.close(fig)


if __name__ == "__main__":
    fig_architecture()
    fig_results()
    fig_cache()
    fig_goalmode()
    print("figures written to", FIG)
