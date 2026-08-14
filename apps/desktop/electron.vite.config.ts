import { resolve } from 'node:path'
import { defineConfig, externalizeDepsPlugin } from 'electron-vite'
import rendererConfig from './vite.renderer.config'

export default defineConfig({
  main: {
    plugins: [externalizeDepsPlugin()],
    build: {
      rollupOptions: {
        input: { index: resolve(import.meta.dirname, 'src/main/index.ts') },
        // `electron` MUST stay external. `externalizeDepsPlugin` only
        // externalizes `dependencies`, and electron is correctly a
        // devDependency (it must not ship inside the asar). Without this it
        // gets inlined — and the inlined module is npm's path-lookup shim, not
        // the runtime API, so `import { app } from 'electron'` silently becomes
        // the wrong thing and the app dies at load with a misleading
        // "Electron failed to install correctly".
        external: ['electron'],
      },
    },
  },
  preload: {
    plugins: [externalizeDepsPlugin()],
    build: {
      rollupOptions: {
        input: { index: resolve(import.meta.dirname, 'src/preload/index.ts') },
        external: ['electron'],
        // **A sandboxed preload script cannot be an ES module.** This package is
        // `"type": "module"`, so a `.js` output is ESM, and Electron's sandboxed
        // preload loader silently declines to run it: no error, no preload, and
        // `window.monitor` simply never exists. The renderer then falls back to
        // its browser-only mode and reports that the backend did not start —
        // while the backend is in fact running perfectly.
        //
        // CommonJS with an explicit `.cjs` extension so Node's module detection
        // cannot be fooled by the package type either way.
        output: {
          format: 'cjs',
          entryFileNames: '[name].cjs',
        },
      },
    },
  },
  // Shared with `bun run dev:web`, so the browser-only mode cannot drift from
  // what Electron actually builds.
  renderer: rendererConfig,
})
