/**
 * Routing.
 *
 * A hash router in forty lines rather than a routing library. There are seven
 * screens with no nesting and no loaders, and the one thing routing genuinely
 * has to provide here — a URL that can be copied, restored after a backend
 * restart, and pasted to a colleague, particularly for a comparison — is a
 * `location.hash` and a `popstate` listener. A dependency would carry code for
 * problems this app does not have.
 */

import { useEffect, useState } from 'react'

export const ROUTES = [
  { path: '/dashboard', label: 'Dashboard' },
  { path: '/live', label: 'Live Tasks' },
  { path: '/benchmarks', label: 'Benchmarks' },
  { path: '/history', label: 'History' },
  { path: '/models', label: 'Models & Pricing' },
  { path: '/applications', label: 'Applications' },
  { path: '/settings', label: 'Settings' },
] as const

export type RoutePath = (typeof ROUTES)[number]['path']

export interface Location {
  path: string
  /** Query after the path, so a comparison selection can live in the URL. */
  params: URLSearchParams
}

function read(): Location {
  const raw = window.location.hash.replace(/^#/, '') || '/dashboard'
  const [path, query] = raw.split('?')
  return {
    path: path || '/dashboard',
    params: new URLSearchParams(query ?? ''),
  }
}

export function navigate(path: string, params?: Record<string, string>): void {
  const query = params ? new URLSearchParams(params).toString() : ''
  window.location.hash = query ? `${path}?${query}` : path
}

export function useLocation(): Location {
  const [location, setLocation] = useState<Location>(read)

  useEffect(() => {
    const onChange = () => setLocation(read())
    window.addEventListener('hashchange', onChange)
    return () => window.removeEventListener('hashchange', onChange)
  }, [])

  return location
}
