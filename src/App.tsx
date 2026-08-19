import { useState, useCallback, useEffect, useRef, useMemo } from "react";
import ChatPanel from "./components/ChatPanel";
import EditorPanel from "./components/EditorPanel";
import ContextMonitor from "./components/ContextMonitor";
import Toolbar from "./components/Toolbar";
import Sidebar from "./components/Sidebar";
import SessionList from "./components/SessionList";
import SettingsModal from "./components/SettingsModal";
import DiffPanel, { type DiffEntry } from "./components/DiffPanel";
import TaskPanel, { type GoalState, type TaskItem } from "./components/TaskPanel";
import TerminalPanel from "./components/TerminalPanel";
import BrowserPanel from "./components/BrowserPanel";
import MarkdownPreview from "./components/MarkdownPreview";
import AgentsPanel from "./components/AgentsPanel";
import ToolCatalog from "./components/ToolCatalog";
import TrajectoryPanel from "./components/TrajectoryPanel";
import MemoryPanel from "./components/MemoryPanel";
import { Icon } from "./components/icons";
import type { Message, AppState, ThinkingMode, HarnessMode, SlashCommand, TokenUsage, Subagent, TrajectoryEntry } from "./types";
import { loadSettings, saveSettings, type StoredSettings } from "./store";
import { useI18n } from "./i18n";
import type { Language } from "./i18n";

function createInitialState(): AppState {
  return {
    messages: [],
    currentFile: null,
    projectPath: null,
    contextUsage: 0,
    thinkingMode: "think-high",
    isProcessing: false,
    sessionCost: 0,
    sessionTokens: { input: 0, output: 0, cached: 0, cache_hit_rate: 0 },
  };
}

