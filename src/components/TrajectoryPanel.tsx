import type { TrajectoryEntry } from "../types";

interface Props {
  entries: TrajectoryEntry[];
}

export default function TrajectoryPanel({ entries }: Props) {
  return (
    <div className="trajectory-panel">
      {entries.length === 0 ? (
        <div className="trajectory-empty">发送任务后，这里会显示会话的 append-only 轨迹。</div>
      ) : (
        entries.map((e, i) => (
          <div key={i} className="trajectory-entry">
            <span className="trajectory-type">{e.type}</span>
            {e.summary && <span className="trajectory-summary">{e.summary}</span>}
          </div>
        ))
      )}
    </div>
  );
}
