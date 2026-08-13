/**
 * Test configuration.
 *
 * Two environments in one run, chosen by path: the main-process code is plain
 * Node, and the renderer needs a DOM. Splitting them keeps a jsdom global off
 * the supervisor tests, which are about a state machine and should not depend
 * on a browser being simulated.
 */

import react from '@vitejs/plugin-react'
import { defineConfig } from 'vitest/config'

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      '@aum/api-contract': new URL('../../packages/api-contract/src/index.ts', import.meta.url)
        .pathname,
    },
  },
  test: {
    globals: false,
    projects: [
      {
        extends: true,
        test: {
          name: 'main',
          environment: 'node',
          include: ['src/main/**/*.test.ts'],
        },
      },
      {
        extends: true,
        test: {
          name: 'renderer',
          environment: 'jsdom',
          include: ['src/renderer/**/*.test.tsx'],
          setupFiles: ['./src/renderer/test-setup.ts'],
        },
      },
    ],
  },
})
