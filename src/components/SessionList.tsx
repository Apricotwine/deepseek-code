import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { SessionMeta, Message } from "../types";
import { useI18n } from "../i18n";

interface SessionListProps {
  onLoadSession: (messages: Message[], sessionId?: string) => void;
  onNewSession: () => void;
}

export default function SessionList({
  onLoadSession,
  onNewSession,
}: SessionListProps) {
  const { t } = useI18n();
  const [sessions, setSessions] = useState<SessionMeta[]>([]);
  const [loading, setLoading] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const list = await invoke<SessionMeta[]>("list_sessions");
      setSessions(list);
    } catch {
      // No sessions yet
    }
  }, []);

  // 每次切到 History 页都刷新（自动保存会在轮次结束写盘）
  useEffect(() => {
    refresh();
  }, [refresh]);

  const handleLoad = async (id: string) => {
    setLoading(true);
    try {
      const raw = await invoke<string>("load_session", { sessionId: id });
      const stored: {
        role: string;
        content: string;
        timestamp: number;
        thinking_content?: string;
      }[] = JSON.parse(raw);
      const messages: Message[] = stored.map((m) => ({
        id: crypto.randomUUID(),
        role: m.role as "user" | "assistant",
        content: m.content,
        timestamp: m.timestamp,
        thinkingContent: m.thinking_content,
      }));
      // 把会话注入 agent 上下文——继续聊时模型才记得之前的内容
      try { await invoke("restore_session", { sessionId: id }); } catch { /* context restore failed */ }
      onLoadSession(messages, id);
    } catch (err) {
      console.error("Load failed:", err);
    } finally {
      setLoading(false);
    }
  };

  const handleDelete = async (id: string) => {
    try {
      await invoke("delete_session", { sessionId: id });
      await refresh();
    } catch (err) {
      console.error("Delete failed:", err);
    }
  };

  return (
    <div className="session-list">
      <div className="session-header">
        <span className="session-title">{t("sessions.history")}</span>
        <div className="session-actions">
          <button className="session-btn" onClick={onNewSession} title={t("sessions.newSession")}>
            +
          </button>
        </div>
      </div>
      <div className="session-items">
        {sessions.map((s) => (
          <div key={s.id} className="session-item">
            <div className="session-item-main" onClick={() => handleLoad(s.id)}>
              <span className="session-item-title">{s.title}</span>
              <span className="session-item-meta">
                <span>{t("sessions.messages", { n: s.message_count })}</span>
                <span>{timeAgo(s.updated_at, t)}</span>
              </span>
            </div>
            <button
              className="session-delete-btn"
              onClick={(e) => {
                e.stopPropagation();
                handleDelete(s.id);
              }}
              title={t("sessions.delete")}
              aria-label={t("sessions.delete")}
            >
              x
            </button>
          </div>
        ))}
        {sessions.length === 0 && (
          <div className="session-empty">{t("sessions.empty")}</div>
        )}
        {loading && <div className="session-loading">{t("sessions.loading")}</div>}
      </div>
    </div>
  );
}

function timeAgo(iso: string, t: (key: string, params?: Record<string, string | number>) => string): string {
  const then = new Date(iso).getTime();
  const now = Date.now();
  const mins = Math.floor((now - then) / 60000);
  if (mins < 1) return t("sessions.justNow");
  if (mins < 60) return t("sessions.minAgo", { n: mins });
  const hours = Math.floor(mins / 60);
  if (hours < 24) return t("sessions.hourAgo", { n: hours });
  const days = Math.floor(hours / 24);
  if (days < 7) return t("sessions.dayAgo", { n: days });
  return new Date(iso).toLocaleDateString();
}
