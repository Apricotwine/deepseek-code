#!/usr/bin/env python3
"""temporal_harness_bench.py — DeepSeek Code 时间感知层 benchmark

回答："把时间感知层（L0 时钟 / L1 新鲜度 / L2 前摄 / L3-4 纵意向性）注入
agent harness 后，DeepSeek V4 Flash 的行为真的改变了吗？"

与参考探针 temporal_harness_probe.py 的区别：
  1. 端点改为 DeepSeek Anthropic 兼容协议（app 实际使用的协议）；
  2. 注解格式复刻 app 的实现（[data_time=... age=... freshness=...] +
     系统提示里的新鲜度规则表）；
  3. 新增 T4（IDE 专属）：编辑文件前是否根据文件内容年龄决定重读。

运行：
  export DS_API_KEY="sk-..."            # 缺省时自动读 app 本地设置
  export DS_MODEL="deepseek-v4-flash"
  python3 temporal_harness_bench.py --ablate --out results.json
"""

import argparse
import json
import os
import pathlib
import re
import statistics
import sys
import time
from datetime import datetime

import requests

API_BASE = os.environ.get("DS_API_BASE", "https://api.deepseek.com/anthropic")
MODEL = os.environ.get("DS_MODEL", "deepseek-v4-flash")
TIMEOUT = 120

# 与 src-tauri/src/context.rs::time_harness_system_section 保持一致
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
    """DeepSeek Anthropic 兼容端点，返回 (text, wall_sec, out_tokens)。"""
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


def age_minutes(fetched: str, now: str) -> float:
    fmt = "%Y-%m-%d %H:%M:%S"
    return (datetime.strptime(now, fmt) - datetime.strptime(fetched, fmt)).total_seconds() / 60.0


def freshness_label(age_min: float) -> str:
    return "high (data almost certainly still valid)" if age_min <= 15 else "stale (data very likely out of date)"


# ---------------------------------------------------------------------------
# T1：工具调用时机对齐（L0 时钟 + L1 新鲜度）
# ---------------------------------------------------------------------------

T1_SYS = (
    "You are an agent deciding whether to call a tool. "
    "Answer exactly 'tool' (must re-fetch fresh data) or 'no_tool' "
    "(previous observation is still valid). No other text."
)

T1_CASES = [
    {"id": "stock_2min", "tool": "get_stock_price", "horizon": "30min",
     "fetched_at": "2026-08-06 09:00:00", "now": "2026-08-06 09:02:00",
     "value": "AAPL = $150.20", "expected": "no_tool"},
    {"id": "stock_48h", "tool": "get_stock_price", "horizon": "30min",
     "fetched_at": "2026-08-04 09:00:00", "now": "2026-08-06 09:00:00",
     "value": "AAPL = $150.20", "expected": "tool"},
    {"id": "weather_10min", "tool": "get_weather", "horizon": "30min",
     "fetched_at": "2026-08-06 08:50:00", "now": "2026-08-06 09:00:00",
     "value": "Beijing: 28C, sunny", "expected": "no_tool"},
    {"id": "shipping_3days", "tool": "track_package", "horizon": "6h",
     "fetched_at": "2026-08-03 10:00:00", "now": "2026-08-06 10:00:00",
     "value": "In transit: Shenzhen -> Shanghai", "expected": "tool"},
    # IDE 专属：编辑前是否重读文件
    {"id": "file_edit_5min", "tool": "read_file", "horizon": "30min",
     "fetched_at": "2026-08-06 08:55:00", "now": "2026-08-06 09:00:00",
     "value": "src/main.rs (342 lines) content...", "expected": "no_tool"},
    {"id": "file_edit_3days", "tool": "read_file", "horizon": "30min",
     "fetched_at": "2026-08-03 09:00:00", "now": "2026-08-06 09:00:00",
     "value": "src/main.rs (342 lines) content...", "expected": "tool"},
]


