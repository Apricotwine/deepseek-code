import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { StoredSettings } from "../store";
import { MODELS } from "../types";
import type { Language } from "../i18n";
import { useI18n } from "../i18n";

type SettingsTab = "general" | "connection" | "model" | "workspace" | "about";

interface SettingsModalProps {
  stored: StoredSettings;
  onClose: () => void;
  onConfigured: (apiKey: string, workspacePath: string, model: string, thinkBudget: number, deepBudget: number, language: Language, timeHarness: boolean, useHarness: boolean, sandbox: string) => void;
}

export default function SettingsModal({
  stored,
  onClose,
  onConfigured,
}: SettingsModalProps) {
  const { t, language, setLanguage } = useI18n();
  const [tab, setTab] = useState<SettingsTab>("connection");
  const [apiKey, setApiKey] = useState(stored.apiKey);
  const [workspacePath, setWorkspacePath] = useState(stored.workspacePath);
  const [model, setModel] = useState(stored.model || MODELS[0].id);
  const [thinkBudget, setThinkBudget] = useState(stored.thinkBudget);
  const [deepBudget, setDeepBudget] = useState(stored.deepBudget);
  const [showKey, setShowKey] = useState(false);
  const [timeHarness, setTimeHarness] = useState(stored.timeHarness);
  const [useHarness, setUseHarness] = useState(stored.useHarness);
  const [sandbox, setSandbox] = useState(stored.sandbox || "workspace-write");
  const [status, setStatus] = useState<{
    type: "success" | "error";
    msg: string;
  } | null>(null);
  const [initializing, setInitializing] = useState(false);

  const handleBrowse = async () => {
    const selected = await open({
      directory: true,
      multiple: false,
      title: "Select workspace folder",
    });
    if (selected && typeof selected === "string") {
      setWorkspacePath(selected);
    }
  };

  const handleSave = async () => {
    if (!apiKey.trim()) {
      setStatus({ type: "error", msg: t("settings.apiKeyRequired") });
      return;
    }
    setInitializing(true);
    setStatus(null);
    try {
      const trimmedKey = apiKey.trim();
      const trimmedWs = workspacePath.trim();
      const sameConnection = trimmedKey === stored.apiKey && trimmedWs === stored.workspacePath;
      let result = "";
      if (!sameConnection) {
        // Connection changed → (re)initialize the agent with the new config.
        result = await invoke<string>("init_agent", {
          apiKey: trimmedKey,
          workspacePath: trimmedWs || null,
          model,
        });
      } else if (model !== stored.model) {
        // Live switch — keeps the conversation, only the model changes.
        result = await invoke<string>("switch_model", { model });
      } else {
        // Nothing backend-relevant changed (e.g. language/budgets only) —
        // never call init_agent here: it would wipe the current session.
        result = "Settings saved.";
      }
      setStatus({ type: "success", msg: result });
      onConfigured(trimmedKey, trimmedWs, model, thinkBudget, deepBudget, language, timeHarness, useHarness, sandbox);
    } catch (err) {
      setStatus({ type: "error", msg: `${err}` });
    } finally {
      setInitializing(false);
    }
  };

  const tabs: { id: SettingsTab; label: string }[] = [
    { id: "general", label: t("settings.general") },
    { id: "connection", label: t("settings.connection") },
    { id: "model", label: t("settings.model") },
    { id: "workspace", label: t("settings.workspace") },
    { id: "about", label: t("settings.about") },
  ];

  return (
    <div className="settings-overlay" onClick={onClose}>
      <div className="settings-modal settings-modal-wide" onClick={(e) => e.stopPropagation()}>
        <div className="settings-header">
          <span className="settings-title">{t("settings.title")}</span>
        </div>

        <div className="settings-body">
          <div className="settings-nav">
            {tabs.map((tb) => (
              <button
                key={tb.id}
                className={`settings-nav-item ${tab === tb.id ? "active" : ""}`}
                onClick={() => setTab(tb.id)}
              >
                {tb.label}
              </button>
            ))}
          </div>

          <div className="settings-content">
            {tab === "general" && (
              <div className="settings-section">
                <div className="settings-section-title">{t("settings.general")}</div>
                <div className="settings-field">
                  <label>{t("settings.languageLabel")}</label>
                  <div className="language-segmented">
                    <button
                      className={`language-seg ${language === "zh" ? "active" : ""}`}
                      onClick={() => setLanguage("zh")}
                    >
                      中文
                    </button>
                    <button
                      className={`language-seg ${language === "en" ? "active" : ""}`}
                      onClick={() => setLanguage("en")}
                    >
                      English
                    </button>
                  </div>
                  <div className="settings-hint">{t("settings.languageHint")}</div>
                </div>
                <div className="settings-field">
                  <label>{t("settings.timeHarness")}</label>
                  <label className="settings-toggle-row">
                    <input
                      type="checkbox"
                      checked={timeHarness}
                      onChange={(e) => setTimeHarness(e.target.checked)}
                    />
                    <span>{t("settings.timeHarnessDesc")}</span>
                  </label>
                  <div className="settings-hint">{t("settings.timeHarnessHint")}</div>
                </div>
                <div className="settings-field">
                  <label>{t("settings.useHarness")}</label>
                  <label className="settings-toggle-row">
                    <input
                      type="checkbox"
                      checked={useHarness}
                      onChange={(e) => setUseHarness(e.target.checked)}
                    />
                    <span>{t("settings.useHarnessDesc")}</span>
                  </label>
                  <div className="settings-hint">{t("settings.useHarnessHint")}</div>
                </div>
                <div className="settings-field">
                  <label>{t("settings.sandbox")}</label>
                  <select
                    className="settings-select"
                    value={sandbox}
                    onChange={(e) => setSandbox(e.target.value)}
                  >
                    <option value="read-only">{t("settings.sandboxReadOnly")}</option>
                    <option value="workspace-write">{t("settings.sandboxWorkspace")}</option>
                    <option value="danger-full-access">{t("settings.sandboxFull")}</option>
                  </select>
                  <div className="settings-hint">{t("settings.sandboxHint")}</div>
                </div>
              </div>
            )}

            {tab === "connection" && (
              <div className="settings-section">
                <div className="settings-section-title">{t("settings.connection")}</div>
                <div className="settings-field">
                  <label>{t("settings.apiKeyLabel")}</label>
                  <div className="settings-path-row">
                    <input
                      type={showKey ? "text" : "password"}
                      value={apiKey}
                      onChange={(e) => setApiKey(e.target.value)}
                      placeholder={t("settings.apiKeyPlaceholder")}
                      autoFocus
                    />
                    <button
                      className="settings-btn"
                      onClick={() => setShowKey((v) => !v)}
                      title={showKey ? t("settings.hide") : t("settings.show")}
                    >
                      {showKey ? t("settings.hide") : t("settings.show")}
                    </button>
                  </div>
                  <div className="settings-hint">{t("settings.apiKeyHint")}</div>
                </div>
              </div>
            )}

            {tab === "model" && (
              <div className="settings-section">
                <div className="settings-section-title">{t("settings.model")}</div>
                <div className="settings-field">
                  <label>{t("settings.modelLabel")}</label>
                  <select
                    className="settings-select"
                    value={model}
                    onChange={(e) => setModel(e.target.value)}
                  >
                    {MODELS.map((m) => (
                      <option key={m.id} value={m.id}>
                        {m.label}{m.badge ? ` — ${m.badge}` : ""} · {m.tagline}
                      </option>
                    ))}
                  </select>
                  <div className="settings-hint">{t("settings.modelHint")}</div>
                </div>

                <div className="settings-field">
                  <label>{t("settings.thinkBudgetLabel")} / {t("settings.deepBudgetLabel")}</label>
                  <div className="settings-budget-row">
                    <div className="settings-budget-item">
                      <span className="settings-budget-label">Think</span>
                      <input
                        type="number"
                        min={1000}
                        max={64000}
                        step={1000}
                        value={thinkBudget}
                        onChange={(e) => setThinkBudget(Number(e.target.value) || 0)}
                      />
                    </div>
                    <div className="settings-budget-item">
                      <span className="settings-budget-label">Deep</span>
                      <input
                        type="number"
                        min={4000}
                        max={196608}
                        step={1000}
                        value={deepBudget}
                        onChange={(e) => setDeepBudget(Number(e.target.value) || 0)}
                      />
                    </div>
                  </div>
                  <div className="settings-hint">{t("settings.budgetHint")}</div>
                </div>
              </div>
            )}

            {tab === "workspace" && (
              <div className="settings-section">
                <div className="settings-section-title">{t("settings.workspace")}</div>
                <div className="settings-field">
                  <label>{t("settings.workspaceLabel")}</label>
                  <div className="settings-path-row">
                    <input
                      type="text"
                      value={workspacePath}
                      onChange={(e) => setWorkspacePath(e.target.value)}
                      placeholder={t("settings.workspacePlaceholder")}
                    />
                    <button className="settings-btn browse-btn" onClick={handleBrowse}>
                      {t("settings.browse")}
                    </button>
                  </div>
                  <div className="settings-hint">{t("settings.workspaceHint")}</div>
                </div>
              </div>
            )}

            {tab === "about" && (
              <div className="settings-section">
                <div className="settings-section-title">{t("settings.about")}</div>
                <div className="settings-about">
                  <div className="settings-about-brand">DeepSeek Code</div>
                  <div className="settings-about-meta">
                    {t("settings.aboutVersion")} 0.1.0
                  </div>
                  <p className="settings-about-desc">{t("settings.aboutDesc")}</p>
                  <a
                    className="settings-about-link"
                    href="https://api-docs.deepseek.com"
                    target="_blank"
                    rel="noreferrer"
                  >
                    {t("settings.aboutDocs")}
                  </a>
                </div>
              </div>
            )}
          </div>
        </div>

        {status && (
          <div className={`settings-status ${status.type}`}>{status.msg}</div>
        )}

        <div className="settings-footer">
          <button className="settings-btn" onClick={onClose}>
            {t("settings.cancel")}
          </button>
          <button
            className="settings-btn primary"
            onClick={handleSave}
            disabled={initializing}
          >
            {initializing ? t("settings.connecting") : t("settings.saveConnect")}
          </button>
        </div>
      </div>
    </div>
  );
}
