/** Mirrors `aum_contract::measurement`. */

export type MeasurementSource =
  | 'provider_reported'
  | 'protocol_metadata'
  | 'application_telemetry'
  | 'tokenizer_calculated'
  | 'estimated'

export type DisplayKind = 'exact' | 'calculated' | 'estimated' | 'partial' | 'unavailable'

export type UnavailableReason =
  | { kind: 'no_telemetry'; detail: string }
  | { kind: 'not_reported_by_provider'; field: string; detail: string }
  | { kind: 'request_failed'; detail: string }
  | { kind: 'no_pricing_for_model'; model_id: string }
  | { kind: 'model_unknown'; detail: string }
  | { kind: 'subscription_billed'; plan: string }
  | { kind: 'requires_capture_level'; level: string; detail: string }

export type Accuracy =
  | { kind: 'exact'; source: MeasurementSource }
  | { kind: 'calculated'; source: MeasurementSource }
  | { kind: 'estimated'; source: MeasurementSource }
  | { kind: 'partial'; measured: number; total: number; reason: string }
  | { kind: 'unavailable'; reason: UnavailableReason }

/**
 * A value and how certain it is.
 *
 * `value === null` exactly when `accuracy.kind === 'unavailable'`. Display
 * components accept this type and never a bare number, so there is no path by
 * which a missing measurement reaches the screen as a confident figure.
 */
export interface Measured<T> {
  value: T | null
  accuracy: Accuracy
}

export function displayKind(m: Measured<unknown>): DisplayKind {
  return m.accuracy.kind
}

export function isAvailable<T>(m: Measured<T>): m is Measured<T> & { value: T } {
  return m.value !== null && m.accuracy.kind !== 'unavailable'
}

/** The prefix that warns the reader what kind of number they are looking at. */
export function accuracyPrefix(kind: DisplayKind): string {
  switch (kind) {
    case 'exact':
      return ''
    case 'calculated':
    case 'estimated':
      return '≈'
    case 'partial':
      return '≥'
    case 'unavailable':
      return ''
  }
}

/** The short uppercase tag rendered beside a number. */
export function accuracyTag(kind: DisplayKind): string {
  switch (kind) {
    case 'exact':
      return 'EXACT'
    case 'calculated':
      return 'CALC'
    case 'estimated':
      return 'EST'
    case 'partial':
      return 'PARTIAL'
    case 'unavailable':
      return 'N/A'
  }
}

/** One sentence explaining the certainty, for a tooltip or `aria-label`. */
export function accuracySentence(accuracy: Accuracy): string {
  switch (accuracy.kind) {
    case 'exact':
      return 'Exact — reported by the provider itself.'
    case 'calculated':
      return 'Calculated — derived locally from data the monitor holds, not reported by the provider.'
    case 'estimated':
      return 'Estimated — an approximation, not a measurement.'
    case 'partial':
      // The backend's reason is authoritative: the counts alone can mislead, as
      // a task observed from halfway through has measured every request it saw.
      return `Partial — ${accuracy.reason}. This is a lower bound.`
    case 'unavailable':
      return unavailableSentence(accuracy.reason)
  }
}

export function unavailableSentence(reason: UnavailableReason): string {
  switch (reason.kind) {
    case 'no_telemetry':
    case 'request_failed':
    case 'model_unknown':
      return reason.detail
    case 'not_reported_by_provider':
      return reason.detail
    case 'no_pricing_for_model':
      return `No price is configured for model ${reason.model_id}.`
    case 'subscription_billed':
      return `Billed under the ${reason.plan} subscription, not per token.`
    case 'requires_capture_level':
      return `${reason.detail} Enable ${reason.level} to measure this.`
  }
}
