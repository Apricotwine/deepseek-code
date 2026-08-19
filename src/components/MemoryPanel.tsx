const SKILLS = [
  { name: "通用编码", desc: "读写、搜索、构建、调试代码" },
  { name: "研究检索", desc: "web 检索 + 史料/来源核验（配合时间戳）" },
  { name: "文档写作", desc: "结构化报告、综述、摘要" },
  { name: "shell 运维", desc: "持久终端 + 命令执行" },
];

export default function MemoryPanel() {
  return (
    <div className="memory-panel">
      <div className="memory-section">
        <div className="memory-title">项目记忆</div>
        <div className="memory-note">
          由 Harness 的 append-only 会话日志驱动；跨轮记忆/技能当前随会话持久化，
          实时管理待接入长驻内核后开放。
        </div>
      </div>
      <div className="memory-section">
        <div className="memory-title">可用技能</div>
        {SKILLS.map((s) => (
          <div key={s.name} className="memory-skill">
            <span className="memory-skill-name">{s.name}</span>
            <span className="memory-skill-desc">{s.desc}</span>
          </div>
        ))}
      </div>
      <div className="memory-section">
        <div className="memory-title">Agent 指令</div>
        <div className="memory-note">
          由 persona + 时间感知层规则构成，可在 harness/persona.md 与 time-awareness.md 调整。
        </div>
      </div>
    </div>
  );
}