def _t1_messages(case, with_harness, flip_labels=False):
    lines = [
        f"Previous tool call: {case['tool']} at {case['fetched_at']} returned: {case['value']}.",
        f"Current wall-clock time: {case['now']}.",
    ]
    system = T1_SYS
    if with_harness:
        am = age_minutes(case["fetched_at"], case["now"])
        label = freshness_label(am)
        if flip_labels:
            # Adversarial control: the freshness LABEL contradicts the raw age.
            # Correct behavior is to reason from age (and the horizon) and
            # override the mislabel; following the label is a failure.
            label = "stale (data very likely out of date)" if am <= 15 else "high (data almost certainly still valid)"
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


def run_t1(with_harness):
    correct = 0
    details = []
    for case in T1_CASES:
        text, _, _ = chat(_t1_messages(case, with_harness), max_tokens=10)
        pred = "tool" if "tool" in text.lower() and "no_tool" not in text.lower() else "no_tool"
        ok = pred == case["expected"]
        correct += ok
        details.append({"id": case["id"], "predicted": pred, "expected": case["expected"], "ok": ok})
    return {"accuracy": correct / len(T1_CASES), "cases": details}


def run_t1_control():
    """T1 with the harness, but every freshness label is adversarially flipped.

    The raw age and horizon stay truthful. If the model reasons from the
    timestamp it keeps answering correctly (accuracy stays high); if it
    follows the explicit label, accuracy collapses. Ground truth is defined
    by age semantics, exactly as in the normal condition.
    """
    correct = 0
    details = []
    for case in T1_CASES:
        text, _, _ = chat(_t1_messages(case, True, flip_labels=True), max_tokens=10)
        pred = "tool" if "tool" in text.lower() and "no_tool" not in text.lower() else "no_tool"
        ok = pred == case["expected"]
        correct += ok
        details.append({"id": case["id"], "predicted": pred, "expected": case["expected"], "ok": ok})
    return {"accuracy": correct / len(T1_CASES), "cases": details}


# ---------------------------------------------------------------------------
# T2：自测时长校准（L2 前摄）
# ---------------------------------------------------------------------------

T2_SYS = (
    "You will estimate how long generating your own response takes in wall-clock "
    "seconds (the time from your first output token to your last output token, "
    "NOT counting network latency). First reply with a single number: "
    "'PREDICT: <seconds>'. Then on the next line, complete the task."
)

T2_TASKS = [
    ("Write a 100-word summary of how transformers use attention.", 300),
    ("Write a Python function that detects an anachronism in a sentence and explain it in 5 lines.", 350),
    ("List 10 steps to debug a failing unit test, each step one sentence.", 250),
]


def run_t2():
    ratios = []
    details = []
    for task, budget in T2_TASKS:
        prompt = f"Task: {task}\n\nFirst give PREDICT: <seconds>, then do the task."
        try:
            text, wall, out_tokens = chat(
                [{"role": "system", "content": T2_SYS},
                 {"role": "user", "content": prompt}],
                max_tokens=budget,
            )
        except Exception as exc:
            details.append({"task": task[:40], "error": str(exc)})
            continue
        predict_line = next(
            (ln for ln in text.splitlines() if ln.strip().upper().startswith("PREDICT")), ""
        )
        try:
            predicted = float("".join(c for c in predict_line if c.isdigit() or c == "."))
        except ValueError:
            predicted = None
        per_token_sec = wall / max(out_tokens, 1)
        gen_sec_est = per_token_sec * out_tokens
        ratio = (predicted / gen_sec_est) if predicted else None
        ratios.append(ratio)
        details.append({
            "task": task[:40],
            "predicted_sec": predicted,
            "wall_sec": round(wall, 3),
            "out_tokens": out_tokens,
            "per_token_sec": round(per_token_sec, 4),
            "ratio_pred_to_wall": round(ratio, 2) if ratio else None,
        })
    valid = [r for r in ratios if r is not None]
    return {
        "median_ratio": round(statistics.median(valid), 2) if valid else None,
        "n": len(valid),
        "cases": details,
    }


# ---------------------------------------------------------------------------
# T3：时间一致性审计（L3 纵意向性 + L4 叙事层）
# ---------------------------------------------------------------------------

T3_SYS = (
    "You are a temporal auditor. You receive a chronological event log and a set of "
    "claims. For each claim, decide whether it is temporally consistent with the log "
    "(0 = consistent, 1 = inconsistent / relying on stale or wrongly ordered info). "
    "Reply as a JSON list of 0/1, e.g. [0,1]. No other text."
)

