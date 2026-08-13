import { resolve } from 'node:path'
import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

/**
 * The renderer as a plain Vite app.
 *
 * Imported by `electron.vite.config.ts` for the Electron build, and usable
 * directly (`bun run dev:web`) to serve the UI in an ordinary browser against a
 * manually started sidecar.
 *
 * That browser mode is not a convenience. It is the standing proof that no
 * business logic leaked into Electron and that the backend boundary really is
 * just HTTP — if the UI runs unmodified against a sidecar it did not spawn, it
 * will run against a Go or C# one too.
 */
export default defineConfig({
  root: resolve(import.meta.dirname, 'src/renderer'),
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      '@': resolve(import.meta.dirname, 'src/renderer/src'),
      '@aum/api-contract': resolve(import.meta.dirname, '../../packages/api-contract/src/index.ts'),
    },
  },
  server: { port: 5273, strictPort: true },
  build: {
    outDir: resolve(import.meta.dirname, 'out/renderer'),
    emptyOutDir: true,
    rollupOptions: {
      input: { index: resolve(import.meta.dirname, 'src/renderer/index.html') },
    },
  },
})
