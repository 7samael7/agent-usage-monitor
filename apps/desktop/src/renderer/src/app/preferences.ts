/**
 * Preferences that belong to the person, not to the data.
 *
 * Kept out of the URL deliberately. A link to a comparison should carry which
 * tasks and which metrics, because those are what the link is *about*; carrying
 * the reader's currency as well would mean a shared link silently reformats
 * someone else's screen, and would give the setting two sources of truth that
 * disagree for one render on every restore.
 *
 * Stored locally in the renderer. It never reaches the sidecar except as the
 * `currency` query parameter on a request, because the conversion itself has to
 * happen in decimal arithmetic on the backend.
 */

import { useSyncExternalStore } from 'react'

const KEY = 'aum.currency'

/** Currencies the interface knows how to ask for. */
export const CURRENCIES = ['USD', 'EUR', 'CZK'] as const
export type CurrencyCode = (typeof CURRENCIES)[number]

function isCurrency(value: string | null): value is CurrencyCode {
  return value !== null && (CURRENCIES as readonly string[]).includes(value)
}

let current: CurrencyCode = 'USD'
try {
  const stored = localStorage.getItem(KEY)
  if (isCurrency(stored)) current = stored
} catch {
  // A blocked or unavailable localStorage is not worth failing over. USD is
  // the base currency and needs no rate, so the fallback is always usable.
}

const listeners = new Set<() => void>()

function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

export function setCurrency(next: CurrencyCode): void {
  if (next === current) return
  current = next
  try {
    localStorage.setItem(KEY, next)
  } catch {
    // The choice still applies for this session.
  }
  for (const listener of listeners) listener()
}

export function getCurrency(): CurrencyCode {
  return current
}

/** The display currency, re-rendering whatever reads it when it changes. */
export function useCurrency(): CurrencyCode {
  return useSyncExternalStore(subscribe, getCurrency, getCurrency)
}
