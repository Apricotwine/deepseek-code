import { useState, useRef, useEffect, useCallback, memo } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import StreamingText from "./StreamingText";
import type { Message, ThinkingMode, HarnessMode, AgentStreamEvent, LiveActivity, SlashCommand, TokenUsage } from "../types";
import { HARNESS_MODES } from "../types";
import { useI18n } from "../i18n";

const MIN_RUNNING_MS = 600; // 工具完成得再快，也至少展示 600ms 的运行渐变

/** Compact conversation context for the Harness kernel: the Harness runtime is
 *  one-shot per process (a fresh session per turn), so the IDE carries the
 *  conversation and injects the recent history into each prompt. */
function buildHarnessContext(messages: Message[], maxTurns = 16, maxChars = 12000): string {
  const recent = messages
    .filter((m) => m.role === "user" || m.role === "assistant")
    .slice(-maxTurns * 2);
  let out = "";
  for (const m of recent) {
    const label = m.role === "user" ? "用户" : "小鲸鱼";
    out += `${label}: ${m.content}\n`;
    if (out.length > maxChars) break;
  }
  return out.trim();
}

const QUICK_ACTIONS = [
  { id: "search", label: "搜网页", template: "帮我搜索并总结：\n" },
  { id: "shell", label: "跑命令", template: "在终端执行以下命令并解释输出：\n" },
  { id: "read", label: "读文件", template: "读取并解释这个文件：\n" },
  { id: "subagent", label: "派子代理", template: "派一个子代理去独立调查并回来汇报：\n" },
  { id: "plan", label: "拆任务", template: "把这件事拆成可执行的计划步骤，然后开始执行：\n" },
] as const;

// A turn's live work renders as a supervision pipeline (Cursor/Codex style):
// episode cards ("任务": reasoning summary + intermediate text) alternating
// with action cards (tool calls). The final result message stays a normal
// bubble — process and result never mix.
type EpisodeStep = {
  kind: "episode";
  id: string;
  text: string;
  status: "active" | "done";
};
type ToolStep = { kind: "tool"; activity: LiveActivity };
type ProcessStep = EpisodeStep | ToolStep;

// Display pacing for the supervision pipeline: real-time (default) or slowed
// so the reasoning is actually readable as it streams in.
type RenderSpeed = "realtime" | "slow" | "read";

// Process text is rendered markdown WITHOUT emoji — emoji stay in the final
// result bubble only.
function stripEmoji(text: string): string {
  return text.replace(/[\p{Extended_Pictographic}\uFE0F\u200D]/gu, "");
}

