import type { TokenUsage } from "../types";
import { useI18n } from "../i18n";

interface ToolbarProps {
  contextUsage: number;
  sessionCost: number;
  sessionTokens: TokenUsage;
  showMonitor: boolean;
  showSidebar: boolean;
  showTools: boolean;
  onToggleMonitor: () => void;
  onToggleSidebar: () => void;
  onToggleTools: () => void;
  onOpenSettings: () => void;
  apiKeyConfigured: boolean;
}

function fmt(n: number): string {
  if (n > 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n > 1_000) return `${(n / 1000).toFixed(0)}K`;
  return `${n}`;
}

export default function Toolbar({
  contextUsage,
  sessionCost,
  sessionTokens,
  showMonitor,
  showSidebar,
  showTools,
  onToggleMonitor,
  onToggleSidebar,
  onToggleTools,
  onOpenSettings,
  apiKeyConfigured,
}: ToolbarProps) {
  const { t } = useI18n();
  const totalTokens = sessionTokens.input + sessionTokens.output;
  const contextPct = Math.min((contextUsage / 1_000_000) * 100, 100);

  return (
    <div className="toolbar">
      <div className="toolbar-left">
        <button
          className={`toolbar-btn ${showSidebar ? "active" : ""}`}
          onClick={onToggleSidebar}
          title={t("toolbar.toggleFiles")}
        >
          {t("toolbar.files")}
        </button>
        <span className="toolbar-brand">DeepSeek Code</span>
        <span className="toolbar-version">v0.1</span>
      </div>

      <div className="toolbar-spacer" />

      <div className="toolbar-right">
        <div className="toolbar-stats">
          <span className="toolbar-stat">
            {t("toolbar.ctx")} <span className="toolbar-stat-value">{contextPct.toFixed(0)}%</span>
          </span>
          <span className="toolbar-stat">
            <span className="toolbar-stat-value">{fmt(totalTokens)}</span> {t("toolbar.tok")}
          </span>
          <span className="toolbar-stat">
            <span className="toolbar-stat-value">${sessionCost.toFixed(3)}</span>
          </span>
        </div>
        <button
          className={`toolbar-btn ${showMonitor ? "active" : ""}`}
          onClick={onToggleMonitor}
          title={t("toolbar.togglePanel")}
        >
          {t("toolbar.panel")}
        </button>
        <button
          className={`toolbar-btn toolbar-icon-btn ${showTools ? "active" : ""}`}
          onClick={onToggleTools}
          title={t("toolbar.toggleTools")}
        >
          <svg
            width="15" height="15" viewBox="0 0 24 24" fill="none"
            stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"
            aria-hidden="true"
          >
            <polyline points="4 17 10 11 4 5" />
            <line x1="12" y1="19" x2="20" y2="19" />
          </svg>
        </button>
        <button className="toolbar-btn" onClick={onOpenSettings} title={t("toolbar.openSettings")}>
          {apiKeyConfigured ? t("toolbar.settings") : t("toolbar.connect")}
        </button>
      </div>
    </div>
  );
}
