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