function summarizeText(text: string): string {
  const cleaned = stripEmoji(text).replace(/[#*`>_~|]/g, "").replace(/\s+/g, " ").trim();
  const line = cleaned.split("\n")[0].trim();
  if (!line) return "";
  return line.length > 72 ? `${line.slice(0, 72)}…` : line;
}

interface ChatPanelProps {
  messages: Message[];
  isProcessing: boolean;
  thinkingMode: ThinkingMode;
  apiKeyConfigured: boolean;
  useHarness: boolean;
  harnessMode: HarnessMode;
  sandbox: string;
  model: string;
  onSwitchModel: () => void;
  switchDisabled?: boolean;
  onThinkingModeChange: (mode: ThinkingMode) => void;
  onHarnessModeChange: (mode: HarnessMode) => void;
  /** Increments when the user sets a goal — triggers the first auto turn. */
  goalKickoff: number;
  autoTurn: { index: number; max: number } | null;
  /** Promote the current composer text to the active goal and start working. */
  onSetAsGoal: (objective: string) => void;
  /** Objective of the active goal — used so the kickoff names the task. */
  goalObjective: string | null;
  onSend: (msg: Message) => void;
  onSetProcessing: (val: boolean) => void;
  onContextUpdate: (val: number) => void;
  onUsageUpdate?: (tokens: TokenUsage, cost: number) => void;
  slashCommands: SlashCommand[];
}

export default function ChatPanel({
  messages, isProcessing, thinkingMode, apiKeyConfigured, useHarness, harnessMode, sandbox,
  model, onSwitchModel, switchDisabled, onThinkingModeChange, onHarnessModeChange,
  goalKickoff, autoTurn, onSetAsGoal, goalObjective,
  onSend, onSetProcessing, onContextUpdate, onUsageUpdate, slashCommands,
}: ChatPanelProps) {
  const { t } = useI18n();
  const [input, setInput] = useState("");
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const [streamingId, setStreamingId] = useState<string | null>(null);
  // The in-flight supervision pipeline for the current turn.
  const [pipeline, setPipeline] = useState<ProcessStep[]>([]);
  const pipelineRef = useRef<ProcessStep[]>([]);
  pipelineRef.current = pipeline;
  // Synchronous flag — set on every streamed chunk so the resolve path never
  // depends on render timing (refs are fine for flags, only accumulation
  // needs functional updates).
  const streamedAnyRef = useRef(false);
  // Render-speed dial: 1x = real-time, slower = paced for reading.
  const [renderSpeed, setRenderSpeed] = useState<RenderSpeed>(() => {
    const saved = localStorage.getItem("deepseek-code:render-speed");
    return saved === "slow" || saved === "read" ? saved : "realtime";
  });
  const renderSpeedRef = useRef(renderSpeed);
  renderSpeedRef.current = renderSpeed;
  // Pending display buffer used by the pacer (non-realtime only).
  const paceBufRef = useRef("");

  const appendText = useCallback((chunk: string) => {
    streamedAnyRef.current = true;
    setPipeline((prev) => {
      const last = prev[prev.length - 1];
      if (last && last.kind === "episode" && last.status === "active") {
        return [...prev.slice(0, -1), { ...last, text: last.text + chunk }];
      }
      return [...prev, { kind: "episode", id: crypto.randomUUID(), text: chunk, status: "active" }];
    });
  }, []);

  // Immediately render whatever the pacer still holds (tool boundary, turn
  // end, or switching back to real-time).
  const flushPaced = useCallback(() => {
    const buf = paceBufRef.current;
    if (buf) {
      appendText(buf);
      paceBufRef.current = "";
    }
  }, [appendText]);

  const changeSpeed = useCallback((speed: RenderSpeed) => {
    setRenderSpeed(speed);
    localStorage.setItem("deepseek-code:render-speed", speed);
    if (speed === "realtime") flushPaced();
  }, [flushPaced]);
  // Once the turn result arrives, late in-flight deltas must not resurrect
  // process cards (the final message already carries the full content).
  const turnFinishedRef = useRef(false);
  // ── Slash command panel ("/" summon, like Claude Code) ──
  const [slashOpen, setSlashOpen] = useState(false);
  const [slashQuery, setSlashQuery] = useState("");
  const [slashIdx, setSlashIdx] = useState(0);
  const filtered = slashCommands.filter((c) => c.id.startsWith(slashQuery));
  const chatEndRef = useRef<HTMLDivElement>(null);
  const messagesRef = useRef<HTMLDivElement>(null);
  // Stick to the bottom while the turn streams; stop following once the user
  // scrolls up to read earlier content.
  const stickToBottom = useRef(true);
  const unlistenRef = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    // Instant follow, not smooth: smooth-scrolling on every delta is the main
    // source of jank during long streaming turns.
    if (stickToBottom.current) chatEndRef.current?.scrollIntoView();
  }, [pipeline, messages]);

  // Pacer: in non-realtime modes, drain the display buffer at a readable rate.
  useEffect(() => {
    if (renderSpeed === "realtime") return;
    const charsPerTick = renderSpeed === "read" ? 12 : 30; // per 100ms
    const id = setInterval(() => {
      const buf = paceBufRef.current;
      if (buf) {
        const piece = buf.slice(0, charsPerTick);
        paceBufRef.current = buf.slice(charsPerTick);
        appendText(piece);
      }
    }, 100);
    return () => clearInterval(id);
  }, [renderSpeed, appendText]);

  const handleScroll = () => {
    const el = messagesRef.current;
    if (!el) return;
    stickToBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
  };
  useEffect(() => { return () => { unlistenRef.current?.(); }; }, []);

  const sendMessage = async (prefillText?: string) => {
    const text = (prefillText ?? input).trim();
    if (!text || isProcessing) return;
    const userMsg: Message = { id: crypto.randomUUID(), role: "user", content: text, timestamp: Date.now() };
    // Only clear the composer when the message actually came from it — a
    // goal-mode kickoff must never wipe a draft the user is typing.
    if (prefillText === undefined) setInput("");
    onSend(userMsg); onSetProcessing(true);
    setPipeline([]); pipelineRef.current = [];
    streamedAnyRef.current = false;
    turnFinishedRef.current = false;
    if (unlistenRef.current) { unlistenRef.current(); unlistenRef.current = null; }

    const unlisten = await listen<AgentStreamEvent>("agent-stream", (event) => {
      const p = event.payload as Record<string, unknown>;
      const eventType = (p.type as string) || "";
      if (eventType === "tool_done" || eventType === "tool_error") {
        const toolId = p.id as string;
        // 最小运行展示时长：毫秒级工具也至少停留 600ms 的"工作中"渐变，
        // 用户才看得到扫描动画，而不是瞬间跳绿勾
        const act = pipelineRef.current
          .filter((s): s is ToolStep => s.kind === "tool")
          .find((s) => s.activity.toolId === toolId);
        const age = act ? Date.now() - act.activity.timestamp : 0;
        const wait = Math.max(0, MIN_RUNNING_MS - age);
        const summary = (p.summary as string) || (p.error as string) || "";
        const status = eventType === "tool_done" ? "done" : "error";
        const apply = () =>
          setPipeline((prev) => prev.map((s) =>
            s.kind === "tool" && s.activity.toolId === toolId
              ? { ...s, activity: { ...s.activity, summary, status } }
              : s
          ));
        if (wait > 0) setTimeout(apply, wait); else apply();
        return;
      }
      if (eventType === "turn_end") { onContextUpdate((p.context_usage as number) || 0); return; }
      if (eventType === "diff_created" || eventType === "task_list") return; // handled by App
      if (eventType === "turn_start") return;

      // Reasoning deltas: the model still thinks (backend tracks the trace
      // for signature replay + overthinking detection), but the UI doesn't
      // surface a "思考过程" box — the pipeline only shows actions + text.
      if (eventType === "thinking") {
        if (turnFinishedRef.current) return;
        streamedAnyRef.current = true;
        return;
      }

      // Text deltas → append to the active episode (rendered as markdown).
      if (eventType === "text") {
        if (turnFinishedRef.current) return;
        const chunk = (p.content as string) || "";
        if (!chunk) return;
        streamedAnyRef.current = true;
        if (renderSpeedRef.current === "realtime") {
          appendText(chunk);
        } else {
          paceBufRef.current += chunk;
        }
        return;
      }

      const activity: LiveActivity = { id: crypto.randomUUID(), type: eventType as LiveActivity["type"], timestamp: Date.now(), status: "running" };
      if (eventType !== "tool_start") return;
      activity.toolName = p.name as string; activity.toolId = p.id as string; activity.args = p.args as string;
      // 工具卡落位前必须冲掉待渲染文字，否则剩余文字会溢出到工具卡之后
      if (renderSpeedRef.current !== "realtime") flushPaced();
      // A tool call begins → close the active episode, then push the action card.
      setPipeline((prev) => [
        ...prev.map((s) =>
          s.kind === "episode" && s.status === "active" ? { ...s, status: "done" as const } : s
        ),
        { kind: "tool", activity },
      ]);
    });
    unlistenRef.current = unlisten;

    try {
      const result = useHarness
        ? await invoke<{ message: string; thinking_content?: string; token_usage: { input: number; output: number; cached: number; cache_hit_rate: number; cache_savings?: number; thinking_tokens?: number }; total_cost?: number; finish_reason: string; context_usage: number }>("send_harness_message", {
            message: text,
            thinkingMode: thinkingMode,
            sessionId: `main-${Date.now()}`,
            mode: harnessMode,
            sandbox,
            context: buildHarnessContext(messages, 16, 12000),
          })
        : await invoke<{ message: string; thinking_content?: string; token_usage: { input: number; output: number; cached: number; cache_hit_rate: number; cache_savings?: number; thinking_tokens?: number }; total_cost?: number; finish_reason: string; context_usage: number }>("send_message", { message: text, thinkingMode: thinkingMode });
      const assistantId = crypto.randomUUID();
      const streamedLive = streamedAnyRef.current;
      turnFinishedRef.current = true;
      // 轮次结束：先把缓冲里的过程文字全部渲染出来，再折叠最后一步
      flushPaced();
      onSend({ id: assistantId, role: "assistant", content: result.message, thinkingContent: result.thinking_content, tokenUsage: result.token_usage, timestamp: Date.now() });
      // 过程保留给督查：折叠最后一步的正文（完整回复在结果气泡里），
      // 其余步骤收成摘要行。下一轮发送时清空重来。
      setPipeline((prev) => {
        const next = prev.map((s) =>
          s.kind === "episode" && s.status === "active" ? { ...s, status: "done" as const } : s
        );
        const last = next[next.length - 1];
        if (last && last.kind === "episode") {
          next[next.length - 1] = { ...last, text: "" };
        }
        return next;
      });
      // The text already appeared live — render the final bubble instantly.
      // Only the batch fallback needs the typewriter animation.
      if (!streamedLive) setStreamingId(assistantId);
      onContextUpdate(result.context_usage);
      onUsageUpdate?.(result.token_usage, result.total_cost ?? 0);
    } catch (err) {
      const msg = String(err);
      if (/cancel/i.test(msg)) {
        // ESC 主动取消不是错误——中性提示即可
        onSend({ id: crypto.randomUUID(), role: "system", content: t("app.cancelled"), timestamp: Date.now() });
      } else {
        onSend({ id: crypto.randomUUID(), role: "system", content: `${t("app.error")}${msg}`, timestamp: Date.now() });
      }
    } finally {
      onSetProcessing(false);
      flushPaced();
      // 取消/报错时也把进行中的任务卡收起来，流水线保留供查看。
      setPipeline((prev) => prev.map((s) =>
        s.kind === "episode" && s.status === "active" ? { ...s, status: "done" as const } : s
      ));
      turnFinishedRef.current = true;
      setTimeout(() => { unlistenRef.current?.(); unlistenRef.current = null; }, 2000);
    }
  };

  // Goal-mode kickoff: setting a goal starts the first auto-advance turn
  // without requiring the user to send a message first.
  useEffect(() => {
    if (goalKickoff <= 0 || isProcessing) return;
    const timer = window.setTimeout(
      () => {
        // Name the objective explicitly so the model switches to the new
        // task instead of drifting back to previous work.
        const text = goalObjective
          ? `${t("goal.kickoff")}：${goalObjective}`
          : t("goal.kickoff");
        sendMessage(text);
      },
      400
    );
    return () => window.clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [goalKickoff, isProcessing, goalObjective]);

  // Batch fallback only — the typewriter finished; nothing else to tear down.
  const handleStreamComplete = () => setStreamingId(null);

  const handleCancel = async () => {
    try { await invoke("cancel_agent"); } catch { /* ignore */ }
  };

  // ESC key to cancel
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && isProcessing) { e.preventDefault(); handleCancel(); }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [isProcessing]);

  const handleInputChange = (e: React.ChangeEvent<HTMLTextAreaElement>) => {
    const value = e.target.value;
    setInput(value);
    // "/" at line start or after whitespace summons the command panel
    const m = value.match(/(?:^|\s)\/([\w-]*)$/);
    if (m) { setSlashQuery(m[1]); setSlashOpen(true); setSlashIdx(0); }
    else setSlashOpen(false);
  };

  const runSlash = (cmd: SlashCommand) => {
    void cmd.run();
    setInput("");
    setSlashOpen(false);
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (slashOpen) {
      if (e.key === "ArrowDown") { e.preventDefault(); setSlashIdx((i) => (i + 1) % Math.max(filtered.length, 1)); return; }
      if (e.key === "ArrowUp") { e.preventDefault(); setSlashIdx((i) => (i - 1 + Math.max(filtered.length, 1)) % Math.max(filtered.length, 1)); return; }
      if (e.key === "Enter") {
        if (filtered.length > 0) { e.preventDefault(); runSlash(filtered[slashIdx]); }
        else setSlashOpen(false); // no match — dismiss and keep typing
        return;
      }
      if (e.key === "Escape") { e.stopPropagation(); setSlashOpen(false); return; }
    }
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); sendMessage(); }
  };

  return (
    <div className="chat-panel">
      <div className="chat-messages" ref={messagesRef} onScroll={handleScroll}>
        {messages.length === 0 && !isProcessing && (
          <div className="chat-welcome">
            <h1>DeepSeek Code</h1>
            <p>
              {t("app.welcomeSubtitle")}
              {!apiKeyConfigured && t("app.openSettingsHint")}
            </p>
            <div className="welcome-features">
              <span>{t("app.welcomeFeature1")}</span>
              <span>{t("app.welcomeFeature2")}</span>
              <span>{t("app.welcomeFeature3")}</span>
              <span>{t("app.welcomeFeature4")}</span>
            </div>
          </div>
        )}
        {/* 阅读顺序：问题 → 过程流水线 → 结果。流水线保留到下一轮，供督查 */}
        {(() => {
          const hasLive = pipeline.length > 0;
          const last = messages.length > 0 ? messages[messages.length - 1] : null;
          const activeCount = pipeline.filter((s) =>
            (s.kind === "episode" && s.status === "active") ||
            (s.kind === "tool" && s.activity.status === "running")
          ).length;
          // 轮次进行中（turnFinished=false）时，最新消息是上一轮的回复，
          // 保持时间顺序不动；轮次结束后，把本轮的最终结果移到流水线下方。
          const trailingAssistant = hasLive && last && last.role === "assistant" && turnFinishedRef.current ? last : null;
          const ordered = trailingAssistant ? messages.slice(0, -1) : messages;

          return (
            <>
              {ordered.map((msg) => (
                <MessageBubble key={msg.id} message={msg} isStreaming={msg.id === streamingId} onStreamComplete={handleStreamComplete} />
              ))}
              {hasLive && (
                <div className="process-pipeline">
                  <div className="process-pipeline-header">
                    <span>{t("chat.pipeline", { n: pipeline.length })}</span>
                    <span className="pipeline-live">
                      {activeCount > 0 && <span className="pipeline-live-dot" />}
                      <span>{t("chat.active", { n: activeCount })}</span>
                    </span>
                  </div>
                  {pipeline.map((step) =>
                    step.kind === "tool"
                      ? <LiveActivityRow key={step.activity.id} activity={step.activity} />
                      : <ProcessEpisode key={step.id} step={step} />
                  )}
                </div>
              )}
              {trailingAssistant && (
                <MessageBubble key={trailingAssistant.id} message={trailingAssistant} isStreaming={trailingAssistant.id === streamingId} onStreamComplete={handleStreamComplete} />
              )}
            </>
          );
        })()}
        {isProcessing && pipeline.length === 0 && (
          <div className="chat-typing"><span className="typing-dot" /><span className="typing-dot" /><span className="typing-dot" /></div>
        )}
        <div ref={chatEndRef} />
      </div>
      <div className="chat-input-area">
        <div className="composer-controls">
          <button
            className={`composer-model ${model.includes("flash") ? "flash" : "pro"}`}
            onClick={onSwitchModel}
            disabled={!apiKeyConfigured || switchDisabled}
            title={model.includes("flash") ? "切换到 V4 Pro" : "切换到 V4 Flash"}
          >
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <rect x="3" y="3" width="7" height="7" rx="1" />
              <rect x="14" y="3" width="7" height="7" rx="1" />
              <rect x="3" y="14" width="7" height="7" rx="1" />
              <rect x="14" y="14" width="7" height="7" rx="1" />
            </svg>
            {model.includes("flash") ? "Flash" : "Pro"}
          </button>

          <div className="composer-seg" role="group">
            <button className={thinkingMode === "non-think" ? "active" : ""} onClick={() => onThinkingModeChange("non-think")} title="Fast">
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinejoin="round">
                <path d="M13 2 3 14h7l-1 8 10-12h-7z" />
              </svg>
            </button>
            <button className={thinkingMode === "think-high" ? "active" : ""} onClick={() => onThinkingModeChange("think-high")} title="Think">
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                <path d="M12 3c.8 2.5 1.7 3.4 4.5 4.5-2.8 1.1-3.7 2-4.5 4.5C11.2 9.5 10.3 8.6 7.5 7.5 10.3 6.4 11.2 5.5 12 3z" />
              </svg>
            </button>
            <button className={thinkingMode === "think-max" ? "active" : ""} onClick={() => onThinkingModeChange("think-max")} title="Deep">
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinejoin="round">
                <path d="M12 2 2 7l10 5 10-5z" />
                <path d="M2 12l10 5 10-5" />
                <path d="M2 17l10 5 10-5" />
              </svg>
            </button>
          </div>

          <div className="composer-seg composer-seg-modes" role="group">
            {HARNESS_MODES.map((m) => (
              <button
                key={m.id}
                className={harnessMode === m.id ? "active" : ""}
                onClick={() => onHarnessModeChange(m.id)}
                title={m.id}
              >
                {m.label}
              </button>
            ))}
          </div>

          <div className="pipeline-speed" title={t("chat.speed")}>
            <button
              className={renderSpeed === "realtime" ? "active" : ""}
              onClick={() => changeSpeed("realtime")}
              title={t("chat.speedRealtime")}
            >
              1x
            </button>
            <button
              className={renderSpeed === "slow" ? "active" : ""}
              onClick={() => changeSpeed("slow")}
              title={t("chat.speedSlow")}
            >
              0.5x
            </button>
            <button
              className={renderSpeed === "read" ? "active" : ""}
              onClick={() => changeSpeed("read")}
              title={t("chat.speedRead")}
            >
              0.25x
            </button>
          </div>
        </div>
        {slashOpen && filtered.length > 0 && (
          <div className="slash-panel">
            {filtered.map((cmd, i) => (
              <button
                key={cmd.id}
                className={`slash-item ${i === slashIdx ? "active" : ""}`}
                onMouseEnter={() => setSlashIdx(i)}
                onClick={() => runSlash(cmd)}
              >
                <span className="slash-cmd">/{cmd.id}</span>
                <span className="slash-desc">{cmd.description}</span>
              </button>
            ))}
            <div className="slash-hint">{t("chat.slashHint")}</div>
          </div>
        )}
        {autoTurn && (
          <div className="auto-chip">
            <span className="auto-chip-dot" />
            {t("goal.autoAdvancing", { index: autoTurn.index, max: autoTurn.max })}
          </div>
        )}
        <div className="quick-actions">
          {QUICK_ACTIONS.map((a) => (
            <button
              key={a.id}
              className="quick-action"
              disabled={isProcessing || !apiKeyConfigured}
              onClick={() => {
                setInput(a.template);
                inputRef.current?.focus();
              }}
            >
              {a.label}
            </button>
          ))}
        </div>
        <textarea ref={inputRef} className="chat-input" value={input} onChange={handleInputChange} onKeyDown={handleKeyDown}
          placeholder={apiKeyConfigured ? t("chat.placeholder") : t("chat.placeholderNoKey")}
          rows={3} disabled={isProcessing || !apiKeyConfigured} />
        <div className="chat-send-row">
          {isProcessing && (
            <button className="chat-cancel-btn" onClick={handleCancel}>{t("chat.cancel")}</button>
          )}
          <button
            className="chat-goal-btn"
            title={t("goal.setAsGoalDesc")}
            disabled={isProcessing || !input.trim() || !apiKeyConfigured}
            onClick={() => {
              const objective = input.trim();
              if (!objective) return;
              setInput("");
              onSetAsGoal(objective);
            }}
          >
            {t("goal.setAsGoal")}
          </button>
          <button className="chat-send-btn" onClick={() => sendMessage()} disabled={isProcessing || !input.trim() || !apiKeyConfigured}>
            {isProcessing ? t("chat.processing") : t("chat.send")}
          </button>
        </div>
      </div>
    </div>
  );
}

