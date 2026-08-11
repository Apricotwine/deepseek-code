import { useMemo, useState } from "react";
import { useI18n } from "../i18n";

export interface TaskItem {
  id: string;
  content: string;
  status: "pending" | "in_progress" | "completed" | "cancelled";
}

export type GoalStatus = "active" | "paused" | "blocked" | "budget_limited" | "complete";
export type StepStatus = "pending" | "in_progress" | "completed" | "cancelled" | "blocked";

export interface PlanStep {
  id: string;
  content: string;
  status: StepStatus;
  created_at: number;
  started_at: number | null;
  completed_at: number | null;
  blocked_reason: string | null;
  estimate_sec: number | null;
}

export interface GoalState {
  id: string;
  objective: string;
  status: GoalStatus;
  token_budget: number | null;
  tokens_used: number;
  time_used_seconds: number;
  created_at: number;
  updated_at: number;
  plan: PlanStep[];
  consecutive_blocked_turns: number;
}

function fmtDuration(sec: number): string {
  if (!Number.isFinite(sec) || sec < 0) return "—";
  if (sec < 60) return `${Math.round(sec)}s`;
  const m = Math.floor(sec / 60);
  if (m < 60) return `${m}m ${Math.round(sec % 60)}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}

function fmtAge(ms: number): string {
  if (!ms || ms <= 0) return "";
  const mins = Math.floor((Date.now() - ms) / 60000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}min ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 48) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

interface TaskPanelProps {
  tasks: TaskItem[];
  goal: GoalState | null;
  onSetGoal: (objective: string, tokenBudget: number | null) => void;
  goalMode: boolean;
  onToggleGoalMode: (enabled: boolean) => void;
  autoTurn: { index: number; max: number } | null;
  autoTurnEnd: string | null;
  maxAutoTurns: number;
  onSetMaxAutoTurns: (n: number) => void;
  onToggleGoalPause: (paused: boolean) => void;
  /** Measured calibration: Flash ~3.71 (overestimates), Pro ~0.56 (under). */
  calibrationFactor?: number;
}

export default function TaskPanel({
  tasks, goal, onSetGoal, goalMode, onToggleGoalMode, autoTurn, autoTurnEnd,
  maxAutoTurns, onSetMaxAutoTurns, onToggleGoalPause, calibrationFactor = 3.71,
}: TaskPanelProps) {
  const { t } = useI18n();
  const [objective, setObjective] = useState("");
  const [budget, setBudget] = useState("");
  const [showEditor, setShowEditor] = useState(!goal);

  const completed = useMemo(
    () => (goal ? goal.plan.filter((s) => s.status === "completed").length : tasks.filter((x) => x.status === "completed").length),
    [goal, tasks]
  );
  const total = goal ? goal.plan.length : tasks.length;
  const inProgress = goal
    ? goal.plan.filter((s) => s.status === "in_progress").length
    : tasks.filter((x) => x.status === "in_progress").length;

  const submitGoal = () => {
    const trimmed = objective.trim();
    if (!trimmed) return;
    const parsedBudget = budget.trim() ? Number(budget.trim()) : null;
    onSetGoal(trimmed, parsedBudget && parsedBudget > 0 ? Math.round(parsedBudget) : null);
    setShowEditor(false);
  };

  return (
    <div className="task-panel">
      <div className="task-header">
        <span className="task-title">{t("tasks.tasks")}</span>
        <span className="task-summary">
          {goal
            ? `${completed}/${total} ${t("tasks.doneShort")}${inProgress > 0 ? ` · ${inProgress} ${t("tasks.activeShort")}` : ""}`
            : `${completed}/${total} ${t("tasks.doneShort")}${inProgress > 0 ? ` · ${inProgress} ${t("tasks.activeShort")}` : ""}`}
        </span>
      </div>

      <div className="task-list">
        {goal && (
          <div className={`goal-card goal-${goal.status}`}>
            <div className="goal-row">
              <span className={`goal-status goal-status-${goal.status}`}>{t(`goal.status.${goal.status}`)}</span>
              <span className="goal-row-actions">
                <label className="goal-mode-toggle" title={t("goal.modeDesc")}>
                  <input
                    type="checkbox"
                    checked={goalMode}
                    onChange={(e) => onToggleGoalMode(e.target.checked)}
                  />
                  <span>{t("goal.mode")}</span>
                </label>
                {(goal.status === "active" || goal.status === "paused") && (
                  <button
                    className="goal-edit-btn goal-pause-btn"
                    onClick={() => onToggleGoalPause(goal.status !== "paused")}
                  >
                    {goal.status === "paused" ? t("goal.resume") : t("goal.pause")}
                  </button>
                )}
                <button className="goal-edit-btn" onClick={() => setShowEditor((v) => !v)}>
                  {showEditor ? t("goal.collapse") : t("goal.edit")}
                </button>
              </span>
            </div>
            <div className="goal-objective" title={goal.objective}>{goal.objective}</div>
            <div className="goal-meta">
              <span>{t("goal.tokens", { used: goal.tokens_used.toLocaleString() })}</span>
              {goal.token_budget && (
                <span className="goal-budget-bar" title={`${goal.tokens_used} / ${goal.token_budget}`}>
                  <span
                    className="goal-budget-fill"
                    style={{ width: `${Math.min(100, (goal.tokens_used / goal.token_budget) * 100)}%` }}
                  />
                </span>
              )}
              <span>{t("goal.time", { time: fmtDuration(goal.time_used_seconds) })}</span>
            </div>
            {goal.status === "blocked" && (
              <div className="goal-blocked-note">
                {t("goal.blockedNote", { n: goal.consecutive_blocked_turns })}
              </div>
            )}
            {goal.status === "budget_limited" && <div className="goal-blocked-note">{t("goal.budgetNote")}</div>}
            {autoTurn && (
              <div className="goal-auto">
                {t("goal.autoAdvancing", { index: autoTurn.index, max: autoTurn.max })}
              </div>
            )}
            {autoTurnEnd && (
              <div className="goal-auto-end">
                {t(`goal.autoEnd.${autoTurnEnd}`, {})}
              </div>
            )}

            {goal.plan.length > 0 && (
              <div className="goal-plan">
                {goal.plan.map((step) => (
                  <div key={step.id} className={`task-item task-${step.status}`}>
                    <span className="task-status-icon">
                      {step.status === "completed" ? "✓" : step.status === "in_progress" ? "·" : step.status === "cancelled" ? "×" : step.status === "blocked" ? "!" : "○"}
                    </span>
                    <span className="task-body">
                      <span className="task-content">{step.content}</span>
                      <span className="task-meta">
                        {step.status === "in_progress" && step.started_at && `active ${fmtAge(step.started_at)}`}
                        {step.status === "completed" && step.completed_at && `done ${fmtAge(step.completed_at)}`}
                        {step.status === "pending" && `created ${fmtAge(step.created_at)}`}
                        {step.status === "in_progress" && step.estimate_sec != null && (
                          <> · ETA {fmtDuration(step.estimate_sec)} → {fmtDuration(step.estimate_sec / calibrationFactor)}</>
                        )}
                        {step.status === "blocked" && step.blocked_reason && ` · ${step.blocked_reason}`}
                      </span>
                      {(step.status === "pending" || step.status === "blocked" || step.status === "cancelled") &&
                        Date.now() - step.created_at > 24 * 3600_000 && (
                          <span className="task-stale">{t("goal.stale")}</span>
                        )}
                    </span>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}

        {showEditor && (
          <div className="goal-editor">
            <div className="goal-editor-label">{t("goal.prompt")}</div>
            <textarea
              className="goal-input"
              value={objective}
              onChange={(e) => setObjective(e.target.value)}
              placeholder={t("goal.placeholder")}
              rows={2}
            />
            <div className="goal-editor-row">
              <input
                className="goal-budget-input"
                value={budget}
                onChange={(e) => setBudget(e.target.value.replace(/[^0-9]/g, ""))}
                placeholder={t("goal.budgetPlaceholder")}
              />
              <input
                className="goal-budget-input goal-turns-input"
                type="number"
                min={1}
                max={100}
                value={maxAutoTurns}
                onChange={(e) => {
                  const n = Number(e.target.value);
                  if (n >= 1) onSetMaxAutoTurns(Math.min(100, Math.round(n)));
                }}
                title={t("goal.maxTurnsDesc")}
              />
              <button className="goal-set-btn" onClick={submitGoal} disabled={!objective.trim()}>
                {t("goal.set")}
              </button>
            </div>
          </div>
        )}

        {!goal && !showEditor && (
          <div className="task-empty">{t("tasks.empty")}</div>
        )}
      </div>
    </div>
  );
}
