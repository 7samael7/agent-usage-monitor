/**
 * Rendering a number together with how much it can be trusted.
 *
 * The certainty is carried on **five** independent channels, of which colour is
 * the weakest. A greyscale screenshot, a colour-blind reader and a screen
 * reader must all get the same information:
 *
 *  1. the numeral prefix — nothing, `≈`, `≥`, or an em dash
 *  2. an uppercase tag — EXACT / CALC / EST / PARTIAL / N/A
 *  3. border style — solid, dashed, dotted
 *  4. the tooltip sentence, which always explains *why*
 *  5. colour
 *
 * `<TokenCount>` accepts only a `Measured<number>`, never a bare number. That
 * is the point: there is no expression in this codebase that renders a token
 * count without its provenance, so a missing measurement cannot reach the
 * screen looking like a confident zero.
 */

import {
  type DisplayKind,
  type Measured,
  accuracyPrefix,
  accuracySentence,
  accuracyTag,
} from '@aum/api-contract'

const STYLES: Record<DisplayKind, { text: string; border: string; tag: string }> = {
  exact: { text: 'text-text', border: 'border-solid border-exact/40', tag: 'text-exact' },
  calculated: { text: 'text-text', border: 'border-solid border-calc/40', tag: 'text-calc' },
  estimated: { text: 'text-text-dim', border: 'border-dashed border-est/50', tag: 'text-est' },
  partial: {
    text: 'text-text-dim',
    border: 'border-dashed border-partial/50',
    tag: 'text-partial',
  },
  unavailable: { text: 'text-text-mute', border: 'border-dotted border-na/40', tag: 'text-na' },
}

function formatCount(value: number): string {
  return value.toLocaleString('en-US')
}

export function MeasurementTag({ kind }: { kind: DisplayKind }) {
  const style = STYLES[kind]
  return (
    <span
      className={`ml-1.5 rounded border px-1 py-px font-medium text-[9px] uppercase tracking-wider ${style.border} ${style.tag}`}
    >
      {accuracyTag(kind)}
    </span>
  )
}

/**
 * A token count.
 *
 * When the value is missing this renders an em dash and never a zero — the
 * backend distinguishes "unavailable" from "none", and undoing that here would
 * throw away the whole point of carrying it.
 */
export function TokenCount({
  measured,
  showTag = true,
}: {
  measured: Measured<number>
  showTag?: boolean
}) {
  const kind = measured.accuracy.kind
  const style = STYLES[kind]
  const sentence = accuracySentence(measured.accuracy)

  if (measured.value === null) {
    return (
      <span className={`numeric ${style.text}`} title={sentence} aria-label={sentence}>
        —{showTag && <MeasurementTag kind={kind} />}
      </span>
    )
  }

  return (
    <span className={`numeric ${style.text}`} title={sentence} aria-label={sentence}>
      {accuracyPrefix(kind)}
      {formatCount(measured.value)}
      {showTag && <MeasurementTag kind={kind} />}
    </span>
  )
}

/**
 * A monetary amount.
 *
 * `kind` is required and has no default. The three quantities this application
 * deals in are genuinely different — what you would have paid on
 * pay-as-you-go, what the agent itself reported, and what you were actually
 * charged — and collapsing them into one column labelled "cost" would be the
 * most consequential dishonesty available here. Requiring the prop means a
 * currency amount cannot reach the screen without declaring which it is.
 */
export function Money({
  measured,
  kind,
  currency,
}: {
  measured: Measured<string>
  kind: 'billed' | 'provider' | 'api-equivalent'
  currency: 'USD' | 'EUR' | 'CZK'
}) {
  const accuracyKind = measured.accuracy.kind
  const style = STYLES[accuracyKind]

  const label = {
    billed: 'what you were actually charged',
    provider: "the agent's own figure, not a charge",
    'api-equivalent': 'what this would cost on pay-as-you-go — not a charge',
  }[kind]

  const sentence = `${label}. ${accuracySentence(measured.accuracy)}`

  if (measured.value === null) {
    return (
      <span className={`numeric ${style.text}`} title={sentence} aria-label={sentence}>
        —
      </span>
    )
  }

  // Parsed only to format. The value is a decimal string precisely so that
  // arithmetic on it as a float never happens; this rounds for display and
  // keeps the exact figure in the tooltip.
  const amount = Number.parseFloat(measured.value)
  const digits = Math.abs(amount) >= 1 ? 2 : Math.abs(amount) >= 0.001 ? 4 : 6
  const formatted = Number.isFinite(amount)
    ? amount.toLocaleString('en-US', {
        style: 'currency',
        currency,
        minimumFractionDigits: digits,
        maximumFractionDigits: digits,
      })
    : measured.value

  return (
    <span
      className={`numeric ${style.text}`}
      title={`${sentence} Exact value: ${measured.value}`}
      aria-label={sentence}
    >
      {accuracyPrefix(accuracyKind)}
      {formatted}
    </span>
  )
}