function MessageBubble({ message, isStreaming, onStreamComplete }: { message: Message; isStreaming?: boolean; onStreamComplete: () => void }) {
  const { t } = useI18n();
  return (
    <div className={`message-bubble message-${message.role}`}>
      <div className="message-header">
        <span className="message-role">
          {message.role === "user" ? t("chat.you") : message.role === "assistant" ? t("chat.whale") : t("chat.system")}
        </span>
        <span className="message-time">{new Date(message.timestamp).toLocaleTimeString("en-US", { hour: "2-digit", minute: "2-digit" })}</span>
        {message.tokenUsage && <span className="message-tokens">{(message.tokenUsage.input + message.tokenUsage.output).toLocaleString()} {t("chat.tok")}</span>}
      </div>
      <div className="message-content">
        {isStreaming ? (
          <StreamingText text={message.content} speed={5} onComplete={onStreamComplete} />
        ) : (
          <ReactMarkdown
            remarkPlugins={[remarkGfm]}
            components={{
              em: () => null,
            }}
          >
            {message.content}
          </ReactMarkdown>
        )}
      </div>
      {message.thinkingContent && (
        <details className="thinking-block">
          <summary>{t("chat.thinking")}</summary>
          <div className="thinking-content">{message.thinkingContent}</div>
        </details>
      )}
      {message.toolCalls && message.toolCalls.length > 0 && (
        <div className="tool-calls-list">{message.toolCalls.map((tc) => (<div key={tc.id} className={`tool-call tool-call-${tc.status}`}>{tc.name}</div>))}</div>
      )}
    </div>
  );
}

