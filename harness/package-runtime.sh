#!/usr/bin/env bash
set -euo pipefail

# Build a self-contained Harness runtime closure (the "node carrier") and drop
# our cordis.yml + TAL plugin beside it. Output runs with:
#   node <out>/node_modules/@deepseek-ai/dsh-sdk-jsonrpc-demo/lib/packaged-bin.js <out>/cordis.yml

HARNESS_REPO="${HARNESS_REPO:-/tmp/dsh-harness-upstream}"
OUT="${1:-harness-runtime}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

cd "$HARNESS_REPO"

# 1. The official closure manifest (dsh-jsonrpc-agent-pkg) already ships every
#    plugin we use except dsh-time-context (our TAL-L0 live clock).
node -e '
const fs = require("fs");
const p = "python/sdk-runtime/package.json";
const m = JSON.parse(fs.readFileSync(p, "utf8"));
m.dependencies = m.dependencies || {};
if (!m.dependencies["@deepseek-ai/dsh-time-context"]) {
  m.dependencies["@deepseek-ai/dsh-time-context"] = "workspace:^";
}
if (!m.dependencies["@deepseek-ai/dsh-tool-ralph"]) {
  m.dependencies["@deepseek-ai/dsh-tool-ralph"] = "workspace:^";
}
if (!m.dependencies["@deepseek-ai/dsh-tool-cordis"]) {
  m.dependencies["@deepseek-ai/dsh-tool-cordis"] = "workspace:^";
}
fs.writeFileSync(p, JSON.stringify(m, null, 2) + "\n");
'

# 2. Deploy the closed runtime tree (authoritative flags from the official
#    single-exe builder).
rm -rf "$OUT"
pnpm --filter dsh-jsonrpc-agent-pkg deploy \
  --legacy \
  --prod \
  --config.node-linker=hoisted \
  --config.auto-install-peers=false \
  --config.link-workspace-packages=true \
  "$OUT"

# 3. Restore legacy-deploy hoists + materialize symlinks into real files so the
#    tree is self-contained (pnpm workspace links must not leak out).
node "$HERE/materialize-runtime.mjs" "$OUT" "$HARNESS_REPO/python/sdk-runtime/node_modules"

# 4. Drop our composition and zero-dependency TAL plugin into the closure.
cp "$HERE/cordis.yml" "$OUT/cordis.yml"
cp "$HERE/tal-tool-result.mjs" "$OUT/tal-tool-result.mjs"

echo "runtime closure -> $OUT"
echo "entry: $OUT/node_modules/@deepseek-ai/dsh-sdk-jsonrpc-demo/lib/packaged-bin.js"
