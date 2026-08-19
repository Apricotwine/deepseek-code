import type { Subagent } from "../types";

interface Props {
  agents: Subagent[];
}

export default function AgentsPanel({ agents }: Props) {
  const childrenOf = (id: string) => agents.filter((a) => a.parentId === id);
  const roots = agents.filter((a) => !agents.some((o) => o.id === a.parentId));

  const node = (a: Subagent, depth: number) => (
    <div key={a.id} className="agent-node" style={{ marginLeft: depth * 10 }}>
      <div className="agent-node-head">
        <span className={`agent-dot agent-${a.status}`} />
        <span className="agent-id">#{a.id.slice(0, 8)}</span>
        <span className="agent-status">{a.status}</span>
      </div>
      {a.summary && <div className="agent-summary">{a.summary}</div>}
      {childrenOf(a.id).map((c) => node(c, depth + 1))}
    </div>
  );

  return (
    <div className="agents-panel">
      <div className="agents-root">
        <span className="agent-dot agent-root" />
        <span>主代理</span>
      </div>
      {roots.length === 0 ? (
        <div className="agents-empty">暂无子代理活动</div>
      ) : (
        roots.map((a) => node(a, 0))
      )}
    </div>
  );
}
