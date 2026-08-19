# DeepSeek Code — Harness 内核运行时

这是把 IDE 内核替换为 DeepSeek Harness 后的运行时组装目录。Rust 后端通过
[`src-tauri/src/harness.rs`](../src-tauri/src/harness.rs) 以 stdio JSON-RPC
驱动本目录的 `cordis.yml` 组合。

## 文件

- `cordis.yml` — 运行时组合（SDK JSON-RPC server + llm-deepseek + 工具 + 会话持久化 + 子代理 + web 搜索 + plan/goal/workflow + 压缩）
- `persona.md` — 小鲸鱼人格（原 `context.rs::SYSTEM_PROMPT`，逐字迁移）
- `time-awareness.md` — 时间感知层规则（原 `context.rs::time_harness_system_section`，逐字迁移，clock-free + 决策链）

## 环境变量

| 变量 | 作用 |
|---|---|
| `DEEPSEEK_API_KEY` | DeepSeek 凭据（OpenAI 兼容端点 + 官方搜索） |
| `DSH_CWD` | agent 工作目录（文件/bash 工具的根） |
| `DSH_SESSION_ROOT` | JSONL 会话持久化目录 |
| `DSH_SYSTEM_PROMPT` | 系统提示词（前端组装 `persona.md` + `time-awareness.md` + 运行时上下文） |
| `DSH_REASONING_EFFORT` | `off` / `high` / `max`，对应 non-think / think / deep |

## 启动方式（对接 Rust 驱动）

```sh
node <dsh-bin.js> cordis.yml
```

stdout 只输出换行分隔 JSON-RPC 2.0：`initialize` / `session/prompt` / `shutdown`
入站，`session.event` / `session.status` / `subagent.*` 出站。

## 状态

- [x] 协议驱动（`harness.rs`）已实现并通过单元测试（6 例）
- [x] 人格 / TAL 规则文本已迁移
- [x] `cordis.yml` 组合已通过真实运行时加载 + 真实 DeepSeek 跑通（见 [VERIFICATION.md](VERIFICATION.md)）
- [x] 原生 headless 内核 + SDK stdio 桥 + 我们组合三条链路均用真实 DeepSeek 验证通过
- [x] 时间感知层 L0 活时钟（原生 `time-context`）+ 缓存记账（`usage.cacheReadTokens`）已验证
- [x] `harness.rs` 驱动真实 DeepSeek 端到端跑通（live 集成测试 + 全量 26 单测通过）
- [x] `harness_run` 二进制：真实工具调用 + 缓存/thinking 记账的事件转发层（可直接运行）
- [x] TAL-L1 工具结果 `data_time`/horizon 注解插件（[tal-tool-result.ts](tal-tool-result.ts)，已真机验证）
- [x] 后端接线：`send_harness_message` 命令 + 事件翻译层（[harness_backend.rs](../src-tauri/src/harness_backend.rs)），编译/单测通过
- [x] 前端开关：`useHarness` 设置贯通 store/App/SettingsModal/ChatPanel/i18n，`npm run build` 通过
- [x] 打包/vendor：`harness/package-runtime.sh` 产出闭包，Tauri resources + externalBin
  （官方单文件 node）打进 dmg（82M 下载，含运行时 217M + node 136M）
- [x] 后端解析：HARNESS_RUNTIME env → 打包资源 `_up_/harness-runtime` → 仓库本地
  闭包 → tsx checkout 回退；会话持久化到应用数据目录
- [ ] 可视化冒烟：运行 `tauri dev`，切到 Harness 内核发一条真实消息确认 UI 正常

## 运行时会话模型（实测确认）

- **每进程单次**：SDK jsonrpc 运行时服务完一个 `session/prompt` 后即退出——
  同一进程内第二次 prompt 报 Broken pipe；跨进程复用同一 session id 也不会
  续上下文（日志不更新）。
- **因此 IDE 自己承载跨轮连续性**：前端把最近对话（≤16 轮 / ≤12k 字符）作为
  `context` 传给 `send_harness_message`，后端拼进 prompt（"## 之前的对话…"）。
  实测：注入"记住 42"的历史后新会话能正确召回，且重复上下文命中前缀缓存
  （cacheReadTokens=3584）。
