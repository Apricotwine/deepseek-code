import { useMemo, useState } from "react";
import { useI18n } from "../i18n";
import { diffLines, toSideBySideRows, type DiffLine } from "../diff";

export interface DiffEntry {
  path: string;
  original: string;
  modified: string;
  status: "pending" | "accepted" | "rejected";
}

interface DiffPanelProps {
  diffs: DiffEntry[];
  onAccept: (path: string) => void;
  onReject: (path: string) => void;
  onAcceptAll: () => void;
  onRejectAll: () => void;
}

type DiffView = "unified" | "split";

export default function DiffPanel({ diffs, onAccept, onReject, onAcceptAll, onRejectAll }: DiffPanelProps) {
  const { t } = useI18n();
  const [expandedFile, setExpandedFile] = useState<string | null>(null);
  const [view, setView] = useState<DiffView>("unified");
  const pending = diffs.filter((d) => d.status === "pending");

  return (
    <div className="diff-panel">
      <div className="diff-header">
        <span className="diff-title">{t("diff.changes", { n: pending.length })}</span>
        {pending.length > 0 && (
          <div className="diff-actions">
            <button className="diff-btn accept-all" onClick={onAcceptAll}>{t("diff.acceptAll")}</button>
            <button className="diff-btn reject-all" onClick={onRejectAll}>{t("diff.rejectAll")}</button>
          </div>
        )}
      </div>
      <div className="diff-list">
        {diffs.length === 0 && (
          <div className="diff-empty">{t("diff.noChanges")}</div>
        )}
        {diffs.map((d) => (
          <div key={d.path} className={`diff-item diff-${d.status}`}>
            <div
              className="diff-item-header"
              onClick={() => setExpandedFile(expandedFile === d.path ? null : d.path)}
            >
              <span className="diff-file-icon">{expandedFile === d.path ? "v" : ">"}</span>
              <span className="diff-file-path">{d.path}</span>
              {d.status === "pending" ? (
                <div className="diff-item-actions">
                  <button className="diff-accept" onClick={(e) => { e.stopPropagation(); onAccept(d.path); }}>{t("diff.accept")}</button>
                  <button className="diff-reject" onClick={(e) => { e.stopPropagation(); onReject(d.path); }}>{t("diff.reject")}</button>
                </div>
              ) : (
                <span className={`diff-status diff-status-${d.status}`}>
                  {d.status === "accepted" ? t("diff.accepted") : t("diff.rejected")}
                </span>
              )}
            </div>
            {expandedFile === d.path && (
              <DiffViewer original={d.original} modified={d.modified} path={d.path} view={view} onViewChange={setView} />
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

function DiffViewer({
  original,
  modified,
  path,
  view,
  onViewChange,
}: {
  original: string;
  modified: string;
  path: string;
  view: DiffView;
  onViewChange: (v: DiffView) => void;
}) {
  const { t } = useI18n();
  // Memoized: the Myers diff only recomputes when the file content changes.
  const diff = useMemo(() => diffLines(original, modified), [original, modified]);
  const rows = useMemo(() => toSideBySideRows(diff.lines), [diff]);

  return (
    <div className="diff-viewer">
      <div className="diff-viewer-toolbar">
        <span className={`diff-stats ${diff.deletions > 0 ? "has-del" : ""} ${diff.additions > 0 ? "has-add" : ""}`}>
          <span className="diff-stat-add">+{diff.additions}</span>
          <span className="diff-stat-del">−{diff.deletions}</span>
        </span>
        <div className="diff-view-toggle">
          <button className={`diff-view-btn ${view === "unified" ? "active" : ""}`} onClick={() => onViewChange("unified")}>
            {t("diff.unified")}
          </button>
          <button className={`diff-view-btn ${view === "split" ? "active" : ""}`} onClick={() => onViewChange("split")}>
            {t("diff.split")}
          </button>
        </div>
      </div>
      {view === "unified" ? (
        <div className="diff-lines unified">
          <div className="diff-hunk-header">--- a/{path}</div>
          <div className="diff-hunk-header">+++ b/{path}</div>
          {diff.lines.map((line, i) => (
            <DiffRow key={i} line={line} />
          ))}
        </div>
      ) : (
        <div className="diff-lines split">
          <div className="diff-split-head">
            <span>a/{path}</span>
            <span>b/{path}</span>
          </div>
          {rows.map((row, i) => (
            <div key={i} className="diff-split-row">
              <SplitCell line={row.old} />
              <SplitCell line={row.new} />
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function DiffRow({ line }: { line: DiffLine }) {
  return (
    <div className={`diff-row diff-row-${line.type}`}>
      <span className="diff-no">{line.type === "insert" ? "" : line.oldNo}</span>
      <span className="diff-no">{line.type === "delete" ? "" : line.newNo}</span>
      <span className="diff-sign">{line.type === "insert" ? "+" : line.type === "delete" ? "-" : " "}</span>
      <span className="diff-text">{line.text || " "}</span>
    </div>
  );
}

function SplitCell({ line }: { line?: DiffLine }) {
  if (!line) return <div className={`diff-split-cell diff-split-gap`} />;
  return (
    <div className={`diff-split-cell diff-split-${line.type}`}>
      <span className="diff-no">{line.type === "insert" ? "" : line.oldNo}</span>
      <span className="diff-no">{line.type === "delete" ? "" : line.newNo}</span>
      <span className="diff-text">{line.text || " "}</span>
    </div>
  );
}
