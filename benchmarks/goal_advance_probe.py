#!/usr/bin/env python3
"""goal_advance_probe.py — headless verification that goal-mode auto-advance
has fuel: across continuation turns, the model keeps producing tool calls
toward the goal instead of stalling or asking the user.

Simulates the production loop shape:
  turn 1: user goal message (manual)
  turn 2..N: internal continuation trigger (never stored/rendered)
The system prompt mirrors context.rs::time_harness_system_section; the goal
block mirrors goal_section_for_wire. Tools are declared but NOT executed —
we measure the model's *intent* to act (tool_use blocks), which is exactly
what the real agent loop feeds on.

Usage:
  python3 benchmarks/goal_advance_probe.py --turns 3          # intent-only
  python3 benchmarks/goal_advance_probe.py --execute --max-turns 6  # real tools
  python3 benchmarks/goal_advance_probe.py --batch            # stop-condition audit
"""

import argparse
import json
import os
import pathlib
import re
import subprocess
import tempfile
import time

import requests

API_BASE = os.environ.get("DS_API_BASE", "https://api.deepseek.com/anthropic")
MODEL = os.environ.get("DS_MODEL", "deepseek-v4-flash")
TIMEOUT = 120

HARNESS_SYSTEM = (
    "## Time Awareness Layer\n\n"
    "The authoritative wall-clock time is stamped on the current user message as "
    "[time_harness now=...]. Ignore any other time hints unless they are data "
    "timestamps on tool results.\n\n"
    "Every tool result in this conversation carries a [data_time=...] annotation "
    "marking the moment the data was produced. Follow these rules:\n"
    "- Data within its freshness horizon is still valid — reuse it, do not re-query.\n"
    "- Data older than the horizon is STALE — re-fetch before presenting it, and "
    "never present stale data as current.\n"
    "- Decision procedure for every observation: compute the age yourself (now "
    "minus data_time, mind timezone offsets); treat the freshness label as "
    "potentially wrong; age < horizon → reuse, age >= horizon → re-fetch.\n"
    "- Freshness horizons: stock/weather quotes ≈ 15–30 min; package tracking ≈ 6 h; "
    "web search ≈ 24 h; shell/system state ≈ 1 h; file contents have no expiry "
    "unless the user asks about current state (then re-read the file).\n"
    "- You may be auto-continuing (goal mode): keep making concrete progress each "
    "turn; do not stall by asking the user unless truly blocked, and do not loop "
    "on the same step.\n"
)

GOAL = {
    "objective": os.environ.get(
        "GOAL_OBJECTIVE",
        "Write a Python script fib.py that prints the first 10 Fibonacci numbers, "
        "then run it and verify the output.",
    ),
    "status": "active",
    "tokens_used": 0,
}


def goal_section(goal):
    return (
        f"[goal status={goal['status']} tokens_used={goal['tokens_used']}]\n"
        f"Objective: {goal['objective']}\n"
        "Plan: (none yet — call update_plan if the work is multi-step.)\n"
        "Rules:\n"
        "- Work from current evidence: the worktree and external state are authoritative.\n"
        "- Keep the plan current; at most one step in_progress.\n"
        "- Mark the goal complete only after verifying the objective against the current state.\n"
    )


def tools_json():
    # DeepSeek's Anthropic-compatible endpoint expects Anthropic tool format
    # (the app converts OpenAI-style defs to this at request time).
    def tool(name, desc, props, required):
        properties = {}
        for n, t, d in props:
            properties[n] = {"type": t, "description": d}
        return {
            "name": name,
            "description": desc,
            "input_schema": {"type": "object", "properties": properties, "required": required},
        }

    return [
        tool("write_file", "Write content to a file.",
             [("path", "string", "Path"), ("content", "string", "Content")], ["path", "content"]),
        tool("run_shell", "Execute a shell command.",
             [("command", "string", "The shell command")], ["command"]),
        tool("set_goal", "Create or replace the active goal.",
             [("objective", "string", "Objective")], ["objective"]),
        tool("update_goal", "Update goal status.",
             [("status", "string", "complete or blocked")], ["status"]),
        tool("update_plan", "Update the task plan.",
             [("plan", "string", "JSON array of steps")], ["plan"]),
        tool("get_goal", "Read the current goal.", [], []),
    ]


def api_key():
    k = os.environ.get("DS_API_KEY", "")
    if k:
        return k
    p = pathlib.Path.home() / "Library/Application Support/com.deepseek.code/settings.json"
    if p.exists():
        m = re.search(r'"api_key"\s*:\s*"([^"]+)"', p.read_text(encoding="utf-8"))
        if m:
            return m.group(1)
    return ""


