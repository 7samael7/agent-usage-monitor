/**
 * Renderer test environment.
 *
 * jsdom does not implement layout, so anything that measures an element gets
 * zero — which makes a responsive chart container render nothing and a chart
 * test assert on an empty box. Rather than mock the charting library, the few
 * measurements it needs are given non-zero values here.
 */

import { cleanup } from '@testing-library/react'
import { afterEach } from 'vitest'

afterEach(cleanup)

// ResizeObserver is not in jsdom, and recharts' responsive container needs one.
if (!('ResizeObserver' in globalThis)) {
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver
}

for (const [property, value] of [
  ['offsetWidth', 800],
  ['offsetHeight', 400],
  ['clientWidth', 800],
  ['clientHeight', 400],
] as const) {
  Object.defineProperty(HTMLElement.prototype, property, {
    configurable: true,
    value,
  })
}
