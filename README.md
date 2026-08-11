# DeepSeek Code · 小鲸鱼

为 **DeepSeek V4** 原生设计的桌面 AI 编程工作站（Tauri 2 + React + Rust），围绕
1M 上下文窗口打造：时间感知层、目标模式（自动推进）、任务流水线、会话持久化。

## ✨ 核心特性

- **1M 上下文，无压缩优先**：整仓索引 + 软遗忘（>90% 触发），长会话健康
- **时间感知层（TAL）**：单一权威时钟、`data_time` 新鲜度标注、决策链提示词、
  per-model ETA 校准、时区健壮性——让模型分清"昨天"与"现在"
- **目标模式（Goal Mode）**：设定目标即开工，自动推进直到完成；暂停/继续、
  token 预算、轮次上限、ESC 打断、停止原因可见（对齐 Codex 线程目标）
- **任务流水线**：思考链折叠，过程以"任务 + 行动"卡片呈现，结果消息完整渲染
- **会话持久化**：存应用数据目录，重启自动恢复；完整 tool 块保真度
- **缓存经济学**：分桶年龄 + 尾端时钟，前缀缓存命中实测 ~99%
- 并行工具执行、Diff 审查、内置终端/浏览器侧边栏（可拖拽调宽）、中英文 i18n

## 🏗 架构

```
src/               React 前端（ChatPanel / TaskPanel / Toolbar / 侧边栏…）
src-tauri/src/
  agent.rs         Agent 主循环（PERCEIVE → REASON → VERIFY）+ 目标模式自动推进
  context.rs       上下文引擎 + 时间感知层规则
  tools.rs         本地工具执行（文件 / shell / 记忆…）+ 内容级新鲜度嗅探
  api.rs           DeepSeek Anthropic 兼容端点（流式 / 批量回退 / 重试退避）
  session.rs       会话持久化（含 goal + plan）
benchmarks/        时间矩阵 T1-T9、目标推进探针、缓存命中探针
paper/             论文（LaTeX + Markdown + 配图）
```

## 🚀 快速开始

```bash
npm install
npm run tauri dev
```

首次启动在设置里填入 DeepSeek API Key 即可。

## 📊 验证资产

- `cargo test`：19 个单测（时间戳注入、计划语义、停止条件、协议安全）
- 时间感知基准：`benchmarks/temporal_harness_bench.py --ablate`（T1-T9）
- 目标模式实测：3/3 目标自动完成，平均 4.7 轮，完成前必验证
- 缓存命中：TAL 开启后 ~99% 前缀缓存保留

详见 [时间戳harness测试报告.md](时间戳harness测试报告.md) 与
[设计审查与优化清单.md](设计审查与优化清单.md)。

## 📄 文档

- [目标模式使用指南.md](目标模式使用指南.md)
- [时间戳harness测试报告.md](时间戳harness测试报告.md)
- [设计审查与优化清单.md](设计审查与优化清单.md)
- [paper/](paper/) — 论文稿

## 🔒 安全

- API Key 仅存于系统设置文件，仓库不含密钥
- 文件工具对工作区做符号链接规范化，防止逃逸
- 会话/记忆等私有数据目录已 gitignore

## ⚖️ License

MIT（见 LICENSE）
