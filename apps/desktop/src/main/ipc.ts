/**
 * The control plane, and only the control plane.
 *
 * Eight small channels. Usage data deliberately does **not** cross IPC: routing
 * it through here would mean a marshalling layer per DTO in the main process,
 * which would quietly become a second, drifting copy of the domain model — and
 * would serialize hundreds of structured clones per second behind menu handling
 * and window events, in an application whose whole purpose is to not perturb
 * what it measures.
 */

import fs from 'node:fs/promises'
import { type BrowserWindow, dialog, ipcMain, shell } from 'electron'
import { app } from 'electron'
import type { NetworkGuard } from './security'
import type { SidecarSupervisor } from './sidecar/supervisor'

export interface IpcDeps {
  supervisor: SidecarSupervisor
  guard: NetworkGuard
  getWindow: () => BrowserWindow | null
}

/**
 * Paths the user chose in a save dialog, awaiting their write.
 *
 * A path is consumed by the write that follows it, so one dialog authorises one
 * file.
 */
const chosenPaths = new Set<string>()

export function registerIpc({ supervisor, guard, getWindow }: IpcDeps): void {
  ipcMain.handle('backend:info', () => supervisor.info)

  ipcMain.handle('backend:restart', () => {
    supervisor.retry()
    return supervisor.info
  })

  ipcMain.handle('backend:logs', () => supervisor.stderrTail)

  ipcMain.on('backend:stream-activity', () => supervisor.noteStreamActivity())

  ipcMain.handle('app:info', () => ({
    version: app.getVersion(),
    platform: process.platform,
    arch: process.arch,
    isPackaged: app.isPackaged,
    userDataPath: app.getPath('userData'),
    blockedNetworkRequests: guard.blockedCount(),
  }))

  ipcMain.handle('native:pickDirectory', async () => {
    const window = getWindow()
    if (!window) return null
    const result = await dialog.showOpenDialog(window, {
      properties: ['openDirectory', 'createDirectory'],
      title: 'Choose a working directory',
    })
    return result.canceled ? null : (result.filePaths[0] ?? null)
  })

  ipcMain.handle(
    'native:pickSavePath',
    async (_e, args: { defaultName: string; filters?: Electron.FileFilter[] }) => {
      const window = getWindow()
      if (!window) return null
      const result = await dialog.showSaveDialog(window, {
        defaultPath: args.defaultName,
        filters: args.filters,
      })
      const chosen = result.canceled ? null : (result.filePath ?? null)
      // Remembered so a later write can prove the user picked this path.
      if (chosen) chosenPaths.add(chosen)
      return chosen
    },
  )

  /**
   * Write text the user has chosen to save.
   *
   * The renderer has no filesystem access and should not gain any just to save
   * an export, so the host writes it — but only to a path the user picked in a
   * native dialog during this session. A path the renderer invented is refused,
   * which keeps "save this export" from becoming "write anywhere".
   */
  ipcMain.handle('native:writeTextFile', async (_e, args: { path: string; contents: string }) => {
    if (!chosenPaths.has(args.path)) {
      throw new Error('refusing to write to a path the user did not choose')
    }
    chosenPaths.delete(args.path)
    await fs.writeFile(args.path, args.contents, 'utf8')
    return true
  })

  ipcMain.handle('native:reveal', (_e, args: { path: string }) => {
    // Only reveal inside directories the application owns. A path from the
    // renderer is untrusted input, and `showItemInFolder` on an arbitrary path
    // is a small but free information-disclosure primitive.
    const allowedRoots = [app.getPath('userData'), app.getPath('logs')]
    if (!allowedRoots.some((root) => args.path.startsWith(root))) return false
    shell.showItemInFolder(args.path)
    return true
  })
}