def chat(messages, max_tokens=1500):
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
        "tools": tools_json(),
        "thinking": {"type": "disabled"},
    }
    resp = requests.post(url, headers=headers, json=payload, timeout=TIMEOUT)
    resp.raise_for_status()
    data = resp.json()
    text = "".join(b.get("text", "") for b in data.get("content", []) if b.get("type") == "text")
    tool_calls = [
        {"name": b["name"], "args": b.get("input", {})}
        for b in data.get("content", [])
        if b.get("type") == "tool_use"
    ]
    usage = data.get("usage") or {}
    return text.strip(), tool_calls, usage.get("output_tokens", 0)


def would_stop(text):
    t = text.strip()
    if not t:
        return True
    return len(t) < 300 and (t.endswith("?") or t.endswith("？"))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--turns", type=int, default=3)
    ap.add_argument("--execute", action="store_true",
                    help="actually run write_file / run_shell in a temp dir")
    ap.add_argument("--max-turns", type=int, default=6)
    ap.add_argument("--batch", action="store_true",
                    help="stop-condition audit: run several goals, aggregate completion")
    args = ap.parse_args()

    if not api_key():
        print("[错误] 未找到 DS_API_KEY。", file=sys.stderr)
        raise SystemExit(4)

    if args.execute:
        main_exec(args)
        return
    if args.batch:
        main_batch(args)
        return

    now = time.strftime("%Y-%m-%d %H:%M:%S %z")
    messages = [
        {"role": "system", "content": HARNESS_SYSTEM},
        {
            "role": "user",
            "content": f"[time_harness now={now}]\n\n{goal_section(GOAL)}\n\n"
                       f"请完成这个目标：{GOAL['objective']}",
        },
    ]

    print(f"model: {MODEL}  turns: {args.turns}")
    results = []
    for i in range(1, args.turns + 1):
        text, calls, out_tokens = chat(messages)
        assistant_blocks = [{"type": "text", "text": text}]
        for c in calls:
            assistant_blocks.append({
                "type": "tool_use",
                "id": f"tu_{i}_{c['name']}",
                "name": c["name"],
                "input": c["args"],
            })
        messages.append({"role": "assistant", "content": assistant_blocks})
        if calls:
            # Protocol rule: ALL tool_use results must merge into the SINGLE
            # user message immediately after the assistant message.
            messages.append({
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": f"tu_{i}_{c['name']}",
                        "content": "(simulated — not executed in this probe)",
                    }
                    for c in calls
                ],
            })
        row = {
            "turn": i,
            "tool_calls": [c["name"] for c in calls],
            "text_len": len(text),
            "would_stop": would_stop(text),
            "text_tail": text[-120:],
        }
        results.append(row)
        print(f"  turn {i}: tools={row['tool_calls'] or '-'} text_len={row['text_len']} "
              f"would_stop={row['would_stop']}")

        if i == args.turns:
            break
        # Internal continuation trigger (production auto_continuation_message).
        messages.append({
            "role": "user",
            "content": f"[time_harness now={time.strftime('%Y-%m-%d %H:%M:%S %z')}]\n\n"
                       f"（目标模式自动续作）继续推进当前目标：{GOAL['objective']}",
        })

    with open("benchmarks/goal_advance_probe_result.json", "w", encoding="utf-8") as f:
        json.dump({"model": MODEL, "goal": GOAL["objective"], "turns": results}, f,
                  ensure_ascii=False, indent=2)
    print("结果已写入 benchmarks/goal_advance_probe_result.json")


def execute_tool(name, args_dict, workdir, plan_state):
    """Run one tool for real in `workdir`. Returns the result text."""
    if name == "write_file":
        path = args_dict.get("path", "")
        content = args_dict.get("content", "")
        full = pathlib.Path(path)
        if not full.is_absolute():
            full = workdir / full
        full.parent.mkdir(parents=True, exist_ok=True)
        full.write_text(content, encoding="utf-8")
        return f"written {full.name} ({len(content)} chars)"
    if name == "run_shell":
        cmd = args_dict.get("command", "")
        try:
            proc = subprocess.run(
                ["zsh", "-lc", cmd], cwd=str(workdir), capture_output=True,
                text=True, timeout=30,
            )
            out = (proc.stdout or "") + (("\n[stderr]\n" + proc.stderr) if proc.stderr else "")
            return out[-4000:] or f"(exit {proc.returncode}, no output)"
        except Exception as e:
            return f"exec error: {e}"
    if name == "update_plan":
        try:
            incoming = json.loads(args_dict.get("plan", "[]"))
            now = time.strftime("%H:%M:%S")
            for st in incoming:
                step_text = st.get("content") or st.get("step")
                cid = st.get("id") or step_text
                plan_state[cid] = {
                    "content": step_text or "",
                    "status": st.get("status", "pending"),
                    "at": now,
                }
            return f"Plan updated: {len(incoming)} steps -> " + ", ".join(
                f"{(s.get('content') or s.get('step') or '')[:18]}[{s.get('status','?')}]" for s in incoming
            )
        except Exception as e:
            return f"plan parse error: {e}"
    if name == "update_goal":
        return f"Goal status accepted: {args_dict.get('status')}"
    if name == "get_goal":
        return "Goal: " + GOAL["objective"]
    if name == "set_goal":
        return "Goal replaced."
    return f"(no executor for {name})"


