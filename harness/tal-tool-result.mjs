// TAL L1 — annotate accepted tool results with production time + freshness
// horizon. Zero-dependency ESM so plain Node can load it from any config dir,
// including a packaged closed runtime.

export const name = 'time-awareness-tool-result'
export const inject = ['tools']

const DEFAULT_HORIZON = {
  bash: '1h',
  'bash-persistent': '1h',
  pwsh: '1h',
  web: '24h',
  search: '24h',
  read: 'file',
  write: 'file',
  edit: 'file',
}

function horizonFor(tool) {
  return DEFAULT_HORIZON[tool] ?? '1h'
}

export function apply(ctx) {
  ctx.on(
    'tools/post-execute',
    async (_exec, _result, next) => {
      const decision = await next()
      if (
        decision.kind !== 'accept'
        || Object.hasOwn(decision, 'value')
        || _exec.parent !== undefined
      ) {
        return decision
      }
      const content = decision.content ?? _result.content
      const stamp = `[data_time=${new Date().toISOString()} age=0min freshness=just_fetched horizon=${horizonFor(_exec.name)}]`
      const annotated = [{ type: 'text', text: `${stamp}\n` }, ...content]
      return {
        kind: 'accept',
        content: annotated,
        ...(decision.additionalContexts
          ? { additionalContexts: decision.additionalContexts }
          : {}),
      }
    },
    { prepend: true },
  )
}