export default function App() {
  const { t, setLanguage } = useI18n();
  const [state, setState] = useState<AppState>(createInitialState);
  const [showSidebar, setShowSidebar] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [apiKeyConfigured, setApiKeyConfigured] = useState(false);
  const [stored, setStored] = useState<StoredSettings>({ apiKey: "", workspacePath: "", model: "deepseek-v4-flash", thinkBudget: 16_000, deepBudget: 32_000, language: "zh", timeHarness: true, useHarness: false, sandbox: "workspace-write", goalMode: true });
  const [initialized, setInitialized] = useState(false);
  const [sidebarTab, setSidebarTab] = useState<"files" | "history" | "agents" | "memory">("files");
  const [rightPanelTab, setRightPanelTab] = useState<"monitor" | "diffs" | "tasks" | "tools" | "trajectory">("diffs");
  const [rightPanelVisible, setRightPanelVisible] = useState(true);
  // Separate tools sidebar (terminal + browser), toggled from the top-right
  // toolbar icon — Codex/Cursor style, independent of the review panel.
  const [toolsPanelTab, setToolsPanelTab] = useState<"terminal" | "browser">("terminal");
  const [toolsPanelVisible, setToolsPanelVisible] = useState(false);
  // Tools sidebar width: default generous (the browser needs real width);
  // drag-resizable via the handle on the panel's left edge.
  const [toolsWidth, setToolsWidth] = useState(600);

  const startToolsResize = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    const onMove = (ev: MouseEvent) => {
      const w = Math.min(820, Math.max(380, window.innerWidth - ev.clientX));
      setToolsWidth(w);
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }, []);
  const [mdPreview, setMdPreview] = useState(false);
  // Bumped whenever the active conversation changes — remounts ChatPanel so
  // its in-memory pipeline (supervision chain) never leaks across sessions.
  const [chatKey, setChatKey] = useState(0);

  // Diff state
  const [diffs, setDiffs] = useState<DiffEntry[]>([]);
  const [tasks, setTasks] = useState<TaskItem[]>([]);
  const [goal, setGoal] = useState<GoalState | null>(null);
  const [goalMode, setGoalMode] = useState(true);
  const [useHarness, setUseHarness] = useState(false);
  const [harnessMode, setHarnessMode] = useState<HarnessMode>("standard");
  const [agents, setAgents] = useState<Subagent[]>([]);
  const [sandbox, setSandbox] = useState("workspace-write");
  const [trajectory, setTrajectory] = useState<TrajectoryEntry[]>([]);
  const [maxAutoTurns, setMaxAutoTurns] = useState(10);
  const [autoTurn, setAutoTurn] = useState<{ index: number; max: number } | null>(null);
  const [autoTurnEnd, setAutoTurnEnd] = useState<string | null>(null);
  const [goalKickoff, setGoalKickoff] = useState(0);

  useEffect(() => {
    loadSettings().then((s) => {
      setStored(s);
      setLanguage(s.language || "zh");
      setGoalMode(s.goalMode ?? true);
      setUseHarness(s.useHarness ?? false);
      setSandbox(s.sandbox ?? "workspace-write");
      if (s.apiKey) {
        import("@tauri-apps/api/core").then(({ invoke }) => {
          invoke<string>("init_agent", { apiKey: s.apiKey, workspacePath: s.workspacePath || null, model: s.model || "deepseek-v4-flash" })
            .then(async () => {
              setApiKeyConfigured(true);
              try {
                await invoke("set_thinking_budgets", { thinkBudget: s.thinkBudget, deepBudget: s.deepBudget });
              } catch { /* defaults apply */ }
              try {
                await invoke("set_time_harness", { enabled: s.timeHarness !== false });
              } catch { /* defaults apply */ }
              try {
                const g = await invoke<GoalState | null>("get_goal_cmd");
                setGoal(g);
              } catch { /* no goal yet */ }
              // Sync the PERSISTED goal-mode preference into the backend —
              // reading backend state here would overwrite the user's saved
              // choice with the backend default on every restart.
              const persistedGoalMode = s.goalMode ?? true;
              try {
                await invoke("set_goal_mode_cmd", { enabled: persistedGoalMode });
                setGoalMode(persistedGoalMode);
                const m = await invoke<{ goal_max_auto_turns: number }>("get_goal_mode_cmd");
                setMaxAutoTurns(m.goal_max_auto_turns);
              } catch { /* defaults */ }
              // Auto-restore the most recent session for this workspace so an
              // app update/restart never looks like history loss.
              try {
                const restoredId = await invoke<string | null>("restore_last_session");
                if (restoredId) {
                  sessionIdRef.current = restoredId;
                  lastSavedMsgCount.current = 0;
                  const raw = await invoke<string>("load_session", { sessionId: restoredId });
                  const stored = JSON.parse(raw) as {
                    role: string; content: string; timestamp: number; thinking_content?: string;
                  }[];
                  loadMessages(stored.map((m) => ({
                    id: crypto.randomUUID(),
                    role: m.role as "user" | "assistant",
                    content: m.content,
                    timestamp: m.timestamp,
                    thinkingContent: m.thinking_content,
                  })));
                  const g = await invoke<GoalState | null>("get_goal_cmd");
                  setGoal(g);
                }
              } catch { /* no session to restore */ }
            })
            .catch(() => setShowSettings(true));
        });
      } else {
        setShowSettings(true);
      }
      setInitialized(true);
    });
  }, []);

  // Listen for agent-stream events to capture diffs and tasks
  useEffect(() => {
    if (!apiKeyConfigured) return;
    let unlisten: (() => void) | null = null;

    import("@tauri-apps/api/event").then(({ listen }) => {
      listen<{ type: string; path?: string; original?: string; modified?: string; tasks_json?: string }>(
        "agent-stream",
        (event) => {
          const p = event.payload as Record<string, unknown>;
          if (p.type === "diff_created" && p.path && p.original !== undefined && p.modified !== undefined) {
            setDiffs((prev) => [
              ...prev,
              {
                path: p.path as string,
                original: p.original as string,
                modified: p.modified as string,
                status: "pending",
              },
            ]);
          }
          if (p.type === "task_list" && p.tasks_json) {
            try {
              const parsed = JSON.parse(p.tasks_json as string) as TaskItem[];
              setTasks(parsed);
            } catch { /* ignore parse errors */ }
          }
          if (p.type === "goal_update" && p.goal_json) {
            try {
              setGoal(JSON.parse(p.goal_json as string) as GoalState);
            } catch { /* ignore parse errors */ }
          }
          if (p.type === "subagent_started") {
            setAgents((prev) => [
              ...prev,
              {
                id: p.child_session_id as string,
                parentId: p.parent_session_id as string,
                status: "running",
                summary: "",
              },
            ]);
          }
          if (p.type === "subagent_finished") {
            setAgents((prev) =>
              prev.map((a) =>
                a.id === (p.child_session_id as string)
                  ? { ...a, status: (p.status === "ok" ? "done" : "error") as Subagent["status"], summary: (p.summary as string) || a.summary }
                  : a
              )
            );
          }
          if (p.type === "trajectory") {
            setTrajectory((prev) => [
              ...prev.slice(-199),
              { type: p.event_type as string, summary: (p.summary as string) || "" },
            ]);
          }
          if (p.type === "auto_turn" && typeof p.index === "number" && typeof p.max === "number") {
            setAutoTurn({ index: p.index as number, max: p.max as number });
          }
          if (p.type === "auto_turn_end") {
            setAutoTurn(null);
            if (typeof p.reason === "string") {
              setAutoTurnEnd(p.reason as string);
              window.setTimeout(() => setAutoTurnEnd(null), 5000);
              addMessage({
                id: crypto.randomUUID(),
                role: "system",
                content: t(`goal.autoEnd.${p.reason}`),
                timestamp: Date.now(),
              });
            }
          }
        }
      ).then((fn) => { unlisten = fn; });
    });

    return () => { unlisten?.(); };
  }, [apiKeyConfigured]);

  const addMessage = useCallback((msg: Message) => {
    setState((prev) => ({ ...prev, messages: [...prev.messages, msg] }));
  }, []);

  const setThinkingMode = useCallback((mode: ThinkingMode) => {
    setState((prev) => ({ ...prev, thinkingMode: mode }));
  }, []);

  const setProcessing = useCallback((val: boolean) => {
    setState((prev) => ({ ...prev, isProcessing: val }));
  }, []);

  const handleSetGoal = useCallback((objective: string, tokenBudget: number | null) => {
    import("@tauri-apps/api/core").then(({ invoke }) => {
      invoke("set_goal_cmd", { objective, tokenBudget })
        .then(async () => {
          const g = await invoke<GoalState | null>("get_goal_cmd");
          setGoal(g);
          if (goalMode) setGoalKickoff((k) => k + 1);
        })
        .catch(() => { /* keep previous goal */ });
    });
  }, [goalMode]);

  const handleSetAsGoal = useCallback((objective: string) => {
    // Promote the current composer text to the active goal (Codex's
    // "set as goal"): set it, show the goal card, and kick off immediately.
    import("@tauri-apps/api/core").then(({ invoke }) => {
      invoke("set_goal_cmd", { objective, tokenBudget: null })
        .then(async () => {
          const g = await invoke<GoalState | null>("get_goal_cmd");
          setGoal(g);
          setRightPanelTab("tasks");
          if (goalMode) setGoalKickoff((k) => k + 1);
        })
        .catch(() => { /* keep previous goal */ });
    });
  }, [goalMode]);

  const handleToggleGoalMode = useCallback((enabled: boolean) => {
    import("@tauri-apps/api/core").then(({ invoke }) => {
      invoke("set_goal_mode_cmd", { enabled })
        .then(() => {
          setGoalMode(enabled);
          const updated = { ...stored, goalMode: enabled };
          saveSettings(updated);
          setStored(updated);
        })
        .catch(() => { /* keep previous */ });
    });
  }, [stored]);

  const handleSetMaxAutoTurns = useCallback((n: number) => {
    import("@tauri-apps/api/core").then(({ invoke }) => {
      invoke("set_goal_max_auto_turns_cmd", { max: n })
        .then(() => setMaxAutoTurns(n))
        .catch(() => { /* keep previous */ });
    });
  }, []);

  const handleToggleGoalPause = useCallback((paused: boolean) => {
    import("@tauri-apps/api/core").then(({ invoke }) => {
      invoke("set_goal_paused_cmd", { paused })
        .then(() => invoke<GoalState | null>("get_goal_cmd"))
        .then((g) => {
          setGoal(g);
          // Resuming must restart the work — changing status alone leaves the
          // agent waiting for input, which feels like it got stuck.
          if (!paused && g && goalMode) setGoalKickoff((k) => k + 1);
        })
        .catch(() => { /* keep previous */ });
    });
  }, [goalMode]);

  const updateContextUsage = useCallback((val: number) => {
    setState((prev) => ({ ...prev, contextUsage: val }));
  }, []);

  // 每轮回复带回累计 token/成本，驱动 Toolbar 和 Monitor 的真实统计
  const updateSessionUsage = useCallback((tokens: TokenUsage, cost: number) => {
    setState((prev) => ({ ...prev, sessionTokens: tokens, sessionCost: cost }));
  }, []);

  const handleConfigured = useCallback(async (apiKey: string, workspacePath: string, model: string, thinkBudget: number, deepBudget: number, language: Language, timeHarness: boolean, useHarness: boolean, sandbox: string) => {
    const settings = { apiKey, workspacePath, model, thinkBudget, deepBudget, language, timeHarness, useHarness, sandbox, goalMode };
    await saveSettings(settings);
    setStored(settings);
    setLanguage(language);
    setUseHarness(useHarness);
    setSandbox(sandbox);
    setApiKeyConfigured(true);
    setShowSettings(false);
    // Push the reasoning dial to the agent (safe to fire even before init —
    // the command just no-ops if the agent isn't connected yet).
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("set_thinking_budgets", { thinkBudget, deepBudget });
      await invoke("set_time_harness", { enabled: timeHarness });
    } catch { /* agent not ready — defaults apply */ }
  }, []);

  // One-click Flash ⇄ Pro toggle — live, keeps the conversation.
  // Damped: an 800ms cooldown locks the knob after a switch (no rapid flips),
  // and a visible system note confirms the context travelled with the model.
  const [switchLocked, setSwitchLocked] = useState(false);
  const switchCooldownRef = useRef(0);

  const handleSwitchModel = useCallback(async () => {
    if (Date.now() - switchCooldownRef.current < 800) return;
    switchCooldownRef.current = Date.now();
    const next = stored.model === "deepseek-v4-flash" ? "deepseek-v4-pro" : "deepseek-v4-flash";
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke<string>("switch_model", { model: next });
      const updated = { ...stored, model: next };
      await saveSettings(updated);
      setStored(updated);
      setSwitchLocked(true);
      setTimeout(() => setSwitchLocked(false), 800);
      // 让"上下文跟着模型走"看得见——消息、推理链、工具历史全部随新模型发送
      const n = state.messages.filter((m) => m.role === "user" || m.role === "assistant").length;
      addMessage({
        id: crypto.randomUUID(),
        role: "system",
        content: t("app.modelSwitched", {
          model: next === "deepseek-v4-flash" ? "DeepSeek V4 Flash" : "DeepSeek V4 Pro",
          n,
        }),
        timestamp: Date.now(),
      });
    } catch (err) {
      console.error("Model switch failed:", err);
    }
  }, [stored, state.messages, addMessage, t, goalMode]);

  const openFile = useCallback(async (filePath: string) => {
    const { invoke } = await import("@tauri-apps/api/core");
    try {
      const content = await invoke<string>("read_workspace_file", { path: filePath });
      const ext = filePath.split(".").pop() || "txt";
      setState((prev) => ({
        ...prev,
        currentFile: { path: filePath, content, language: ext, modified: false },
      }));
      setMdPreview(false);
    } catch (err) {
      console.error("Failed to open file:", err);
    }
  }, []);

  const loadMessages = useCallback((msgs: Message[]) => {
    setState((prev) => ({ ...prev, messages: msgs }));
    setChatKey((k) => k + 1);
  }, []);

  const newSession = useCallback(() => {
    setState(createInitialState());
    setDiffs([]);
    setTasks([]);
    setGoal(null);
    setMdPreview(false);
    setChatKey((k) => k + 1);
    sessionIdRef.current = crypto.randomUUID();
    lastSavedMsgCount.current = 0;
  }, []);

  const isMarkdown = (path: string) => {
    const ext = path.split(".").pop()?.toLowerCase() ?? "";
    return ext === "md" || ext === "markdown" || ext === "mdx";
  };

  // ── Slash commands — type "/" in the chat box to summon ──
  const modeLabel = state.thinkingMode === "non-think" ? "Fast" : state.thinkingMode === "think-high" ? "Think" : "Deep";
  const modelLabel = (stored.model || "deepseek-v4-flash").includes("flash") ? "Flash" : "Pro";

  const slashCommands = useMemo<SlashCommand[]>(() => {
    const helpText = [
      t("slash.helpTitle"),
      "",
      `- \`/help\` — ${t("slash.helpIntro")}`,
      `- \`/model\` — ${t("slash.modelDesc", { model: modelLabel })}`,
      `- \`/mode\` — ${t("slash.modeDesc", { mode: modeLabel })}`,
      `- \`/clear\` — ${t("slash.clearDesc")}`,
      `- \`/new\` — ${t("slash.newDesc")}`,
      `- \`/compact\` — ${t("slash.compactDesc")}`,
      `- \`/settings\` — ${t("slash.settingsDesc")}`,
      `- \`/sidebar\` — ${t("slash.sidebarDesc")}`,
      "",
      t("slash.shortcuts"),
    ].join("\n");

    return [
      {
        id: "help", title: t("slash.help"), description: t("slash.helpIntro"),
        run: () => {
          addMessage({ id: crypto.randomUUID(), role: "system", content: helpText, timestamp: Date.now() });
        },
      },
      {
        id: "model", title: t("slash.model"), description: t("slash.modelDesc", { model: modelLabel }),
        run: handleSwitchModel,
      },
      {
        id: "mode", title: t("slash.mode"), description: t("slash.modeDesc", { mode: modeLabel }),
        run: () => {
          const order: ThinkingMode[] = ["non-think", "think-high", "think-max"];
          const next = order[(order.indexOf(state.thinkingMode) + 1) % order.length];
          setThinkingMode(next);
        },
      },
      {
        id: "clear", title: t("slash.clear"), description: t("slash.clearDesc"),
        run: async () => {
          const { invoke } = await import("@tauri-apps/api/core");
          try { await invoke("clear_conversation"); } catch { /* ignore */ }
          newSession();
        },
      },
      {
        id: "new", title: t("slash.new"), description: t("slash.newDesc"),
        run: newSession,
      },
      {
        id: "compact", title: t("slash.compact"), description: t("slash.compactDesc"),
        run: async () => {
          try {
            const { invoke } = await import("@tauri-apps/api/core");
            const msg = await invoke<string>("compact_now");
            addMessage({ id: crypto.randomUUID(), role: "system", content: msg, timestamp: Date.now() });
          } catch (err) {
            addMessage({ id: crypto.randomUUID(), role: "system", content: `${t("app.error")}${String(err)}`, timestamp: Date.now() });
          }
        },
      },
      {
        id: "settings", title: t("slash.settings"), description: t("slash.settingsDesc"),
        run: () => setShowSettings(true),
      },
      {
        id: "sidebar", title: t("slash.sidebar"), description: t("slash.sidebarDesc"),
        run: () => setShowSidebar((v) => !v),
      },
    ];
  }, [state.thinkingMode, stored.model, handleSwitchModel, newSession, setThinkingMode, setShowSettings, setShowSidebar, addMessage, t, modeLabel, modelLabel]);

  // ── Auto-save: every finished turn is written to History without clicking Save ──
  const sessionIdRef = useRef<string>(crypto.randomUUID());
  const lastSavedMsgCount = useRef(0);
  const prevProcessing = useRef(false);

  const autoSave = useCallback(async (msgs: Message[]) => {
    const usable = msgs.filter((m) => m.role === "user" || m.role === "assistant");
    if (usable.length === 0 || usable.length === lastSavedMsgCount.current) return;
    lastSavedMsgCount.current = usable.length;
    const firstUser = usable.find((m) => m.role === "user");
    const title = firstUser ? firstUser.content.slice(0, 50).replace(/\n/g, " ") : "Untitled";
    const stored = usable.map((m) => ({
      role: m.role,
      content: m.content,
      timestamp: m.timestamp,
      thinking_content: m.thinkingContent || null,
    }));
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("save_current_session", {
        sessionId: sessionIdRef.current,
        title,
        messagesJson: JSON.stringify(stored),
      });
    } catch (err) {
      console.error("Auto-save failed:", err);
    }
  }, []);

  // 每轮处理结束（正常完成或取消）时自动落盘
  useEffect(() => {
    if (!apiKeyConfigured) return;
    if (prevProcessing.current && !state.isProcessing) {
      void autoSave(state.messages);
    }
    prevProcessing.current = state.isProcessing;
  }, [state.isProcessing, state.messages, apiKeyConfigured, autoSave]);

  // Durability heartbeat: a crash/close between turns must not lose the
  // latest conversation — save periodically and when the page hides.
  useEffect(() => {
    if (!apiKeyConfigured) return;
    const timer = window.setInterval(() => void autoSave(state.messages), 30_000);
    const onHide = () => void autoSave(state.messages);
    document.addEventListener("visibilitychange", onHide);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onHide);
    };
  }, [apiKeyConfigured, state.messages, autoSave]);

  const handleAcceptDiff = (path: string) => {
    setDiffs((prev) => prev.map((d) => (d.path === path ? { ...d, status: "accepted" as const } : d)));
  };

  // Reject = 真正把文件回滚到修改前的内容（Cursor 行为）
  const rollbackFile = useCallback(async (path: string) => {
    const diff = diffs.find((d) => d.path === path && d.status === "pending");
    if (!diff) return;
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("write_workspace_file", { path, content: diff.original });
    } catch (err) {
      console.error("Rollback failed:", err);
    }
    setDiffs((prev) => prev.map((d) => (d.path === path ? { ...d, status: "rejected" as const } : d)));
  }, [diffs]);

  const handleRejectDiff = (path: string) => { void rollbackFile(path); };

  const handleAcceptAll = () => setDiffs((prev) => prev.map((d) => d.status === "pending" ? { ...d, status: "accepted" as const } : d));
  const handleRejectAll = () => {
    const pending = diffs.filter((d) => d.status === "pending").map((d) => d.path);
    pending.forEach((p) => void rollbackFile(p));
  };

  if (!initialized) {
    return <div className="app-loading"><span>DeepSeek Code</span></div>;
  }

  return (
    <div className="app-container">
      <Toolbar
        contextUsage={state.contextUsage}
        sessionCost={state.sessionCost}
        sessionTokens={state.sessionTokens}
        showMonitor={rightPanelVisible}
        showSidebar={showSidebar}
        showTools={toolsPanelVisible}
        onToggleMonitor={() => setRightPanelVisible((v) => !v)}
        onToggleSidebar={() => setShowSidebar((v) => !v)}
        onToggleTools={() => setToolsPanelVisible((v) => !v)}
        onOpenSettings={() => setShowSettings(true)}
        apiKeyConfigured={apiKeyConfigured}
      />
      <div className="app-body">
        <div className={`sidebar ${showSidebar ? "" : "sidebar-hidden"}`}>
          <div className="sidebar-tabs">
            <button className={`sidebar-tab ${sidebarTab === "files" ? "active" : ""}`} onClick={() => setSidebarTab("files")}><Icon name="files" />{t("sidebar.files")}</button>
            <button className={`sidebar-tab ${sidebarTab === "history" ? "active" : ""}`} onClick={() => setSidebarTab("history")}><Icon name="history" />{t("sidebar.history")}</button>
            <button className={`sidebar-tab ${sidebarTab === "agents" ? "active" : ""}`} onClick={() => setSidebarTab("agents")}><Icon name="agents" />{t("sidebar.agents")}</button>
            <button className={`sidebar-tab ${sidebarTab === "memory" ? "active" : ""}`} onClick={() => setSidebarTab("memory")}><Icon name="memory" />{t("sidebar.memory")}</button>
          </div>
          <div className="sidebar-panels">
            <div className={`sidebar-panel ${sidebarTab === "files" ? "active" : ""}`}>
              <Sidebar onOpenFile={openFile} workspacePath={stored.workspacePath} />
            </div>
            <div className={`sidebar-panel ${sidebarTab === "history" ? "active" : ""}`}>
              <SessionList
                onLoadSession={(msgs, id) => {
                  loadMessages(msgs);
                  if (id) { sessionIdRef.current = id; lastSavedMsgCount.current = 0; }
                  import("@tauri-apps/api/core").then(({ invoke }) => {
                    invoke<GoalState | null>("get_goal_cmd").then(setGoal).catch(() => {});
                  });
                }}
                onNewSession={newSession}
              />
            </div>
            <div className={`sidebar-panel ${sidebarTab === "agents" ? "active" : ""}`}>
              <AgentsPanel agents={agents} />
            </div>
            <div className={`sidebar-panel ${sidebarTab === "memory" ? "active" : ""}`}>
              <MemoryPanel />
            </div>
          </div>
        </div>
        <div className="app-main">
          {state.currentFile ? (
            isMarkdown(state.currentFile.path) && mdPreview ? (
              <MarkdownPreview
                file={state.currentFile}
                onEdit={() => setMdPreview(false)}
                onClose={() => setState((prev) => ({ ...prev, currentFile: null }))}
              />
            ) : (
              <EditorPanel
                currentFile={state.currentFile}
                onFileChange={(f) => setState((prev) => ({ ...prev, currentFile: f }))}
                onClose={() => setState((prev) => ({ ...prev, currentFile: null }))}
                onPreview={isMarkdown(state.currentFile.path) ? () => setMdPreview(true) : undefined}
              />
            )
          ) : (
            <ChatPanel
              key={chatKey}
              messages={state.messages}
              isProcessing={state.isProcessing}
              thinkingMode={state.thinkingMode}
              apiKeyConfigured={apiKeyConfigured}
              useHarness={useHarness}
              harnessMode={harnessMode}
              sandbox={sandbox}
              model={stored.model || "deepseek-v4-flash"}
              onSwitchModel={handleSwitchModel}
              switchDisabled={switchLocked}
              onThinkingModeChange={setThinkingMode}
              onHarnessModeChange={setHarnessMode}
              goalKickoff={goalKickoff}
              autoTurn={autoTurn}
              onSetAsGoal={handleSetAsGoal}
              goalObjective={goal?.objective ?? null}
              onSend={addMessage}
              onSetProcessing={setProcessing}
              onContextUpdate={updateContextUsage}
              onUsageUpdate={updateSessionUsage}
              slashCommands={slashCommands}
            />
          )}
        </div>
        <div className={`right-panel ${rightPanelVisible ? "" : "rp-hidden"}`}>
          <div className="right-panel-tabs">
            <button className={`rp-tab ${rightPanelTab === "diffs" ? "active" : ""}`} onClick={() => setRightPanelTab("diffs")}>
              <Icon name="diffs" />
              <span>{t("diff.title")}</span>
              {diffs.filter((d) => d.status === "pending").length > 0 && (
                <span className="rp-tab-count">{diffs.filter((d) => d.status === "pending").length}</span>
              )}
            </button>
            <button className={`rp-tab ${rightPanelTab === "tasks" ? "active" : ""}`} onClick={() => setRightPanelTab("tasks")}>
              <Icon name="tasks" />
              <span>{t("tasks.tasks")}</span>
              {(goal && goal.plan.length > 0) || tasks.length > 0 ? (
                <span className="rp-tab-count">
                  {goal ? `${goal.plan.filter((s) => s.status === "completed").length}/${goal.plan.length}` : `${tasks.filter((t) => t.status === "completed").length}/${tasks.length}`}
                </span>
              ) : null}
            </button>
            <button className={`rp-tab ${rightPanelTab === "monitor" ? "active" : ""}`} onClick={() => setRightPanelTab("monitor")}>
              <Icon name="monitor" />
              {t("toolbar.monitor")}
            </button>
            <button className={`rp-tab ${rightPanelTab === "tools" ? "active" : ""}`} onClick={() => setRightPanelTab("tools")}>
              <Icon name="tools" />
              {t("tools.title")}
            </button>
            <button className={`rp-tab ${rightPanelTab === "trajectory" ? "active" : ""}`} onClick={() => setRightPanelTab("trajectory")}>
              <Icon name="trajectory" />
              {t("trajectory.title")}
            </button>
            <div className="rp-tab-spacer" />
            <button
              className="rp-collapse-btn"
              onClick={() => setRightPanelVisible(false)}
              title={t("toolbar.collapsePanel")}
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <polyline points="9 18 15 12 9 6" />
              </svg>
            </button>
          </div>
          <div className={`rp-content ${rightPanelTab === "diffs" ? "active" : ""}`}>
            <DiffPanel diffs={diffs} onAccept={handleAcceptDiff} onReject={handleRejectDiff} onAcceptAll={handleAcceptAll} onRejectAll={handleRejectAll} />
          </div>
          <div className={`rp-content ${rightPanelTab === "tasks" ? "active" : ""}`}>
            <TaskPanel
              tasks={tasks}
              goal={goal}
              onSetGoal={handleSetGoal}
              goalMode={goalMode}
              onToggleGoalMode={handleToggleGoalMode}
              autoTurn={autoTurn}
              autoTurnEnd={autoTurnEnd}
              maxAutoTurns={maxAutoTurns}
              onSetMaxAutoTurns={handleSetMaxAutoTurns}
              onToggleGoalPause={handleToggleGoalPause}
              calibrationFactor={(stored.model || "deepseek-v4-flash").includes("pro") ? 0.56 : 3.71}
            />
          </div>
          <div className={`rp-content ${rightPanelTab === "monitor" ? "active" : ""}`}>
            <ContextMonitor
              contextUsage={state.contextUsage}
              sessionCost={state.sessionCost}
              sessionTokens={state.sessionTokens}
              thinkingMode={state.thinkingMode}
              messagesCount={state.messages.length}
            />
          </div>
          <div className={`rp-content ${rightPanelTab === "tools" ? "active" : ""}`}>
            <ToolCatalog mode={harnessMode} />
          </div>
          <div className={`rp-content ${rightPanelTab === "trajectory" ? "active" : ""}`}>
            <TrajectoryPanel entries={trajectory} />
          </div>
        </div>
        {/* Tools sidebar — terminal + browser, toggled from the toolbar icon */}
        <div
          className={`tools-panel ${toolsPanelVisible ? "" : "rp-hidden"}`}
          style={{ width: toolsWidth }}
        >
          <div className="tools-resize-handle" onMouseDown={startToolsResize} title={t("toolbar.resizeTools")} />
          <div className="tools-panel-tabs">
            <button className={`rp-tab ${toolsPanelTab === "terminal" ? "active" : ""}`} onClick={() => setToolsPanelTab("terminal")}>
              {t("sidebar.terminal")}
            </button>
            <button className={`rp-tab ${toolsPanelTab === "browser" ? "active" : ""}`} onClick={() => setToolsPanelTab("browser")}>
              {t("sidebar.browser")}
            </button>
            <div className="rp-tab-spacer" />
            <button
              className="rp-collapse-btn"
              onClick={() => setToolsPanelVisible(false)}
              title={t("toolbar.collapseTools")}
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <polyline points="9 18 15 12 9 6" />
              </svg>
            </button>
          </div>
          <div className={`rp-content ${toolsPanelTab === "terminal" ? "active" : ""}`}>
            <TerminalPanel active={toolsPanelVisible && toolsPanelTab === "terminal"} />
          </div>
          <div className={`rp-content ${toolsPanelTab === "browser" ? "active" : ""}`}>
            <BrowserPanel />
          </div>
        </div>
      </div>
      {showSettings && (
        <SettingsModal stored={stored} onClose={() => setShowSettings(false)} onConfigured={handleConfigured} />
      )}
    </div>
  );
}
