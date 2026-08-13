/**
 * Charts.
 *
 * Three rules, all of which exist to stop a chart being more confident than the
 * data behind it:
 *
 * **The bands are stacked because they are disjoint.** `inputFresh`,
 * `cacheRead`, `cacheWrite`, `output` and `unclassified` never overlap, for
 * either provider. Stacking a provider's raw fields instead would double-count
 * Codex's cached input and reasoning tokens, producing a plausible chart about
 * 20% too tall for one agent only — a bug that looks like nothing at all.
 *
 * **Gaps stay gaps.** `connectNulls` is off, so a period with no measurement
 * shows as a break rather than a straight line implying steady usage.
 *
 * **No smoothing, no animation.** `monotone` interpolation invents plausible
 * curves between real token spikes, and re-animating on every data change makes
 * a live chart unreadable.
 */

import type { SeriesPoint } from '@aum/api-contract'
import {
  Area,
  AreaChart,
  CartesianGrid,
  Legend,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts'

const BANDS = [
  { key: 'input_fresh', label: 'input', colour: 'var(--color-accent)' },
  { key: 'cache_read', label: 'cache read', colour: 'var(--color-info)' },
  { key: 'cache_write', label: 'cache write', colour: 'var(--color-calc)' },
  { key: 'output_total', label: 'output', colour: 'var(--color-exact)' },
  // Codex compaction calls: real tokens the provider did not classify. Shown in
  // the muted "unavailable" colour, because they are counted but unpriceable.
  { key: 'unclassified', label: 'unclassified', colour: 'var(--color-na)' },
] as const

function shortTime(at: string): string {
  return at.slice(11, 16)
}

export function TokensOverTime({ series }: { series: SeriesPoint[] }) {
  if (series.length === 0) {
    return <p className="text-text-mute">Nothing measured yet, so there is nothing to plot.</p>
  }

  return (
    <div className="h-56 w-full">
      <ResponsiveContainer width="100%" height="100%">
        <AreaChart data={series} margin={{ top: 8, right: 8, bottom: 0, left: 8 }}>
          <CartesianGrid stroke="var(--color-border)" vertical={false} />
          <XAxis
            dataKey="at"
            tickFormatter={(at) => shortTime(String(at))}
            stroke="var(--color-text-faint)"
            tick={{ fontSize: 10 }}
            tickLine={false}
          />
          <YAxis
            stroke="var(--color-text-faint)"
            tick={{ fontSize: 10 }}
            tickLine={false}
            width={56}
            tickFormatter={(v) => (typeof v === 'number' ? v.toLocaleString('en-US') : '')}
          />
          <Tooltip
            contentStyle={{
              background: 'var(--color-surface-2)',
              border: '1px solid var(--color-border-strong)',
              borderRadius: 6,
              fontSize: 11,
            }}
            labelFormatter={(at) => String(at).replace('T', ' ').replace('Z', '')}
            formatter={(value, name) => [
              typeof value === 'number' ? value.toLocaleString('en-US') : String(value ?? '—'),
              String(name),
            ]}
          />
          <Legend wrapperStyle={{ fontSize: 10 }} />
          {BANDS.map((band) => (
            <Area
              key={band.key}
              type="linear"
              dataKey={band.key}
              name={band.label}
              stackId="tokens"
              stroke={band.colour}
              fill={band.colour}
              fillOpacity={0.25}
              // A measurement gap is a gap, not a straight line through it.
              connectNulls={false}
              isAnimationActive={false}
              dot={false}
            />
          ))}
        </AreaChart>
      </ResponsiveContainer>
    </div>
  )
}