const LiveActivityRow = memo(function LiveActivityRow({ activity }: { activity: LiveActivity }) {
  const { t } = useI18n();
  const name = activity.toolName || "";
  const label = t(`chat.tool.${name}`);
  const running = activity.status === "running";
  const argsPreview = activity.args ? highlightPath(parseToolArgs(name, activity.args)) : "";
  // Header line: what it's doing while running, the result once settled.
  const headText = running ? argsPreview : activity.summary || argsPreview;
  return (
    <details className={`process-card process-tool-card tool-${activity.status} tool-type-${name}`} open={running}>
      <summary className="process-tool-head">
        <span className={`process-status-dot ${running ? "pulse" : activity.status === "done" ? "done" : "error"}`} />
        <span className="live-tool-icon">
          <ToolIcon name={name} />
        </span>
        <span className="process-tag">{label.startsWith("chat.tool.") ? name : label}</span>
        <span className="process-summary">{headText}</span>
        <span className="live-tool-kind">{TOOL_KINDS[name] || "TOOL"}</span>
        <span className="live-tool-status">
          {running && <span className="status-dots"><i /><i /><i /></span>}
          {activity.status === "done" && (
            <svg className="status-check" viewBox="0 0 16 16" width="13" height="13" fill="none"
              stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
              <path d="M3.2 8.6l3.1 3 6.5-7" />
            </svg>
          )}
          {activity.status === "error" && (
            <svg className="status-x" viewBox="0 0 16 16" width="13" height="13" fill="none"
              stroke="currentColor" strokeWidth="1.6" strokeLinecap="round">
              <path d="M4.5 4.5l7 7M11.5 4.5l-7 7" />
            </svg>
          )}
        </span>
      </summary>
      <div className="process-tool-body">
        {activity.args && <div className="act-args">{highlightPath(parseToolArgs(name, activity.args))}</div>}
        {activity.summary && <div className="act-summary">{activity.summary}</div>}
        {running && <div className="tool-progress"><span /></div>}
      </div>
    </details>
  );
});

