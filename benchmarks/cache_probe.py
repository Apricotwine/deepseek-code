#!/usr/bin/env python3
"""cache_probe.py — does the Time Awareness Layer preserve DeepSeek prefix cache?

Experiment design:
  A (no TAL):  send conversation M1..M50, then send M1..M50 + new turn.
  B (with TAL): send stamped(M1..M50), then send stamped(M1..M50) + new turn
               (stamps are bucket-stable; only the new tail carries a fresh clock).

If the paper's cache-conscious-placement claim holds, the cache_read_input_tokens
fraction on the second call should be high and similar across A and B; if per-turn
clock refresh in the system prompt were used instead, B would collapse toward 0.

Usage:  python3 cache_probe.py [--model deepseek-v4-flash] [--turns 50]
"""

import argparse
import json
import os
import pathlib
import re
import time

import requests

API_BASE = os.environ.get("DS_API_BASE", "https://api.deepseek.com/anthropic")


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


def chat(model, messages, max_tokens=24):
    url = API_BASE.rstrip("/") + "/v1/messages"
    headers = {
        "x-api-key": api_key(),
        "anthropic-version": "2023-06-01",
        "Content-Type": "application/json",
    }
    payload = {
        "model": model,
        "messages": messages,
        "max_tokens": max_tokens,
        "temperature": 0,
        "thinking": {"type": "disabled"},
    }
    resp = requests.post(url, headers=headers, json=payload, timeout=180)
    resp.raise_for_status()
    return resp.json()


def build_conversation(n_turns: int, stamp: bool, msg_len: int = 300,
                       now: str = "2026-08-06 09:00:00"):
    """Deterministic M1..M50 conversation. Stamped variant mirrors the app's
    stamp_messages_for_wire output (bucketed ages, span note, end-of-input clock)."""
    span = (
        "[time_harness: this conversation spans from 2d ago. Live data mentioned in "
        "older messages may be stale — re-verify per the freshness rules before "
        "relying on it.]\n"
    )
    messages = []
    for i in range(n_turns):
        role = "user" if i % 2 == 0 else "assistant"
        if role == "user":
            body = (f"Task {i}: check the status of module {i % 9} and summarize findings. "
                    + "Review the recent build output, verify the failing tests, and cross-check "
                    + "the dependency versions before proposing a fix. " * (msg_len // 130))
        else:
            body = (f"Module {i % 9} is healthy; build passed at iteration {i}. No action needed. "
                    + "The last full test run completed without regressions, and the artifact cache "
                    + "was reused. " * (msg_len // 130))
        if stamp:
            age = "2d" if i < n_turns - 2 else "0min"
            mtime = "2026-08-04 09:00:00" if i < n_turns - 2 else "2026-08-06 09:00:00"
            if role == "assistant" and i == n_turns - 1:
                body = f"[message_time={mtime} age={age}] {body} [time_harness now={now}]"
            else:
                body = f"[message_time={mtime} age={age}] {body}"
            if i == 0:
                body = span + body
        messages.append({"role": role, "content": body})
    return messages


def usage_fraction(data: dict):
    u = data.get("usage") or {}
    inp = u.get("input_tokens", 0)
    cached = u.get("cache_read_input_tokens", 0)
    total = cached + inp
    return cached, inp, round(cached / total, 4) if total else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default=os.environ.get("DS_MODEL", "deepseek-v4-flash"))
    ap.add_argument("--turns", type=int, default=50)
    ap.add_argument("--msg-len", type=int, default=300,
                    help="approximate per-message character length")
    ap.add_argument("--out", default="cache_probe_result.json")
    args = ap.parse_args()

    if not api_key():
        raise SystemExit("no API key found")

    results = {"model": args.model, "turns": args.turns, "msg_len": args.msg_len, "conditions": {}}

    for cond, stamp in (("no_tal", False), ("with_tal", True)):
        print(f"\n[{cond}] building {args.turns}-turn conversation (stamp={stamp}) ...")
        msgs = build_conversation(args.turns, stamp, msg_len=args.msg_len)
        warm_tail = {"role": "user", "content": "Warm the cache."}
        if stamp:
            warm_tail = {"role": "user", "content": "[time_harness now=2026-08-06 09:00:00] Warm the cache."}
        next_tail = {"role": "user", "content": "Next turn: check module 7 and report."}
        if stamp:
            next_tail = {"role": "user", "content": "[time_harness now=2026-08-06 09:05:00] Next turn: check module 7 and report."}
        call1 = msgs + [warm_tail]
        call2 = msgs + [next_tail]

        print("  call 1 (warm cache with prefix)...")
        r1 = chat(args.model, call1)
        c1, i1, f1 = usage_fraction(r1)
        print(f"  call 1: input={i1} cache_read={c1} fraction={f1}")

        time.sleep(0.5)
        print("  call 2 (same prefix, new tail)...")
        r2 = chat(args.model, call2)
        c2, i2, f2 = usage_fraction(r2)
        print(f"  call 2: input={i2} cache_read={c2} fraction={f2}")

        results["conditions"][cond] = {
            "call1": {"input_tokens": i1, "cache_read_input_tokens": c1, "fraction": f1,
                      "raw_usage": r1.get("usage")},
            "call2": {"input_tokens": i2, "cache_read_input_tokens": c2, "fraction": f2,
                      "raw_usage": r2.get("usage")},
        }

    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(results, f, ensure_ascii=False, indent=2)
    print(f"\nresults written to {args.out}")


if __name__ == "__main__":
    main()
