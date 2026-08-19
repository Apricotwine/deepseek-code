# TAL L1 工具结果注解插件 — 实现规格

L0 活时钟已由原生 `@deepseek-ai/dsh-time-context` 提供。剩余的时间感知层运行时注解是
**L1 新鲜度**：给每个工具结果打上生产时刻与新鲜度 horizon。

## 目标

在每个 `tool/result` 内容前注入：

```text
[data_time=2026-08-15T16:07:26+08:00 age=0min freshness=just_fetched horizon=30min]
<原工具结果>
```

## 扩展点

`tools/post-execute` 瀑布（见 `docs/tool-execution-pipeline.md`）允许 `accept / block /
replace / add context`。TAL 插件注册一个 post-execute 监听器，把结果内容替换为
`注解 + 原内容`。这是唯一需要自定义代码的部分。

## 插件骨架

```ts
export const name = 'time-awareness-tool-result'
export const inject = ['tools']  // 需按 capability seam 确认

const HORIZON: Record<string, string> = {
  'tool-bash': '1h',
  'tool-web': '24h',
  // 其他工具 → 默认 '1h'；文件读取由 fs 观察策略另议
}

export function apply(ctx, config) {
  ctx.on('tools/post-execute', async (call, next) => {
    const result = await next()
    if (result.kind !== 'ok') return result
    const stamp = `[data_time=${new Date().toISOString()} age=0min freshness=just_fetched horizon=${HORIZON[call.tool] ?? '1h'}]\n`
    return { ...result, content: stamp + result.content }
  })
}
```

> 注：`tools/post-execute` 的事件签名与 `call.tool` 字段名需在实现时以
> `packages/tools` 的 Service Definition 为准，避免臆测。

## 后续（非本轮必需）

- 恢复历史会话时给历史消息补每消息 age 戳（`[message_time=... age=...]`）；原生
  time-context 已提供"距上一条消息 elapsed"，可在其基础上扩展。
- 把决策链规则（已在 `time-awareness.md`）与这里的 `data_time` 注解格式对齐。