// One "任务" card in the pipeline: a summarized reasoning line (collapsible
// to the full trace) + markdown-rendered intermediate text, no emoji. Active
// cards stream open; done cards collapse to their summary for supervision.
const ProcessEpisode = memo(function ProcessEpisode({ step }: { step: EpisodeStep }) {
  const { t } = useI18n();
  const active = step.status === "active";
  const summary = summarizeText(step.text);
  // 任务卡只呈现模型实际说/做的内容（思考过程不上屏）。
  // 进行中展开显示流式文字，落定后收起成摘要行。
  const openCard = active;
  return (
    <details className={`process-card process-episode ${active ? "active" : "done"}`} open={openCard}>
      <summary className="process-episode-head">
        <span className={`process-status-dot ${active ? "pulse" : "done"}`} />
        <span className="process-tag">{t("chat.step")}</span>
        <span className="process-summary">{summary || (active ? "…" : "")}</span>
      </summary>
      <div className="process-episode-body">
        {step.text && (
          <div className={`process-text-body ${active ? "streaming" : ""}`}>
            <ReactMarkdown remarkPlugins={[remarkGfm]}>{stripEmoji(step.text)}</ReactMarkdown>
            {active && <span className="streaming-cursor" />}
          </div>
        )}
        {active && !step.text && (
          <div className="process-working">
            <i /><i /><i />
            <span>{t("chat.working")}</span>
          </div>
        )}
      </div>
    </details>
  );
});