T3_LOGS = [
    {
        "id": "planted_ordering_bug",
        "now": "2026-08-06 09:10:00",
        "last_entry_age_days": 0,
        "log": [
            ("09:00", "read config"),
            ("09:02", "run tests -> FAIL"),
            ("09:05", "fix bug in main.py"),
            ("09:07", "run tests -> PASS"),
            ("09:09", "commit"),
        ],
        "claims": [
            "tests passed BEFORE the bug was fixed",
            "the commit happened after tests passed",
        ],
        "inconsistencies": [0],
    },
    {
        "id": "planted_staleness_bug",
        "now": "2026-08-06 10:00:00",
        "last_entry_age_days": 17,
        "log": [
            ("07-20 10:00", "deploy v2 to production"),
            ("07-20 10:05", "health check -> OK"),
        ],
        "claims": [
            "production is healthy, verified 17 days ago; no need to re-check",
            "the last health check was on 07-20",
        ],
        "inconsistencies": [0],
    },
    {
        "id": "consistent_log",
        "now": "2026-08-06 09:10:00",
        "last_entry_age_days": 0,
        "log": [
            ("09:00", "read config"),
            ("09:02", "run tests -> FAIL"),
            ("09:05", "fix bug in main.py"),
            ("09:07", "run tests -> PASS"),
            ("09:09", "commit"),
        ],
        "claims": [
            "tests passed after the bug was fixed",
            "the commit happened after tests passed",
        ],
        "inconsistencies": [],
    },
]


def _t3_messages(log_case, with_harness):
    log_text = "\n".join(f"{t}  {e}" for t, e in log_case["log"])
    claims_text = "\n".join(f"{i}: {c}" for i, c in enumerate(log_case["claims"]))
    prompt = f"Event log:\n{log_text}\n\nClaims:\n{claims_text}\n\nVerdicts:"
    system = T3_SYS
    if with_harness:
        prompt += (
            f"\n[harness note: log entries are ordered chronologically; "
            f"the latest relevant entry is {log_case['last_entry_age_days']} day(s) old "
            f"(freshness horizon 24h). Any claim asserting something is still valid "
            f"based on data older than the horizon is stale → mark it 1.]"
        )
    return [
        {"role": "system", "content": system},
        {"role": "user", "content": prompt},
    ]


def run_t3(with_harness):
    hit = 0
    total = 0
    details = []
    for log_case in T3_LOGS:
        text, _, _ = chat(_t3_messages(log_case, with_harness), max_tokens=60)
        try:
            pred = json.loads(text.strip())
            if not isinstance(pred, list):
                raise ValueError
        except Exception:
            details.append({"id": log_case["id"], "parse_error": text[:120]})
            continue
        truth = [1 if i in log_case["inconsistencies"] else 0 for i in range(len(log_case["claims"]))]
        for p, t in zip(pred, truth):
            total += 1
            if int(p) == t:
                hit += 1
        details.append({"id": log_case["id"], "predicted": pred, "truth": truth})
    return {
        "claim_accuracy": round(hit / total, 3) if total else None,
        "n_claims": total,
        "cases": details,
    }


# ---------------------------------------------------------------------------
# 矩阵探针（来自 IDE 自评报告的测试矩阵）：T5 时钟一致性、T6 假新鲜陷阱、
# T7 混合保鲜期、T8 陈旧自纠错
# ---------------------------------------------------------------------------

T5_PROMPT = """A context contains two wall-clock hints:
- system_prompt_clock: 2026-08-06 15:30:33 (frozen at session start)
- message_stamp_clock: 2026-08-06 16:31:33 (stamped on the current user message)

Question: What is the current time? If you notice a conflict, say so explicitly.
Reply as JSON: {{"time": "HH:MM:SS", "conflict": true/false, "authoritative": "..."}}"""


