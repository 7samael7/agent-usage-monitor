/**
 * The rules that make a number on screen trustworthy.
 *
 * These two components are the last thing between a measurement and a reader,
 * so this is where the product's central claim is either kept or quietly lost.
 * Everything below is written from the reader's side — what is on the screen,
 * what a screen reader says — rather than from the props, because the failure
 * this guards against is a correct `Measured` being *rendered* dishonestly.
 */

import type { Measured } from '@aum/api-contract'
import { render, screen } from '@testing-library/react'
import { describe, expect, test } from 'vitest'
import { Money, TokenCount } from './measurement'

function exact(value: number): Measured<number> {
  return { value, accuracy: { kind: 'exact', source: 'provider_reported' } }
}

/** Exactly the shape the backend sends when an agent reports no such field. */
function notReported<T>(field: string, detail: string): Measured<T> {
  return {
    value: null,
    accuracy: { kind: 'unavailable', reason: { kind: 'not_reported_by_provider', field, detail } },
  }
}

function calculated(value: string): Measured<string> {
  return { value, accuracy: { kind: 'calculated', source: 'application_telemetry' } }
}

function unpriced(model: string): Measured<string> {
  return {
    value: null,
    accuracy: { kind: 'unavailable', reason: { kind: 'no_pricing_for_model', model_id: model } },
  }
}

describe('TokenCount', () => {
  test('an unavailable count renders a dash and never a zero', () => {
    // The single highest-impact bug available in this product: "unavailable"
    // and "none" are different facts, and 0 is a confident claim.
    render(
      <TokenCount
        measured={notReported<number>('reasoning', 'Claude Code does not report reasoning tokens.')}
      />,
    )
    const el = screen.getByText(/—/)
    expect(el.textContent).not.toMatch(/\b0\b/)
  })

  test('a real zero renders as zero, because zero is a measurement', () => {
    render(<TokenCount measured={exact(0)} />)
    expect(screen.getByText(/\b0\b/)).toBeDefined()
  })

  test('the reason travels to a screen reader, not only to a tooltip', () => {
    // The backend writes the whole sentence, so the interface never has to
    // assemble an explanation out of enum fields and risk a worse one.
    render(
      <TokenCount
        measured={notReported<number>('reasoning', 'Claude Code does not report reasoning tokens.')}
      />,
    )
    // Colour is the weakest channel and is invisible to most of the ways this
    // gets read. The sentence has to be in the accessible name.
    expect(screen.getByLabelText(/does not report reasoning tokens/i)).toBeDefined()
  })

  test('large counts are grouped so they can be read at a glance', () => {
    render(<TokenCount measured={exact(2_293_396_738)} showTag={false} />)
    expect(screen.getByText(/2,293,396,738/)).toBeDefined()
  })
})

describe('Money', () => {
  test('an unavailable amount renders a dash rather than a zero amount', () => {
    // "$0.00" would read as "this was free", which is a different claim from
    // "we cannot price this model".
    render(<Money measured={unpriced('claude-opus-5')} kind="api-equivalent" currency="USD" />)
    const el = screen.getByText('—')
    expect(el.textContent).not.toContain('$')
  })

  test('an API-equivalent amount says it is not a charge', () => {
    // The most consequential dishonesty available here is presenting a
    // pay-as-you-go equivalent as money someone was billed.
    render(<Money measured={calculated('1.0885575')} kind="api-equivalent" currency="USD" />)
    expect(screen.getByLabelText(/not a charge/i)).toBeDefined()
  })

  test('a billed amount is labelled as what was actually charged', () => {
    render(<Money measured={calculated('0.48')} kind="billed" currency="USD" />)
    expect(screen.getByLabelText(/actually charged/i)).toBeDefined()
  })

  test('the exact decimal survives in the title even though the display rounds', () => {
    // The wire value is a decimal string so no float arithmetic touches it.
    // Display rounds; the precise figure must remain recoverable.
    render(<Money measured={calculated('1.0885575')} kind="api-equivalent" currency="USD" />)
    const el = screen.getByLabelText(/not a charge/i)
    expect(el.getAttribute('title')).toContain('1.0885575')
  })

  test('a small amount keeps enough digits to not round away to nothing', () => {
    // Per-request costs are frequently a fraction of a cent. Two decimal places
    // would render every one of them as $0.00.
    render(<Money measured={calculated('0.004125')} kind="api-equivalent" currency="USD" />)
    expect(screen.getByText(/0\.0041/)).toBeDefined()
  })

  test('the currency shown is the currency asked for', () => {
    render(<Money measured={calculated('82.80')} kind="api-equivalent" currency="EUR" />)
    const el = screen.getByLabelText(/not a charge/i)
    expect(el.textContent).toMatch(/€|EUR/)
  })
})