// Device-type tags — telemetry instrument codes
const TOOL_KINDS: Record<string, string> = {
  read_file: "READ", write_file: "WRITE", edit_file: "EDIT", run_shell: "SHELL",
  search_code: "GREP", list_directory: "LIST", read_memory: "MEM", write_memory: "MEM",
  web_search: "WEB", web_fetch: "FETCH", todo_write: "TODO", compact_context: "CTX",
};

// Stroke-style inline SVG icons (16 viewBox, currentColor) — one glyph per tool type.
function ToolIcon({ name }: { name: string }) {
  const p = {
    width: 13, height: 13, viewBox: "0 0 16 16", fill: "none" as const,
    stroke: "currentColor", strokeWidth: 1.4, strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
  };
  switch (name) {
    case "read_file":
      return <svg {...p}><path d="M8 4.6C7 3.7 5.6 3.3 3.5 3.3v9.4c2.1 0 3.5.4 4.5 1.3 1-.9 2.4-1.3 4.5-1.3V3.3c-2.1 0-3.5.4-4.5 1.3z" /><path d="M8 4.6v9.4" /></svg>;
    case "write_file":
      return <svg {...p}><path d="M3 13l.8-3.2 7.2-7.2a1.2 1.2 0 0 1 1.7 1.7l-7.2 7.2z" /><path d="M9.6 4.2l2.2 2.2" /></svg>;
    case "edit_file":
      return <svg {...p}><path d="M4 12l.5-2.5 6.6-6.6a1.4 1.4 0 0 1 2 2L6.5 11.4z" /></svg>;
    case "run_shell":
      return <svg {...p}><rect x="2.5" y="3.5" width="11" height="9" rx="1.5" /><path d="M5.5 6.6l2 1.9-2 1.9M9.5 10.4h2.5" /></svg>;
    case "search_code":
      return <svg {...p}><circle cx="7" cy="7" r="4" /><path d="M10.2 10.2L13.5 13.5" /></svg>;
    case "list_directory":
      return <svg {...p}><path d="M2.5 4.5h3.6l1.4 1.6h6v6.4a1.5 1.5 0 0 1-1.5 1.5H4a1.5 1.5 0 0 1-1.5-1.5z" /></svg>;
    case "read_memory":
    case "write_memory":
      return <svg {...p}><rect x="3.5" y="3.5" width="9" height="9" rx="1.5" /><path d="M8 3.5v9M3.5 8h9" /></svg>;
    case "web_search":
      // sonar arcs — the ping itself
      return <svg {...p}><circle cx="8" cy="8" r="1.5" /><path d="M8 4.8A3.2 3.2 0 0 1 11.2 8M8 2.3A5.7 5.7 0 0 1 13.7 8" /></svg>;
    case "web_fetch":
      return <svg {...p}><rect x="3" y="2.5" width="10" height="11" rx="1.5" /><path d="M8 5.5v5M5.8 8.2L8 10.4l2.2-2.2" /></svg>;
    case "todo_write":
      return <svg {...p}><rect x="3.5" y="3.5" width="9" height="9" rx="2" /><path d="M5.8 8.2l1.6 1.6 3-3.4" /></svg>;
    case "compact_context":
      return <svg {...p}><path d="M8 2.5v6M5.8 4.5L8 2.5l2.2 2M8 13.5v-6M5.8 11.5L8 13.5l2.2-2" /></svg>;
    default:
      return <svg {...p}><circle cx="8" cy="8" r="4.5" /></svg>;
  }
}

function parseToolArgs(name: string, args: string): string {
  try {
    const parsed = JSON.parse(args);
    switch (name) {
      case "read_file": case "write_file": case "edit_file": return parsed.path || "";
      case "run_shell": return parsed.command || "";
      case "search_code": return parsed.pattern || "";
      case "list_directory": return parsed.path || "./";
      default: return Object.values(parsed).join(" ");
    }
  } catch { return args.length > 60 ? args.slice(0, 60) + "..." : args; }
}

// Wrap path-like tokens in a bioluminescent highlight span.
function highlightPath(text: string): React.ReactNode {
  const parts = text.split(
    /((?:\/[\w\-./@:]+|\.[\w\-./@]+|\w+\.(?:rs|ts|tsx|js|jsx|py|toml|json|md|css|html|yaml|yml|sh|txt|log)))/g
  );
  return parts.map((part, i) =>
    /\/|\.(?:rs|ts|tsx|js|jsx|py|toml|json|md|css|html|yaml|yml|sh|txt|log)\b/.test(part)
      ? <span key={i} className="act-path">{part}</span>
      : <span key={i}>{part}</span>
  );
}