def run_t5():
    """时钟一致性：植入两个差 1 小时的时钟，看模型是否发现并选对权威时钟。"""
    text, _, _ = chat(
        [{"role": "system", "content": "You are a temporal auditor."},
         {"role": "user", "content": T5_PROMPT}],
        max_tokens=120,
    )
    import json as _json
    try:
        d = _json.loads(text.strip())
        time_ok = d.get("time", "").startswith("16:31")
        conflict_ok = d.get("conflict") is True
        authoritative_ok = "message" in str(d.get("authoritative", ""))
    except Exception:
        time_ok = conflict_ok = authoritative_ok = False
        d = {"raw": text[:160]}
    return {
        "time_correct": time_ok,
        "conflict_detected": conflict_ok,
        "authoritative_picked": authoritative_ok,
        "all_ok": time_ok and conflict_ok and authoritative_ok,
        "response": d,
    }


T6_CASE = {
    "id": "fake_fresh_moji",
    "tool_result": "Beijing weather: 8-17C, partly cloudy, humidity 68%",
    "raw_url": "https://example.weather/moji?r=1504921726666",
    "note": "r=1504921726666 decodes to 2017-09-09 (epoch ms)",
}

T6_HARNESS_RESULT = (
    "[data_time=2026-08-06 16:00:00 age=0min freshness=just_fetched horizon=24h]\n"
    + T6_CASE["tool_result"]
    + "\nurl: " + T6_CASE["raw_url"]
)

T6_SYS = (
    "You answer user questions from tool results. "
    "If content-embedded evidence (timestamps, cache parameters) contradicts "
    "the fetch time, prefer the content evidence and flag it."
)


def _t6_prompt(with_harness):
    lines = []
    if with_harness:
        lines.append(T6_HARNESS_RESULT)
        lines.append("")
        lines.append(T6_CASE["note"])
    else:
        lines.append(T6_CASE["tool_result"])
        lines.append("url: " + T6_CASE["raw_url"])
    lines.append("")
    lines.append("User asks: 'What is the weather in Beijing right now?'")
    return "\n".join(lines)


def _flag_stale(text):
    lower = text.lower()
    return ("2017" in text or "9 year" in lower or "stale" in lower
            or "old" in lower and "cache" in lower or "out of date" in lower
            or "过期" in text or "陈旧" in text)


def run_t6(with_harness):
    """假新鲜陷阱：标注 fresh 但内容自带 2017 年缓存证据，模型是否盲信。"""
    text, _, _ = chat(
        [{"role": "system", "content": T6_SYS},
         {"role": "user", "content": _t6_prompt(with_harness)}],
        max_tokens=200,
    )
    return {"flagged_stale": _flag_stale(text), "response": text[:200]}


T7_CASE = """You have three observations and must decide which need re-fetching:
1) Stock quote AAPL=$150.20, fetched 2026-08-06 15:20:00 (40 min ago), horizon 30min.
2) Web page on API docs, fetched 2026-08-06 13:00:00 (3 h ago), horizon 24h.
3) File src/main.rs content, read 2026-08-03 09:00:00 (3 days ago), file contents have no expiry.
The current wall-clock time is 2026-08-06 16:00:00.

Which observations MUST be re-fetched before answering a question about the
CURRENT value/state? Reply as JSON: {"refetch": [indices]}. Only include
observations whose data is stale beyond its horizon or where the user asks
about current state."""


def run_t7():
    """混合保鲜期：股价(30min) + 网页(24h) + 文件(无过期) 分层应用是否正确。"""
    import json as _json
    text, _, _ = chat(
        [{"role": "system", "content": "You decide freshness using per-tool horizons."},
         {"role": "user", "content": T7_CASE}],
        max_tokens=80,
    )
    try:
        d = _json.loads(text.strip())
        refetch = sorted(int(i) for i in d.get("refetch", []))
    except Exception:
        refetch = None
        d = {"raw": text[:120]}
    return {
        "refetch": refetch,
        "expected": [1],
        "ok": refetch == [1],
        "response": d,
    }


T8_CASE = """You are continuing a session resumed after 30 hours.
Yesterday's turn (2026-08-05 09:00) fetched live package tracking and concluded:
"Package will arrive tomorrow." Today's turn (2026-08-06 16:00) has a new message.
The package tracking observation is now 31 hours old (horizon 6h).
What should you do before repeating or relying on yesterday's conclusion?
Reply as JSON: {"action": "refetch"|"reuse"|"flag", "reason": "..."}"""


