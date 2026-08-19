// Post-process a `pnpm deploy` closure into a self-contained runtime tree,
// mirroring the two steps from scripts/build-exe-for-python-sdk.ts:
//   restoreLegacyHoists   — copy direct workspace deps that legacy deploy
//                           hoisted beside the deploy source
//   materializeStagedLinks — replace every symlink with real bytes and drop
//                           package-manager .bin links

import { existsSync } from 'node:fs'
import { cp, lstat, mkdir, readdir, readFile, realpath, rm } from 'node:fs/promises'
import { dirname, join, sep } from 'node:path'

const [staging, sourceNodeModules] = process.argv.slice(2)
if (!staging || !sourceNodeModules) {
  console.error('usage: materialize-runtime.mjs <staging-dir> <deploy-source-node_modules>')
  process.exit(2)
}

async function restoreLegacyHoists() {
  const manifest = JSON.parse(await readFile(join(staging, 'package.json'), 'utf8'))
  const deps = Object.keys(manifest.dependencies ?? {}).sort()
  const restored = []
  for (const dep of deps) {
    const dest = join(staging, 'node_modules', dep)
    if (existsSync(dest)) continue
    const src = join(sourceNodeModules, dep)
    if (!existsSync(src)) {
      throw new Error(`deploy dependency ${dep} is absent from ${src}`)
    }
    await mkdir(dirname(dest), { recursive: true })
    const nested = join(src, 'node_modules')
    await cp(src, dest, {
      recursive: true,
      dereference: true,
      filter: (p) => p !== nested && !p.startsWith(nested + sep),
    })
    restored.push(dep)
  }
  const missing = Object.keys(manifest.dependencies ?? {}).filter(
    (d) => !existsSync(join(staging, 'node_modules', d)),
  )
  if (missing.length > 0) {
    throw new Error(`staged dependencies remain missing: ${missing.join(', ')}`)
  }
  if (restored.length > 0) {
    console.log('restored legacy hoists:', restored.join(', '))
  }
}

async function findSymlink(dir) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const p = join(dir, entry.name)
    const m = await lstat(p)
    if (m.isSymbolicLink()) return p
    if (m.isDirectory()) {
      const nested = await findSymlink(p)
      if (nested !== undefined) return nested
    }
  }
  return undefined
}

async function materializeStagedLinks() {
  const nodeModules = join(staging, 'node_modules')
  let remaining = await findSymlink(nodeModules)
  while (remaining !== undefined) {
    const segments = remaining.slice(nodeModules.length + 1).split(sep)
    const binIndex = segments.lastIndexOf('.bin')
    if (binIndex >= 0) {
      await rm(join(nodeModules, ...segments.slice(0, binIndex + 1)), {
        recursive: true,
        force: true,
      })
      remaining = await findSymlink(nodeModules)
      continue
    }
    const source = await realpath(remaining)
    const nested = join(source, 'node_modules')
    await rm(remaining, { recursive: true, force: true })
    await cp(source, remaining, {
      recursive: true,
      dereference: true,
      filter: (p) => p !== nested && !p.startsWith(nested + sep),
    })
    remaining = await findSymlink(nodeModules)
  }
}

await restoreLegacyHoists()
await materializeStagedLinks()
console.log('runtime closure materialized:', staging)
