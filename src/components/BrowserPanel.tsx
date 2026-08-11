import { useState } from "react";
import { useI18n } from "../i18n";

const QUICK_LINKS = [
  { label: "DeepSeek API Docs", url: "https://api-docs.deepseek.com" },
  { label: "GitHub", url: "https://github.com" },
  { label: "Wikipedia", url: "https://www.wikipedia.org" },
  { label: "Bing", url: "https://www.bing.com" },
];

function normalizeUrl(raw: string): string {
  const trimmed = raw.trim();
  if (!trimmed) return "";
  return /^https?:\/\//i.test(trimmed) ? trimmed : `https://${trimmed}`;
}

export default function BrowserPanel() {
  const { t } = useI18n();
  const [input, setInput] = useState("");
  const [current, setCurrent] = useState<string | null>(null);

  const go = (raw: string) => {
    const url = normalizeUrl(raw);
    if (url) setCurrent(url);
  };

  const openExternal = async () => {
    if (!current) return;
    try {
      const { open } = await import("@tauri-apps/plugin-shell");
      await open(current);
    } catch {
      window.open(current, "_blank");
    }
  };

  return (
    <div className="browser-panel">
      <div className="browser-toolbar">
        <input
          className="browser-url"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => { if (e.key === "Enter") go(input); }}
          placeholder={t("browser.urlPlaceholder")}
          spellCheck={false}
        />
        <button className="browser-go" onClick={() => go(input)}>Go</button>
        <button
          className="browser-external"
          title={t("browser.openExternal")}
          onClick={() => void openExternal()}
        >
          ↗
        </button>
      </div>
      {current ? (
        <>
          <iframe
            className="browser-frame"
            src={current}
            title="sidebar browser"
          />
          <div className="browser-note">{t("browser.iframeNote")}</div>
        </>
      ) : (
        <div className="browser-empty">
          <div className="browser-empty-title">{t("browser.quickLinks")}</div>
          {QUICK_LINKS.map((l) => (
            <button
              key={l.url}
              className="browser-quicklink"
              onClick={() => { setInput(l.url); go(l.url); }}
            >
              {l.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