def run_t8():
    """陈旧自纠错：跨天恢复后，昨日结论是否自动触发重查而非照搬。"""
    import json as _json
    text, _, _ = chat(
        [{"role": "system", "content": "You are a careful agent. Stale claims must be re-verified."},
         {"role": "user", "content": T8_CASE}],
        max_tokens=100,
    )
    try:
        d = _json.loads(text.strip())
        action = d.get("action", "")
        ok = action in ("refetch", "flag")
    except Exception:
        ok = False
        d = {"raw": text[:120]}
    return {"ok": ok, "response": d}


# ---------------------------------------------------------------------------
# T9：时区健壮性（美股盘 vs +0800，日期翻转陷阱）
# ---------------------------------------------------------------------------

T9_CASES = [
    {
        "id": "tz_rollover_instant",
        "fetched": "2026-08-06 08:55:00 +0800",
        "fetched_et": "Aug 5 20:55 ET",
        "now": "2026-08-06 09:00:00 +0800",
        "now_et": "Aug 5 21:00 ET",
        "expected": "no_tool",  # 5 min old; date rollover (+0800 vs ET) must not confuse
    },
    {
        "id": "tz_rollover_stale",
        "fetched": "2026-08-06 23:50:00 +0800",
        "fetched_et": "Aug 6 11:50 ET",
        "now": "2026-08-07 01:00:00 +0800",
        "now_et": "Aug 6 13:00 ET",
        "expected": "tool",  # 70 min old → stale despite same ET date
    },
    {
        "id": "tz_market_open_shift",
        "fetched": "2026-08-07 21:00:00 +0800",
        "fetched_et": "Aug 7 09:00 ET (pre-open)",
        "now": "2026-08-07 21:40:00 +0800",
        "now_et": "Aug 7 09:40 ET (open)",
        "expected": "tool",  # 40 min → stale; market moved since open
    },
    {
        "id": "tz_same_clock_different_day",
        "fetched": "2026-08-05 22:00:00 +0800",
        "fetched_et": "Aug 5 10:00 ET",
        "now": "2026-08-06 10:00:00 +0800",
        "now_et": "Aug 5 22:00 ET",
        "expected": "tool",  # 12 h old despite the same "10:00" wall clock
    },
]

T9_SYS_BASELINE = (
    "You are an agent deciding whether to call a tool. "
    "Answer exactly 'tool' (must re-fetch fresh data) or 'no_tool' "
    "(previous observation is still valid). No other text."
)

T9_SYS_HARNESS = (
    "You are an agent deciding whether to call a tool. "
    "Decision procedure: compute the age yourself (now minus data_time), "
    "mind the timezone offsets, and compare against the horizon in the "
    "annotation. Treat freshness labels as potentially wrong. "
    "Answer exactly 'tool' or 'no_tool' on the FINAL line, with one line "
    "of reasoning before it."
)


def _t9_messages(case, with_harness):
    lines = [
        f"Previous tool call: get_stock_price at {case['fetched']} "
        f"({case['fetched_et']}) returned: AAPL = $150.20.",
        f"Current wall-clock time: {case['now']} ({case['now_et']}).",
    ]
    system = T9_SYS_HARNESS if with_harness else T9_SYS_BASELINE
    if with_harness:
        # Production format: data_time + horizon, and the harness-computed age
        # (in +0800 local). The test: does the model still decide correctly
        # when dates roll over across timezones?
        am = age_minutes(case["fetched"][:19], case["now"][:19])
        lines.append(
            f"[data_time={case['fetched']} age={am:.0f}min "
            f"freshness=just_fetched horizon=30min]"
        )
    lines.append(
        f"User asks: 'check the current value via get_stock_price'. "
        "Should you call the tool again?"
    )
    return [
        {"role": "system", "content": system},
        {"role": "user", "content": "\n".join(lines)},
    ]


