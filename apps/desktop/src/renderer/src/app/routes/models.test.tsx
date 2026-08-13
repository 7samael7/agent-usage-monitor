/**
 * The pricing screen, rendered.
 *
 * Typechecking proves the props line up; it does not prove the screen renders,
 * and every model this machine runs starts out unpriced — so this screen is the
 * only route from "cost unavailable" to a cost. If it throws, the application
 * has no pricing at all and nothing else would notice.
 */

import type { PricingView } from '@aum/api-contract'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest'
import { Models } from './models'

const conn = { baseUrl: 'http://127.0.0.1:1234', token: 'test-token' }

vi.mock('../../backend/backend-provider', () => ({
  useConnection: () => conn,
}))

function view(overrides: Partial<PricingView> = {}): PricingView {
  return {
    models: [
      {
        model_id: 'claude-opus-5',
        adapter_id: 'claude_code',
        requests: 14_358,
        total_tokens: 2_293_396_738,
        priced: false,
      },
      {
        model_id: 'claude-haiku-4-5-20251001',
        adapter_id: 'claude_code',
        requests: 598,
        total_tokens: 1_000,
        priced: true,
      },
    ],
    prices: [
      {
        version_id: 'v1',
        model_id: 'claude-haiku-4-5-20251001',
        input_per_mtok: '1.00',
        output_per_mtok: '5.00',
        cache_read_per_mtok: '0.10',
        cache_write_5m_per_mtok: '1.25',
        cache_write_1h_per_mtok: '2.00',
        effective_from: '2026-01-01T00:00:00.000Z',
        source: 'seed',
        is_current: true,
      },
    ],
    fx: [],
    supported_currencies: ['USD', 'EUR', 'CZK'],
    ...overrides,
  }
}

let posted: { url: string; body: unknown }[] = []

beforeEach(() => {
  posted = []
  vi.stubGlobal(
    'fetch',
    vi.fn((url: string, init?: RequestInit) => {
      if (init?.method === 'POST') {
        posted.push({ url, body: JSON.parse(String(init.body)) })
        return Promise.resolve(
          new Response(JSON.stringify(view().prices[0]), {
            headers: { 'content-type': 'application/json' },
          }),
        )
      }
      return Promise.resolve(
        new Response(JSON.stringify(view()), { headers: { 'content-type': 'application/json' } }),
      )
    }),
  )
})

afterEach(() => vi.unstubAllGlobals())

describe('Models & Pricing', () => {
  test('an unpriced model is shown as unpriced, with a way to fix it', async () => {
    render(<Models />)

    expect(await screen.findByText('claude-opus-5')).toBeDefined()
    expect(screen.getByText('not priced')).toBeDefined()
    // The affordance is the point. Telling someone a cost is unavailable and
    // giving them no way to supply the rate is a dead end.
    expect(screen.getByRole('button', { name: /set a price/i })).toBeDefined()
  })

  test('a priced model shows its rate rather than an invitation to add one', async () => {
    render(<Models />)
    await screen.findByText('claude-haiku-4-5-20251001')
    expect(screen.getByRole('button', { name: /correct/i })).toBeDefined()
  })

  test('the count of priced models is stated plainly', async () => {
    render(<Models />)
    expect(await screen.findByText(/1 of 2 models in use have a rate/i)).toBeDefined()
  })

  test('a blank cache rate is sent as absent, never as zero', async () => {
    // Zero would be a claim that caching is free and would understate a long
    // cached session by most of its total. Absent means "same as input", which
    // is both providers' documented default.
    const user = userEvent.setup()
    render(<Models />)

    await user.click(await screen.findByRole('button', { name: /set a price/i }))
    await user.type(await screen.findByLabelText(/input, per million tokens/i), '15.00')
    await user.type(screen.getByLabelText(/output, per million tokens/i), '75.00')
    await user.click(screen.getByRole('button', { name: /save rates/i }))

    await waitFor(() => expect(posted.length).toBe(1))
    const body = posted[0]?.body as Record<string, unknown>
    expect(body.model_id).toBe('claude-opus-5')
    expect(body.input_per_mtok).toBe('15.00')
    expect(body.cache_read_per_mtok).toBeNull()
    expect(body.cache_write_1h_per_mtok).toBeNull()
  })

  test('rates are sent as strings so no float ever touches money', async () => {
    const user = userEvent.setup()
    render(<Models />)

    await user.click(await screen.findByRole('button', { name: /set a price/i }))
    await user.type(await screen.findByLabelText(/input, per million tokens/i), '0.075')
    await user.type(screen.getByLabelText(/output, per million tokens/i), '1.234567891')
    await user.click(screen.getByRole('button', { name: /save rates/i }))

    await waitFor(() => expect(posted.length).toBe(1))
    const body = posted[0]?.body as Record<string, unknown>
    expect(typeof body.input_per_mtok).toBe('string')
    expect(body.output_per_mtok).toBe('1.234567891')
  })

  test('with no exchange rate recorded, the screen says amounts stay in USD', async () => {
    render(<Models />)
    expect(await screen.findByText(/Amounts are shown in USD until one is entered/i)).toBeDefined()
  })

  test('a stale exchange rate is shown with its age rather than silently used', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(() =>
        Promise.resolve(
          new Response(
            JSON.stringify(
              view({
                fx: [
                  {
                    quote_currency: 'EUR',
                    rate: '0.92',
                    as_of: '2026-07-01T00:00:00.000Z',
                    source: 'manual',
                    age_days: 43,
                    is_stale: true,
                    description: 'FX rate 43 days old, from 2026-07-01 (manual).',
                  },
                ],
              }),
            ),
            { headers: { 'content-type': 'application/json' } },
          ),
        ),
      ),
    )

    render(<Models />)
    expect(await screen.findByText(/43 days old/i)).toBeDefined()
  })

  test('an empty database says there is nothing to price rather than showing an empty table', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(() =>
        Promise.resolve(
          new Response(JSON.stringify(view({ models: [], prices: [] })), {
            headers: { 'content-type': 'application/json' },
          }),
        ),
      ),
    )

    render(<Models />)
    expect(await screen.findByText(/no usage recorded yet/i)).toBeDefined()
  })
})
