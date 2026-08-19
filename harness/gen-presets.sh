#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
mkdir -p "$HERE/presets"

# standard — the current full composition, verbatim.
cp "$HERE/cordis.yml" "$HERE/presets/standard.yml"

# minimal — shell + editor only, no web/goal/workflow/subagent.
cat > "$HERE/presets/minimal.yml" <<'YML'
# DeepSeek Code — minimal preset (bash + editor only).
- id: sdk-jsonrpc-server
  name: '@deepseek-ai/dsh-sdk-jsonrpc-server'
  config:
    maxTokensAsSuccess: false

- id: llm-deepseek
  name: '@deepseek-ai/dsh-llm-deepseek'
  config:
    apiKeyEnv: DEEPSEEK_API_KEY
    thinking: !!js "process.env.DSH_REASONING_EFFORT === 'off' ? 'disabled' : 'enabled'"
    reasoningEffort: !!js process.env.DSH_REASONING_EFFORT ?? 'high'
    streamIdleTimeoutMs: 90000

- id: time-context
  name: '@deepseek-ai/dsh-time-context'

- id: sandbox
  name: '@deepseek-ai/dsh-sandbox-local'

- id: sandbox-policy
  name: '@deepseek-ai/dsh-sandbox-policy'
  config:
    mode: !!js process.env.DSH_PERMISSION_MODE ?? 'workspace-write'
    workspaceRoot: !!js process.env.DSH_CWD ?? process.cwd()

- id: subprocess
  name: '@deepseek-ai/dsh-subprocess-local'

- id: bash
  name: '@deepseek-ai/dsh-bash-local'
  config:
    cwd: !!js process.env.DSH_CWD ?? process.cwd()
    timeoutMs: 60000

- id: agent-spine
  name: '@deepseek-ai/dsh-agent-spine-demo'
  config:
    persona: !!js process.env.DSH_SYSTEM_PROMPT ?? 'You are a coding agent.'
    workspaceContext: false
    skills:
      enabled: false
    toolBash:
      enableRunInBackground: false
    toolJobs: false

- id: fs-local
  name: '@deepseek-ai/dsh-fs-local'
  config:
    cwd: !!js process.env.DSH_CWD ?? process.cwd()

- id: str-replace-editor
  name: '@deepseek-ai/dsh-tool-str-replace-editor'
  config:
    maxOutputChars: 16000

- id: sessions
  name: '@deepseek-ai/dsh-session-persistence-jsonl'
  config:
    root: !!js process.env.DSH_SESSION_ROOT ?? './.sessions'
    compression: zstd

- id: token-meter
  name: '@deepseek-ai/dsh-token-meter'

- id: time-awareness-tool-result
  name: './tal-tool-result.mjs'
YML

# ptc / creative / ralph — standard plus one mode-specific plugin.
for mode in ptc creative ralph; do
  {
    cat "$HERE/cordis.yml"
    case "$mode" in
      ptc)
        printf '\n# PTC mode — programmatic tool calling (run_code).\n'
        printf -- "- id: code-runtime\n  name: '@deepseek-ai/dsh-code-runtime-worker-thread'\n"
        ;;
      creative)
        printf '\n# Creative mode — self-modification runtime is deferred; this preset is\n# standard for now (cordis-host-runner + cordis-client-runner + tool-cordis TBD).\n'
        ;;
      ralph)
        printf '\n# Ralph mode — multi-agent relay.\n'
        printf -- "- id: tool-ralph\n  name: '@deepseek-ai/dsh-tool-ralph'\n  config:\n    subagentProvider: spawn\n    maxRounds: 64\n"
        ;;
    esac
  } > "$HERE/presets/$mode.yml"
done

echo "presets generated under $HERE/presets"