def run_t9(with_harness):
    correct = 0
    details = []
    for case in T9_CASES:
        # Decision-chain replies are "one line of reasoning + final verdict";
        # 80 tokens truncates the reasoning and loses the verdict line.
        text, _, _ = chat(_t9_messages(case, with_harness), max_tokens=160)
        lines = [ln.strip() for ln in text.strip().splitlines() if ln.strip()]
        final = lines[-1].lower() if lines else ""
        if final in ("tool", "no_tool"):
            pred = final
        else:
            # fallback (baseline replies without reasoning)
            pred = "tool" if "tool" in text.lower() and "no_tool" not in text.lower() else "no_tool"
        ok = pred == case["expected"]
        correct += ok
        details.append({
            "id": case["id"],
            "predicted": pred,
            "expected": case["expected"],
            "ok": ok,
            "reply": text[:220],
        })
    return {"accuracy": correct / len(T9_CASES), "cases": details}


def run_matrix():
    result = {}
    print("[T5] 时钟一致性（双时钟冲突检测）")
    result["t5_clock_consistency"] = run_t5()
    for cond, flag in (("with_time_stack", True), ("baseline", False)):
        print(f"[T6] 假新鲜陷阱 — {cond}")
        result[f"t6_fake_fresh_{cond}"] = run_t6(flag)
    print("[T7] 混合保鲜期")
    result["t7_mixed_horizons"] = run_t7()
    print("[T8] 陈旧自纠错")
    result["t8_stale_self_correction"] = run_t8()
    for cond, flag in (("with_time_stack", True), ("baseline", False)):
        print(f"[T9] 时区健壮性 — {cond}")
        result[f"t9_timezone_{cond}"] = run_t9(flag)
    return result


# ---------------------------------------------------------------------------
# 主流程
# ---------------------------------------------------------------------------


def main():
    ap = argparse.ArgumentParser(description="DeepSeek Code 时间感知层 benchmark")
    ap.add_argument("--ablate", action="store_true", help="同时跑 有/无 时间层两个条件")
    ap.add_argument("--matrix", action="store_true", help="跑 IDE 自评报告的测试矩阵探针 (T5-T8)")
    ap.add_argument("--out", default="temporal_harness_result.json")
    args = ap.parse_args()

    if not api_key():
        print("[错误] 未找到 DS_API_KEY，且 app 本地设置文件也没有 key。", file=sys.stderr)
        sys.exit(4)

    print(f"model    : {MODEL}")
    print(f"api base : {API_BASE}")
    print()

    result = {"model": MODEL, "api_base": API_BASE, "probes": {}}

    if args.ablate:
        for cond, flag in (("with_time_stack", True), ("baseline", False)):
            print(f"[T1] 工具调用时机对齐 — {cond}")
            result["probes"][f"t1_{cond}"] = run_t1(flag)
        print("[T1] 对抗性对照组 — 标签翻转（时间戳仍真实）")
        result["probes"]["t1_control_label_flip"] = run_t1_control()
        print("[T2] 自测时长校准")
        result["probes"]["t2"] = run_t2()
        for cond, flag in (("with_time_stack", True), ("baseline", False)):
            print(f"[T3] 时间一致性审计 — {cond}")
            result["probes"][f"t3_{cond}"] = run_t3(flag)

        a1 = result["probes"]["t1_with_time_stack"]["accuracy"]
        b1 = result["probes"]["t1_baseline"]["accuracy"]
        a3 = result["probes"]["t3_with_time_stack"]["claim_accuracy"]
        b3 = result["probes"]["t3_baseline"]["claim_accuracy"]
        result["ablation_delta"] = {
            "t1_tool_alignment_delta": round(a1 - b1, 3),
            "t3_temporal_critique_delta": round(a3 - b3, 3),
        }
        print(f"\nablation: T1 {a1 - b1:+.3f}  T3 {a3 - b3:+.3f}")
    else:
        result["probes"]["t1"] = run_t1(True)
        result["probes"]["t2"] = run_t2()
        result["probes"]["t3"] = run_t3(True)

    if args.matrix:
        matrix_out = args.out.replace(".json", "_matrix.json")
        matrix = run_matrix()
        with open(matrix_out, "w", encoding="utf-8") as f:
            json.dump(matrix, f, ensure_ascii=False, indent=2)
        print(f"\n矩阵结果已写入 {matrix_out}")

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
