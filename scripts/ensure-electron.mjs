#!/usr/bin/env node
/**
 * Make sure Electron's binary is actually downloaded.
 *
 * bun blocks dependency lifecycle scripts by default. `trustedDependencies`
 * is supposed to re-enable them, but it does not reliably reach a devDependency
 * of a workspace package — so electron's postinstall never runs, no binary is
 * fetched, and `electron-vite dev` fails with the deeply unhelpful
 * "Error: Electron uninstall".
 *
 * Rather than leave that as a manual step every contributor rediscovers, we
 * check for the binary after install and run electron's own installer if it is
 * missing. Idempotent, and a no-op once the binary is present.
 */

import { existsSync } from 'node:fs'
import { createRequire } from 'node:module'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const require = createRequire(path.join(repoRoot, 'apps/desktop/package.json'))

let electronDir
try {
  electronDir = path.dirname(require.resolve('electron/package.json'))
} catch {
  // Electron is not installed at all (e.g. a CI job that only builds Rust).
  process.exit(0)
}

if (existsSync(path.join(electronDir, 'path.txt')) && existsSync(path.join(electronDir, 'dist'))) {
  process.exit(0)
}

console.log('[ensure-electron] binary missing; running electron install…')
const { spawnSync } = await import('node:child_process')
const result = spawnSync(process.execPath, ['install.js'], {
  cwd: electronDir,
  stdio: 'inherit',
})

if (result.status !== 0) {
  console.error('[ensure-electron] failed. Run it by hand:')
  console.error(`    cd ${electronDir} && node install.js`)
  process.exit(1)
}
console.log('[ensure-electron] done.')
