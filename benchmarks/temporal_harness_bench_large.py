#!/usr/bin/env python3
"""temporal_harness_bench_large.py — scalable Time Awareness Layer benchmark.

Same probe family as temporal_harness_bench.py (T1 tool alignment / T2
self-duration / T3 temporal audit), but cases are programmatically generated:

  - T1: n cases per seed, ages drawn log-uniformly against per-tool horizons
        (fresh 0.08–0.9x horizon, stale 1.1–100x, plus exact-boundary cases).
  - T3: n logs with 3 claims each; planted ordering / staleness bugs cycle
        deterministically; ground truth is computed, never hand-labeled.
  - T2: 9 generation tasks across short/medium/long budgets.

The freshness LABEL is now horizon-aware (the v1–v3 fixed cases happened to be
consistent with a hardcoded 15-min cutoff; generated cases require comparing
age against the per-case horizon).

Conditions per T1/T3: baseline (no TAL), with TAL, and an adversarial
label-flip control for T1 (freshness labels contradicted; age/horizon honest).

Usage:
  export DS_MODEL="deepseek-v4-flash"          # or deepseek-v4-pro
  python3 temporal_harness_bench_large.py \
      --t1 36 --t3 12 --t2 9 --seeds 7 11 --workers 4 --out result_large.json
"""

import argparse
import concurrent.futures as cf
import json
import math
import os
import pathlib
import random
import re
import statistics
import sys
import time
from datetime import datetime, timedelta

import requests

API_BASE = os.environ.get("DS_API_BASE", "https://api.deepseek.com/anthropic")
MODEL = os.environ.get("DS_MODEL", "deepseek-v4-flash")
TIMEOUT = 120

# Mirrors src-tauri/src/context.rs::time_harness_system_section
HARNESS_SYSTEM = (
    "Every tool result carries a [data_time=...] annotation marking the moment "
    "the data was produced. Follow these rules:\n"
    "- Data within its freshness horizon is still valid — reuse it, do not re-query.\n"
    "- Data older than the horizon is STALE — re-fetch before presenting it, and "
    "never present stale data as current.\n"
    "- Freshness horizons: stock/weather quotes ≈ 15–30 min; package tracking ≈ 6 h; "
    "web search ≈ 24 h; shell/system state ≈ 1 h; file contents have no expiry "
    "unless the user asks about current state (then re-read the file).\n"
    "- If a claim asserts that something is still valid based on an observation "
    "older than its horizon, that claim is stale — flag it instead of repeating it."
)

T1_SYS = (
    "You are an agent deciding whether to call a tool. "
    "Answer exactly 'tool' (must re-fetch fresh data) or 'no_tool' "
    "(previous observation is still valid). No other text."
)

T1_DC_SYS = (
    "You are an agent deciding whether to call a tool. The freshness label in the "
    "annotation, if present, MAY BE WRONG — verify it yourself.\n"
    "Decision procedure:\n"
    "1. Compute the data age: current wall-clock time minus data_time.\n"
    "2. Compare the age against the horizon in the annotation.\n"
    "3. If age is within the horizon, the data is still valid: reuse it (no_tool). "
    "If age meets or exceeds the horizon, re-fetch (tool).\n"
    "4. State your reasoning in ONE line, then give your verdict on the FINAL "
    "line as exactly 'tool' or 'no_tool'."
)

T2_SYS = (
    "You will estimate how long generating your own response takes in wall-clock "
    "seconds (the time from your first output token to your last output token, "
    "NOT counting network latency). First reply with a single number: "
    "'PREDICT: <seconds>'. Then on the next line, complete the task."
)

T3_SYS = (
    "You are a temporal auditor. You receive a chronological event log and a set of "
    "claims. For each claim, decide whether it is temporally consistent with the log "
    "(0 = consistent, 1 = inconsistent / relying on stale or wrongly ordered info). "
    "Reply as a JSON list of 0/1, e.g. [0,1]. No other text."
)

