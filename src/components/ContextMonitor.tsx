import { useEffect, useState } from "react";
import type { ThinkingMode, TokenUsage } from "../types";
import { useI18n } from "../i18n";

interface ContextMonitorProps {
  contextUsage: number;
  sessionCost: number;
  sessionTokens: TokenUsage;
  thinkingMode: ThinkingMode;
  messagesCount: number;
}

const MODE_LABELS: Record<ThinkingMode, string> = {
  "non-think": "Fast",
  "think-high": "Think",
  "think-max": "Deep",
};

interface ContextBreakdown {
  system_prompt_tokens: number;
  structure_tokens: number;
  key_files_tokens: number;
  key_files: { path: string; tokens: number }[];
  conversation_tokens: number;
  tool_definitions_tokens: number;
  total_tokens: number;
  max_tokens: number;
}

function fmt(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 1_000) return `${(n / 1000).toFixed(1)}K`;
  return `${n}`;
}

export default function ContextMonitor({
  contextUsage,
  sessionCost,
  sessionTokens,
  thinkingMode,
  messagesCount,
}: ContextMonitorProps) {
  const { t } = useI18n();
  const [breakdown, setBreakdown] = useState<ContextBreakdown | null>(null);
  const pct = Math.min((contextUsage / 1_048_576) * 100, 100);
  const barColor = pct > 90 ? "var(--red)" : pct > 60 ? "var(--amber)" : "var(--accent)";
  const totalTokens = sessionTokens.input + sessionTokens.output;

  // Section-level snapshot of the 1M window; refreshes while the tab is open.
  useEffect(() => {
    let alive = true;
    const load = async () => {
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const b = await invoke<ContextBreakdown>("get_context_breakdown");
        if (alive) setBreakdown(b);
      } catch { /* agent not connected yet */ }
    };
    void load();
    const id = setInterval(load, 5000);
    return () => { alive = false; clearInterval(id); };
  }, []);

  return (
    <div className="context-monitor">
      <div className="monitor-title">{t("monitor.context")}</div>

      <div className="monitor-section">
        <span className="monitor-section-label">{t("monitor.windowUsage")}</span>
        <div className="context-bar-bg">
          <div
            className="context-bar-fill"
            style={{ width: `${pct}%`, backgroundColor: barColor }}
          />
        </div>
        <span className="context-label">
          {fmt(contextUsage)} / 1,048,576 tok ({pct.toFixed(1)}%)
        </span>
      </div>

      <div className="monitor-section">
        <span className="monitor-section-label">{t("monitor.reasoningMode")}</span>
        <span className="monitor-value">{MODE_LABELS[thinkingMode]}</span>
      </div>

      <div className="monitor-section">
        <span className="monitor-section-label">{t("monitor.session")}</span>
        <div className="monitor-stats">
          <div className="stat-row">
            <span className="stat-label">{t("monitor.messages")}</span>
            <span className="stat-value">{messagesCount}</span>
          </div>
          <div className="stat-row">
            <span className="stat-label">{t("monitor.input")}</span>
            <span className="stat-value">{fmt(sessionTokens.input)} tok</span>
          </div>
          <div className="stat-row">
            <span className="stat-label">{t("monitor.output")}</span>
            <span className="stat-value">{fmt(sessionTokens.output)} tok</span>
          </div>
          <div className="stat-row">
            <span className="stat-label">{t("monitor.total")}</span>
            <span className="stat-value">{fmt(totalTokens)} tok</span>
          </div>
          <div className="stat-row">
            <span className="stat-label">{t("monitor.cacheHit")}</span>
            <span className={`stat-value ${sessionTokens.cache_hit_rate > 0.5 ? "stat-good" : ""}`}>
              {(sessionTokens.cache_hit_rate * 100).toFixed(0)}%
            </span>
          </div>
        </div>
      </div>

      <div className="monitor-section">
        <span className="monitor-section-label">{t("monitor.estimatedCost")}</span>
        <span className="cost-value">${sessionCost.toFixed(4)}</span>
        <span className="cost-note">{t("monitor.costNote")}</span>
      </div>

      <div className="monitor-section">
        <span className="monitor-section-label">{t("monitor.nativeDials")}</span>
        <div className="monitor-stats">
          <div className="stat-row">
            <span className="stat-label">{t("monitor.cacheSavings")}</span>
            <span className="stat-good">
              ${(sessionTokens.cache_savings || 0).toFixed(4)} {t("monitor.thisSession")}
            </span>
          </div>
          <div className="stat-row">
            <span className="stat-label">{t("monitor.thinkingTokens")}</span>
            <span className="stat-good">{fmt(sessionTokens.thinking_tokens || 0)} tok</span>
          </div>
          <div className="stat-row">
            <span className="stat-label">{t("monitor.reasoningShare")}</span>
            <span className="stat-good">
              {sessionTokens.output > 0
                ? t("monitor.ofOutput", { pct: Math.min(100, ((sessionTokens.thinking_tokens || 0) / sessionTokens.output) * 100).toFixed(0) })
                : "—"}
            </span>
          </div>
        </div>
      </div>

      {breakdown && breakdown.total_tokens > 0 && (
        <div className="monitor-section">
          <span className="monitor-section-label">{t("monitor.composition")}</span>
          <div className="ctx-stack">
            <CtxSegment width={segPct(breakdown.system_prompt_tokens, breakdown.total_tokens)} color="var(--accent)" />
            <CtxSegment width={segPct(breakdown.structure_tokens, breakdown.total_tokens)} color="var(--coral)" />
            <CtxSegment width={segPct(breakdown.conversation_tokens, breakdown.total_tokens)} color="var(--green)" />
            <CtxSegment width={segPct(breakdown.tool_definitions_tokens, breakdown.total_tokens)} color="var(--amber)" />
          </div>
          <div className="ctx-rows">
            <CtxRow color="var(--accent)" label={t("monitor.systemPrompt")} value={fmt(breakdown.system_prompt_tokens)} />
            <CtxRow color="var(--coral)" label={t("monitor.structure")} value={fmt(breakdown.structure_tokens)} />
            <CtxRow color="var(--green)" label={t("monitor.conversation")} value={fmt(breakdown.conversation_tokens)} />
            <CtxRow color="var(--amber)" label={t("monitor.tools")} value={fmt(breakdown.tool_definitions_tokens)} />
          </div>
          <div className="ctx-keyfiles">
            <span>{t("monitor.keyFiles", { n: breakdown.key_files.length })}</span>
            <span>{fmt(breakdown.key_files_tokens)} tok · {t("monitor.onDemand")}</span>
          </div>
        </div>
      )}
    </div>
  );
}

function segPct(tokens: number, total: number): number {
  if (tokens <= 0) return 0;
  return Math.max((tokens / total) * 100, 1);
}

function CtxSegment({ width, color }: { width: number; color: string }) {
  return <div className="ctx-stack-seg" style={{ width: `${width}%`, background: color }} />;
}

function CtxRow({ color, label, value }: { color: string; label: string; value: string }) {
  return (
    <div className="ctx-row">
      <span className="ctx-row-dot" style={{ background: color }} />
      <span className="ctx-row-label">{label}</span>
      <span className="ctx-row-value">{value}</span>
    </div>
  );
}