def main_exec(args, objective=None):
    """Full execution mode: real write_file/run_shell, loop until the goal is
    marked complete, max turns reached, or the model stalls."""
    objective = objective or GOAL["objective"]
    goal = dict(GOAL, objective=objective)
    now = time.strftime("%Y-%m-%d %H:%M:%S %z")
    messages = [
        {"role": "system", "content": HARNESS_SYSTEM},
        {
            "role": "user",
            "content": f"[time_harness now={now}]\n\n{goal_section(goal)}\n\n"
                       f"请完成这个目标：{objective}",
        },
    ]
    workdir = pathlib.Path(tempfile.mkdtemp(prefix="goal-advance-"))
    plan_state = {}
    print(f"model: {MODEL}  execute: yes  workdir: {workdir}")
    turns = []
    goal_complete = False
    tools_before_complete = []
    prev_tools: list = []
    for i in range(1, args.max_turns + 1):
        text, calls, _ = chat(messages)
        if not calls and not text.strip():
            turns.append({"turn": i, "note": "STALL"})
            print(f"  turn {i}: STALL — stopping")
            break
        assistant_blocks = [{"type": "text", "text": text}]
        results = []
        turn_tools = []
        for c in calls:
            rid = f"tu_{i}_{c['name']}_{results.__len__()}"
            assistant_blocks.append({
                "type": "tool_use", "id": rid, "name": c["name"], "input": c["args"],
            })
            out = execute_tool(c["name"], c["args"], workdir, plan_state)
            results.append({"id": rid, "name": c["name"], "out": out})
            turn_tools.append(c["name"])
            if c["name"] == "update_goal" and c["args"].get("status") == "complete":
                goal_complete = True
                tools_before_complete = list(prev_tools)
        messages.append({"role": "assistant", "content": assistant_blocks})
        if results:
            messages.append({
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": r["id"], "content": r["out"]}
                    for r in results
                ],
            })
        turns.append({
            "turn": i,
            "tools": [r["name"] for r in results],
            "text_len": len(text),
        })
        print(f"  turn {i}: tools={[r['name'] for r in results] or '-'} "
              f"text_len={len(text)} complete={goal_complete}")
        prev_tools.extend(turn_tools)
        if goal_complete:
            break
        messages.append({
            "role": "user",
            "content": f"[time_harness now={time.strftime('%Y-%m-%d %H:%M:%S %z')}]\n\n"
                       f"（目标模式自动续作）继续推进当前目标：{objective}",
        })

    summary = {
        "model": MODEL,
        "goal": objective,
        "goal_complete": goal_complete,
        "turns_used": len(turns),
        "turns": turns,
        "plan_lifecycle": plan_state,
        "tools_before_complete": tools_before_complete,
        "final_text": text if not goal_complete else None,
    }
    with open("benchmarks/goal_advance_exec_result.json", "w", encoding="utf-8") as f:
        json.dump(summary, f, ensure_ascii=False, indent=2)
    print(f"\ngoal_complete={goal_complete} turns={len(turns)}")
    print("结果已写入 benchmarks/goal_advance_exec_result.json")
    return summary


BATCH_GOALS = [
    "Write a Python script fib.py that prints the first 10 Fibonacci numbers, "
    "then run it and verify the output.",
    "Write a Python module stats.py with mean, median, std functions and a "
    "pytest test file, then run pytest and report results.",
    "Write a shell script count.sh that counts the number of files in the "
    "current directory, run it, and verify the count matches `ls | wc -l`.",
]


def main_batch(args):
    """Stop-condition audit: run several real goals; check completion rate,
    turns used, and whether the model verified (executed something) before
    marking the goal complete."""
    agg = []
    for goal_text in BATCH_GOALS:
        print(f"\n=== goal: {goal_text[:52]}... ===")
        result = main_exec(args, objective=goal_text)
        agg.append({
            "goal": goal_text,
            "goal_complete": result["goal_complete"],
            "turns_used": result["turns_used"],
            "plan_lifecycle": list(result.get("plan_lifecycle", {}).values()),
            "tools_before_complete": result.get("tools_before_complete", []),
        })
    summary = {
        "n_goals": len(agg),
        "completed": sum(1 for a in agg if a["goal_complete"]),
        "avg_turns": round(sum(a["turns_used"] for a in agg) / len(agg), 1),
        "goals": agg,
    }
    with open("benchmarks/goal_advance_batch_result.json", "w", encoding="utf-8") as f:
        json.dump(summary, f, ensure_ascii=False, indent=2)
    print(f"\n=== 批量汇总: {summary['completed']}/{summary['n_goals']} 完成, "
          f"平均 {summary['avg_turns']} 轮 ===")
    print("结果已写入 benchmarks/goal_advance_batch_result.json")


if __name__ == "__main__":
    main()
