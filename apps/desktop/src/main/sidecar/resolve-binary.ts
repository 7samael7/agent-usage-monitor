import { existsSync } from 'node:fs'
import path from 'node:path'
import { app } from 'electron'

export interface BinaryResolution {
  readonly path: string
  readonly exists: boolean
  /** Shown verbatim on the failure screen when the binary is missing. */
  readonly hint: string
}

const BINARY_NAME = process.platform === 'win32' ? 'aum-sidecar.exe' : 'aum-sidecar'

/**
 * Where the sidecar binary lives.
 *
 * In development we run the **compiled binary directly**, never `cargo run`:
 * cargo writes build progress to stdout, which would be parsed as the handshake
 * line and corrupt the bootstrap. It also adds seconds of compile latency to
 * every Electron restart. `bun run sidecar:watch` rebuilds it separately.
 */
export function resolveSidecarBinary(): BinaryResolution {
  if (app.isPackaged) {
    const p = path.join(process.resourcesPath, 'sidecar', BINARY_NAME)
    return {
      path: p,
      exists: existsSync(p),
      hint: 'The packaged application is missing its backend binary. Reinstall the application.',
    }
  }

  const override = process.env.AUM_SIDECAR_BIN
  if (override) {
    return {
      path: override,
      exists: existsSync(override),
      hint: `AUM_SIDECAR_BIN points at ${override}, which does not exist.`,
    }
  }

  // apps/desktop/out/main -> repo root
  const repoRoot = path.resolve(app.getAppPath(), '..', '..')
  const debugPath = path.join(repoRoot, 'target', 'debug', BINARY_NAME)
  const releasePath = path.join(repoRoot, 'target', 'release', BINARY_NAME)
  const chosen = existsSync(debugPath) ? debugPath : releasePath

  return {
    path: chosen,
    exists: existsSync(chosen),
    hint: 'The backend has not been built yet. Run:\n\n    cargo build -p aum-sidecar',
  }
}