TOOLS = [
    ("get_stock_price", 30, lambda i: f"AAPL = ${150.0 + i * 0.31:.2f}"),
    ("get_weather", 30, lambda i: f"Beijing: {20 + i % 9}C, {'sunny' if i % 2 else 'cloudy'}"),
    ("track_package", 360, lambda i: f"In transit: Shenzhen -> Shanghai (order #{i:04d})"),
    ("web_search", 1440, lambda i: f"top result for query {i}: ... (page {1 + i % 5})"),
    ("run_shell", 60, lambda i: f"$ ./build.sh -> exit 0 ({i} warnings)"),
    ("read_file", 30, lambda i: f"src/mod{i % 7}.rs ({100 + i * 13} lines): fn main() {{ ... }}"),
]
TOOL_HORIZON = {name: h for name, h, _ in TOOLS}


def api_key() -> str:
    k = os.environ.get("DS_API_KEY", "")
    if k:
        return k
    p = pathlib.Path.home() / "Library/Application Support/com.deepseek.code/settings.json"
    if p.exists():
        m = re.search(r'"api_key"\s*:\s*"([^"]+)"', p.read_text(encoding="utf-8"))
        if m:
            return m.group(1)
    return ""


def chat(messages, max_tokens=800):
    url = API_BASE.rstrip("/") + "/v1/messages"
    headers = {
        "x-api-key": api_key(),
        "anthropic-version": "2023-06-01",
        "Content-Type": "application/json",
    }
    payload = {
        "model": MODEL,
        "messages": messages,
        "max_tokens": max_tokens,
        "temperature": 0,
        "thinking": {"type": "disabled"},
    }
    t0 = time.monotonic()
    resp = requests.post(url, headers=headers, json=payload, timeout=TIMEOUT)
    wall = time.monotonic() - t0
    resp.raise_for_status()
    data = resp.json()
    text = "".join(
        b.get("text", "") for b in data.get("content", []) if b.get("type") == "text"
    )
    usage = data.get("usage") or {}
    out = usage.get("output_tokens", max(len(text) // 4, 1))
    return text.strip(), wall, out


def parse_time(s: str) -> datetime:
    return datetime.strptime(s, "%Y-%m-%d %H:%M:%S")


def fmt_time(dt: datetime) -> str:
    return dt.strftime("%Y-%m-%d %H:%M:%S")


def age_minutes(fetched: str, now: str) -> float:
    return (parse_time(now) - parse_time(fetched)).total_seconds() / 60.0


def freshness_label(age_min: float, horizon_min: int) -> str:
    if age_min < horizon_min:
        return "high (data almost certainly still valid)"
    return "stale (data very likely out of date)"


# ---------------------------------------------------------------------------
# Generators
# ---------------------------------------------------------------------------


def generate_t1_cases(n: int, seed: int, now: str = "2026-08-06 09:00:00"):
    rng = random.Random(seed)
    now_dt = parse_time(now)
    cases = []
    for i in range(n):
        tool, horizon, val_fn = TOOLS[i % len(TOOLS)]
        mode = i % 3
        if mode == 0:      # fresh: 0.08–0.9x horizon
            mult = math.exp(rng.uniform(math.log(0.08), math.log(0.9)))
            expected = "no_tool"
        elif mode == 1:    # stale: 1.1–100x horizon
            mult = math.exp(rng.uniform(math.log(1.1), math.log(100)))
            expected = "tool"
        else:              # exact boundary: age == horizon -> re-fetch
            mult = 1.0
            expected = "tool"
        age_min = max(1.0, horizon * mult)
        fetched = fmt_time(now_dt - timedelta(minutes=age_min))
        cases.append({
            "id": f"gen{i:03d}_{tool}_{int(round(age_min))}min",
            "tool": tool,
            "horizon": f"{horizon}min",
            "fetched_at": fetched,
            "now": now,
            "value": val_fn(i),
            "expected": expected,
        })
    return cases


def generate_t2_tasks():
    return [
        ("Write a 50-word summary of how transformers use attention.", 250),
        ("Write a 100-word summary of the water cycle.", 350),
        ("List 5 ways to reduce Python startup time, one sentence each.", 200),
        ("Write a Python function that detects an anachronism in a sentence and explain it in 5 lines.", 350),
        ("Write a bash script to back up a directory and explain each line.", 350),
        ("Explain the CAP theorem in 200 words.", 300),
        ("List 10 steps to debug a failing unit test, each step one sentence.", 250),
        ("Write a short Rust function that finds the median of a slice, with tests.", 500),
        ("Draft a 3-paragraph project plan for migrating a monorepo to Bazel.", 600),
    ]


def generate_t3_logs(n_logs: int, seed: int):
    rng = random.Random(seed)
    events = [
        "read config", "run tests -> FAIL", "fix bug in main.py",
        "run tests -> PASS", "commit", "deploy to staging",
        "smoke test -> OK", "update docs",
    ]
    logs = []
    for i in range(n_logs):
        bug = ["none", "ordering", "staleness"][i % 3]
        k = rng.randint(4, 6)
        base = parse_time("2026-08-06 09:00:00")
        times = [base + timedelta(minutes=2 * j) for j in range(k)]
        chosen = rng.sample(events, k)
        entries = [(fmt_time(t), e) for t, e in zip(times, chosen)]

        claims = []
        inconsistent = []

        # Claim A: ordering of two chosen events
        a, b = rng.sample(range(k), 2)
        earlier, later = min(a, b), max(a, b)
        if bug == "ordering":
            claims.append(f"{chosen[later]} happened before {chosen[earlier]}")
            inconsistent.append(0)
        else:
            claims.append(f"{chosen[earlier]} happened before {chosen[later]}")

        # Claim B: staleness of the last observation
        last_t, last_e = entries[-1]
        if bug == "staleness":
            old = fmt_time(parse_time(last_t) - timedelta(days=rng.randint(15, 40)))
            entries.append((old, "deploy v2 to production"))
            entries.append((old, "health check -> OK"))
            claims.append("production is healthy, verified days ago; no need to re-check")
            inconsistent.append(1)
        else:
            claims.append(f"the last entry ({last_e}) is current enough to rely on")

        # Claim C: truthful count (always consistent)
        claims.append(f"the log contains {len(entries)} entries")

        logs.append({
            "id": f"gen_log{i:03d}_{bug}",
            "now": "2026-08-06 10:00:00",
            "log": entries,
            "claims": claims,
            "inconsistencies": inconsistent,
        })
    return logs


# ---------------------------------------------------------------------------
# Probes
# ---------------------------------------------------------------------------


def _t1_messages(case, with_harness, flip_labels=False):
    lines = [
        f"Previous tool call: {case['tool']} at {case['fetched_at']} returned: {case['value']}.",
        f"Current wall-clock time: {case['now']}.",
    ]
    system = T1_SYS
    if with_harness:
        am = age_minutes(case["fetched_at"], case["now"])
        horizon = int(re.sub(r"[^0-9]", "", case["horizon"]) or 30)
        label = freshness_label(am, horizon)
        if flip_labels:
            label = (
                "stale (data very likely out of date)"
                if am < horizon
                else "high (data almost certainly still valid)"
            )
        lines.append(
            f"[data_time={case['fetched_at']} age={am:.0f}min "
            f"freshness={label} horizon={case['horizon']}]"
        )
        system += "\n\n" + HARNESS_SYSTEM
    lines.append(
        f"User asks: 'check the current value via {case['tool']}'. "
        "Should you call the tool again?"
    )
    return [
        {"role": "system", "content": system},
        {"role": "user", "content": "\n".join(lines)},
    ]


def _t1_dc_messages(case, flip_labels=False):
    """Decision-chain variant: forces explicit age-vs-horizon computation, and
    tells the model the freshness label may be wrong."""
    lines = [
        f"Previous tool call: {case['tool']} at {case['fetched_at']} returned: {case['value']}.",
        f"Current wall-clock time: {case['now']}.",
    ]
    am = age_minutes(case["fetched_at"], case["now"])
    horizon = int(re.sub(r"[^0-9]", "", case["horizon"]) or 30)
    label = freshness_label(am, horizon)
    if flip_labels:
        label = (
            "stale (data very likely out of date)"
            if am < horizon
            else "high (data almost certainly still valid)"
        )
    lines.append(
        f"[data_time={case['fetched_at']} age={am:.0f}min "
        f"freshness={label} horizon={case['horizon']}]"
    )
    lines.append(
        f"User asks: 'check the current value via {case['tool']}'. "
        "Should you call the tool again?"
    )
    return [
        {"role": "system", "content": T1_SYS + "\n\n" + T1_DC_SYS + "\n\n" + HARNESS_SYSTEM},
        {"role": "user", "content": "\n".join(lines)},
    ]


def _classify(text: str) -> str:
    return "tool" if "tool" in text.lower() and "no_tool" not in text.lower() else "no_tool"


def _classify_verdict_line(text: str) -> str:
    """Take the verdict from the final non-empty line (decision-chain format)."""
    for line in reversed(text.strip().splitlines()):
        line = line.strip().lower()
        if "no_tool" in line or "no tool" in line:
            return "no_tool"
        if line in ("tool",) or line.endswith("tool"):
            return "tool"
    return _classify(text)


def _t3_messages(log_case, with_harness):
    log_text = "\n".join(f"{t}  {e}" for t, e in log_case["log"])
    claims_text = "\n".join(f"{i}: {c}" for i, c in enumerate(log_case["claims"]))
    prompt = f"Event log:\n{log_text}\n\nClaims:\n{claims_text}\n\nVerdicts:"
    system = T3_SYS
    if with_harness:
        prompt += (
            "\n[harness note: log entries are ordered chronologically; "
            "the latest relevant entry is 0 day(s) old unless the log says "
            "otherwise (freshness horizon 24h). Any claim asserting something "
            "is still valid based on data older than the horizon is stale → mark it 1.]"
        )
    return [
        {"role": "system", "content": system},
        {"role": "user", "content": prompt},
    ]


def _parse_verdicts(text: str):
    """Robustly extract a 0/1 verdict list, tolerating ```json fences and prose."""
    t = text.strip()
    t = re.sub(r"^```(?:json)?\s*|\s*```$", "", t)
    try:
        parsed = json.loads(t)
        if isinstance(parsed, list) and all(v in (0, 1) for v in parsed):
            return parsed
    except Exception:
        pass
    m = re.search(r"\[([0-9,\s\[\]]+)\]", t)
    if m:
        try:
            parsed = json.loads("[" + m.group(1) + "]")
            if isinstance(parsed, list) and all(v in (0, 1) for v in parsed):
                return parsed
        except Exception:
            pass
    digits = [int(d) for d in re.findall(r"\b[01]\b", t)]
    return digits if digits else None


def _call_parallel(items, worker, workers):
    """Run `worker(item)` for each item with a bounded thread pool, preserving order."""
    results = [None] * len(items)
    with cf.ThreadPoolExecutor(max_workers=workers) as ex:
        futs = {ex.submit(worker, it): idx for idx, it in enumerate(items)}
        done = 0
        for fut in cf.as_completed(futs):
            idx = futs[fut]
            results[idx] = fut.result()
            done += 1
            if done % 10 == 0 or done == len(items):
                print(f"    {done}/{len(items)} calls done", flush=True)
    return results


def run_t1(cases, with_harness, workers, flip_labels=False):
    def worker(case):
        text, _, _ = chat(_t1_messages(case, with_harness, flip_labels), max_tokens=10)
        return case, _classify(text)

    correct = 0
    details = []
    for case, pred in _call_parallel(cases, worker, workers):
        ok = pred == case["expected"]
        correct += ok
        details.append({"id": case["id"], "predicted": pred, "expected": case["expected"], "ok": ok})
    n = len(cases)
    acc = correct / n
    z = 1.96
    ci = z * math.sqrt(acc * (1 - acc) / n) if n else 0
    return {"accuracy": round(acc, 4), "n": n, "wilson95": round(ci, 4), "cases": details}


def run_t1_dc(cases, workers, flip_labels=False):
    """Decision-chain condition: explicit age-vs-horizon computation with a
    warning that the label may be wrong."""
    def worker(case):
        text, _, _ = chat(_t1_dc_messages(case, flip_labels), max_tokens=120)
        return case, _classify_verdict_line(text)

    correct = 0
    details = []
    for case, pred in _call_parallel(cases, worker, workers):
        ok = pred == case["expected"]
        correct += ok
        details.append({"id": case["id"], "predicted": pred, "expected": case["expected"], "ok": ok})
    n = len(cases)
    acc = correct / n
    z = 1.96
    ci = z * math.sqrt(acc * (1 - acc) / n) if n else 0
    return {"accuracy": round(acc, 4), "n": n, "wilson95": round(ci, 4), "cases": details}


def run_t2(tasks, workers):
    def worker(task):
        prompt = f"Task: {task[0]}\n\nFirst give PREDICT: <seconds>, then do the task."
        try:
            text, wall, out_tokens = chat(
                [{"role": "system", "content": T2_SYS},
                 {"role": "user", "content": prompt}],
                max_tokens=task[1],
            )
        except Exception as exc:
            return {"task": task[0][:50], "error": str(exc)}
        predict_line = next(
            (ln for ln in text.splitlines() if ln.strip().upper().startswith("PREDICT")), ""
        )
        try:
            predicted = float("".join(c for c in predict_line if c.isdigit() or c == "."))
        except ValueError:
            predicted = None
        per_token_sec = wall / max(out_tokens, 1)
        ratio = predicted / (per_token_sec * out_tokens) if predicted else None
        return {
            "task": task[0][:60],
            "predicted_sec": predicted,
            "wall_sec": round(wall, 3),
            "out_tokens": out_tokens,
            "ratio_pred_to_wall": round(ratio, 2) if ratio else None,
        }

    details = _call_parallel(tasks, worker, workers)
    ratios = [d["ratio_pred_to_wall"] for d in details if d.get("ratio_pred_to_wall") is not None]
    return {
        "median_ratio": round(statistics.median(ratios), 2) if ratios else None,
        "mean_ratio": round(statistics.mean(ratios), 2) if ratios else None,
        "n": len(ratios),
        "cases": details,
    }


def run_t3(logs, with_harness, workers):
    def worker(log_case):
        text, _, _ = chat(_t3_messages(log_case, with_harness), max_tokens=60)
        pred = _parse_verdicts(text)
        if pred is None:
            return log_case, None, text[:120]
        if len(pred) != len(log_case["claims"]):
            return log_case, None, f"length mismatch: {pred} vs {len(log_case['claims'])} claims"
        return log_case, pred, None

    hit, total = 0, 0
    details = []
    for log_case, pred, err in _call_parallel(logs, worker, workers):
        if err is not None:
            details.append({"id": log_case["id"], "parse_error": err})
            continue
        truth = [1 if i in log_case["inconsistencies"] else 0
                 for i in range(len(log_case["claims"]))]
        for p, t in zip(pred, truth):
            total += 1
            if int(p) == t:
                hit += 1
        details.append({"id": log_case["id"], "predicted": pred, "truth": truth})
    acc = hit / total if total else None
    z = 1.96
    ci = z * math.sqrt(acc * (1 - acc) / total) if acc is not None and total else 0
    return {"claim_accuracy": round(acc, 4) if acc is not None else None,
            "n_claims": total, "wilson95": round(ci, 4), "cases": details}


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main():
    ap = argparse.ArgumentParser(description="Scalable Time Awareness Layer benchmark")
    ap.add_argument("--t1", type=int, default=36, help="T1 cases per seed")
    ap.add_argument("--t3", type=int, default=12, help="T3 logs per seed (3 claims each)")
    ap.add_argument("--t2", type=int, default=9, help="T2 tasks")
    ap.add_argument("--seeds", type=int, nargs="+", default=[7], help="RNG seeds")
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--out", default="temporal_harness_result_large.json")
    ap.add_argument("--jsonl", default=None, help="optional per-call audit file")
    ap.add_argument("--only", action="append", choices=["t1", "t2", "t3"],
                    help="run only the given probes (repeatable); merge into --out if it exists")
    ap.add_argument("--dc", action="store_true",
                    help="also run the decision-chain T1 variant (explicit age-vs-horizon)")
    args = ap.parse_args()

    if not api_key():
        print("[错误] 未找到 DS_API_KEY，且 app 本地设置文件也没有 key。", file=sys.stderr)
        sys.exit(4)

    print(f"model    : {MODEL}")
    print(f"api base : {API_BASE}")
    print(f"seeds    : {args.seeds}  T1 x {args.t1}  T3 logs x {args.t3}  T2 x {args.t2}")

    t1_cases = [c for seed in args.seeds for c in generate_t1_cases(args.t1, seed)]
    t3_logs = [l for seed in args.seeds for l in generate_t3_logs(args.t3, seed)]
    t2_tasks = generate_t2_tasks()[: args.t2]

    result = {"model": MODEL, "api_base": API_BASE, "seeds": args.seeds,
              "probes": {}, "ablation_delta": {}}
    if os.path.exists(args.out):
        with open(args.out, encoding="utf-8") as f:
            result = json.load(f)

    only = set(args.only or [])
    if not only or "t1" in only:
        print("\n[T1] tool-call timing alignment — with TAL")
        result["probes"]["t1_with_time_stack"] = run_t1(t1_cases, True, args.workers)
        print("[T1] tool-call timing alignment — baseline")
        result["probes"]["t1_baseline"] = run_t1(t1_cases, False, args.workers)
        print("[T1] control — labels flipped, timestamps honest")
        result["probes"]["t1_control_label_flip"] = run_t1(t1_cases, True, args.workers, flip_labels=True)
        if args.dc:
            print("[T1] decision-chain — correct labels")
            result["probes"]["t1_dc_with_time_stack"] = run_t1_dc(t1_cases, args.workers)
            print("[T1] decision-chain — flipped labels")
            result["probes"]["t1_dc_control_label_flip"] = run_t1_dc(t1_cases, args.workers, flip_labels=True)

    if not only or "t2" in only:
        print("[T2] self-duration calibration")
        result["probes"]["t2"] = run_t2(t2_tasks, args.workers)

    if not only or "t3" in only:
        print("[T3] temporal-consistency audit — with TAL")
        result["probes"]["t3_with_time_stack"] = run_t3(t3_logs, True, args.workers)
        print("[T3] temporal-consistency audit — baseline")
        result["probes"]["t3_baseline"] = run_t3(t3_logs, False, args.workers)

    def _get(path):
        d = result["probes"]
        for k in path:
            d = d.get(k) or {}
        return d

    a1 = _get(["t1_with_time_stack", "accuracy"])
    b1 = _get(["t1_baseline", "accuracy"])
    c1 = _get(["t1_control_label_flip", "accuracy"])
    a3 = _get(["t3_with_time_stack", "claim_accuracy"])
    b3 = _get(["t3_baseline", "claim_accuracy"])
    if a1 and b1:
        result["ablation_delta"]["t1_tool_alignment_delta"] = round(a1 - b1, 4)
    if c1 and a1:
        result["ablation_delta"]["t1_control_delta_from_with_harness"] = round(c1 - a1, 4)
    dc1 = _get(["t1_dc_with_time_stack", "accuracy"])
    dcc = _get(["t1_dc_control_label_flip", "accuracy"])
    if dc1 and dcc:
        result["ablation_delta"]["t1_dc_control_delta"] = round(dcc - dc1, 4)
    if a3 and b3:
        result["ablation_delta"]["t3_temporal_critique_delta"] = round(a3 - b3, 4)
    print(f"\nablation: T1 {a1 - b1:+.3f}  T1-control {c1:+.3f} vs harness  T3 {a3 - b3:+.3f}")

    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(result, f, ensure_ascii=False, indent=2)
    print(f"\n结果已写入 {args.out}")


if __name__ == "__main__":
    try:
        main()
    except requests.exceptions.ConnectionError as exc:
        print(f"\n[网络错误] 无法连接 {API_BASE}：{exc}", file=sys.stderr)
        sys.exit(2)
    except requests.exceptions.HTTPError as exc:
        print(f"\n[HTTP 错误] {exc}", file=sys.stderr)
        sys.exit(3)
