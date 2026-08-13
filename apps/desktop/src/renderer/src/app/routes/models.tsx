/**
 * Models and pricing.
 *
 * Every model this machine runs — `claude-opus-5`, `claude-fable-5`,
 * `gpt-5.6-sol` — is newer than any published price list, so out of the box
 * they are all uncosted. That is the correct starting state: pricing one of
 * them at a similar model's rate would produce a confident total wrong by an
 * unknown factor. This screen is how the user turns "no price for this model"
 * from something they are told into something they can answer.
 *
 * Prices are append-only. Correcting one adds a version rather than editing a
 * row, so a benchmark run in March still shows March's numbers in August, and
 * the history below makes a changed price visible as a change rather than as a
 * number that quietly differs from last week's.
 */

import type { FxRow, ObservedModel, PriceRow, PricingView } from '@aum/api-contract'
import { useCallback, useEffect, useId, useState } from 'react'
import { useConnection } from '../../backend/backend-provider'
import { fetchPricing, saveFxRate, savePrice } from '../../backend/client'
import { Button, Card, Empty, Field, Row, Screen, Table, Td, Th, inputClass } from '../../viz/ui'
import { CURRENCIES, useCurrency } from '../preferences'

export function Models() {
  const conn = useConnection()
  const [view, setView] = useState<PricingView | null>(null)
  const [editing, setEditing] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const currency = useCurrency()

  // One loader, called on mount and again after every edit, so the screen after
  // a save is the screen the backend would serve on a fresh visit rather than a
  // local guess at what the save did.
  const reload = useCallback(
    (signal?: AbortSignal) => {
      if (!conn) return
      fetchPricing(conn, signal)
        .then(setView)
        .catch((e: unknown) => {
          if (!signal?.aborted) setError(String(e))
        })
    },
    [conn],
  )

  useEffect(() => {
    const ac = new AbortController()
    reload(ac.signal)
    return () => ac.abort()
  }, [reload])

  const priced = view?.models.filter((m) => m.priced).length ?? 0
  const total = view?.models.length ?? 0

  return (
    <Screen
      title="Models & Pricing"
      subtitle="Models seen on this machine. A model with no published rate is shown as unpriced rather than costed at a similar model's rate."
    >
      {error && <p className="mb-4 text-neg">{error}</p>}
      {!view && !error && <Empty>Loading…</Empty>}

      {view && view.models.length === 0 && (
        <Empty>No usage recorded yet, so there are no models to price.</Empty>
      )}

      {view && view.models.length > 0 && (
        <>
          <p className="mb-3 text-[11px] text-text-mute">
            {priced} of {total} models in use have a rate.
            {priced < total && ' Costs for the rest are shown as unavailable, not as zero.'}
          </p>

          <Table
            head={
              <>
                <Th>model</Th>
                <Th>agent</Th>
                <Th align="right">requests</Th>
                <Th align="right">tokens</Th>
                <Th>rate</Th>
                <Th />
              </>
            }
          >
            {view.models.map((m) => (
              <ModelRow
                key={m.model_id}
                model={m}
                price={view.prices.find((p) => p.model_id === m.model_id && p.is_current)}
                onEdit={() => setEditing(editing === m.model_id ? null : m.model_id)}
                editing={editing === m.model_id}
              />
            ))}
          </Table>

          {editing && conn && (
            <PriceForm
              modelId={editing}
              existing={view.prices.find((p) => p.model_id === editing && p.is_current)}
              onSaved={() => {
                setEditing(null)
                reload()
              }}
              onCancel={() => setEditing(null)}
              onError={setError}
            />
          )}

          <History rows={view.prices} />
        </>
      )}

      {view && <FxRates rows={view.fx} selected={currency} onSaved={reload} onError={setError} />}
    </Screen>
  )
}

function ModelRow({
  model,
  price,
  editing,
  onEdit,
}: {
  model: ObservedModel
  price: PriceRow | undefined
  editing: boolean
  onEdit: () => void
}) {
  return (
    <Row>
      <Td>{model.model_id}</Td>
      <Td>{model.adapter_id}</Td>
      <Td align="right" numeric>
        {model.requests.toLocaleString('en-US')}
      </Td>
      <Td align="right" numeric>
        {model.total_tokens.toLocaleString('en-US')}
      </Td>
      <Td>
        {price ? (
          <span className="numeric font-mono text-[11px] text-text-dim">
            ${price.input_per_mtok} in / ${price.output_per_mtok} out
            {price.source === 'user' && <span className="ml-1 text-text-mute">(yours)</span>}
          </span>
        ) : (
          <span className="text-warn" title="No published rate exists for this model.">
            not priced
          </span>
        )}
      </Td>
      <Td>
        <Button onClick={onEdit}>{editing ? 'Cancel' : price ? 'Correct' : 'Set a price'}</Button>
      </Td>
    </Row>
  )
}

/**
 * The five rates, per million tokens.
 *
 * Cache writes are asked for separately because the multipliers genuinely
 * differ — roughly 1.25x input for five minutes against 2x for an hour — and
 * real Claude sessions are dominated by the hour tier. Collapsing them into one
 * figure understates those sessions by about a quarter.
 */
function PriceForm({
  modelId,
  existing,
  onSaved,
  onCancel,
  onError,
}: {
  modelId: string
  existing: PriceRow | undefined
  onSaved: () => void
  onCancel: () => void
  onError: (message: string) => void
}) {
  const conn = useConnection()
  const id = useId()
  const [input, setInput] = useState(existing?.input_per_mtok ?? '')
  const [output, setOutput] = useState(existing?.output_per_mtok ?? '')
  const [cacheRead, setCacheRead] = useState(existing?.cache_read_per_mtok ?? '')
  const [write5m, setWrite5m] = useState(existing?.cache_write_5m_per_mtok ?? '')
  const [write1h, setWrite1h] = useState(existing?.cache_write_1h_per_mtok ?? '')
  const [note, setNote] = useState('')
  const [saving, setSaving] = useState(false)

  const submit = (e: React.FormEvent) => {
    e.preventDefault()
    if (!conn) return
    setSaving(true)
    savePrice(conn, {
      model_id: modelId,
      input_per_mtok: input.trim(),
      output_per_mtok: output.trim(),
      // Left blank means "charged at the input rate", which is both providers'
      // documented default. Sending 0 would claim caching is free.
      cache_read_per_mtok: cacheRead.trim() || null,
      cache_write_5m_per_mtok: write5m.trim() || null,
      cache_write_1h_per_mtok: write1h.trim() || null,
      note: note.trim() || null,
    })
      .then(onSaved)
      .catch((err: unknown) => onError(String(err)))
      .finally(() => setSaving(false))
  }

  return (
    <Card title={`Rates for ${modelId}`} className="mt-4 max-w-3xl">
      <form onSubmit={submit} className="flex flex-col gap-3">
        <div className="grid grid-cols-2 gap-3">
          <Field label="input, per million tokens" htmlFor={`${id}-in`}>
            <input
              id={`${id}-in`}
              className={inputClass}
              value={input}
              onChange={(e) => setInput(e.target.value)}
              placeholder="15.00"
              inputMode="decimal"
              required
            />
          </Field>
          <Field label="output, per million tokens" htmlFor={`${id}-out`}>
            <input
              id={`${id}-out`}
              className={inputClass}
              value={output}
              onChange={(e) => setOutput(e.target.value)}
              placeholder="75.00"
              inputMode="decimal"
              required
            />
          </Field>
        </div>

        <div className="grid grid-cols-3 gap-3">
          <Field
            label="cache read"
            htmlFor={`${id}-cr`}
            hint="Blank means charged at the input rate."
          >
            <input
              id={`${id}-cr`}
              className={inputClass}
              value={cacheRead}
              onChange={(e) => setCacheRead(e.target.value)}
              placeholder="1.50"
              inputMode="decimal"
            />
          </Field>
          <Field label="cache write, 5 min" htmlFor={`${id}-w5`}>
            <input
              id={`${id}-w5`}
              className={inputClass}
              value={write5m}
              onChange={(e) => setWrite5m(e.target.value)}
              placeholder="18.75"
              inputMode="decimal"
            />
          </Field>
          <Field
            label="cache write, 1 hour"
            htmlFor={`${id}-w1`}
            hint="Usually about twice input, and what real sessions mostly use."
          >
            <input
              id={`${id}-w1`}
              className={inputClass}
              value={write1h}
              onChange={(e) => setWrite1h(e.target.value)}
              placeholder="30.00"
              inputMode="decimal"
            />
          </Field>
        </div>

        <Field label="note" htmlFor={`${id}-note`} hint="Where this figure came from.">
          <input
            id={`${id}-note`}
            className={inputClass}
            value={note}
            onChange={(e) => setNote(e.target.value)}
            placeholder="anthropic.com/pricing, checked today"
          />
        </Field>

        <p className="text-[10px] text-text-mute leading-relaxed">
          Rates are in USD, which is what providers publish. Saving adds a version rather than
          replacing one: everything already recorded keeps the rate it was costed with, and only
          usage from now on uses this figure.
        </p>

        <div className="flex gap-2">
          <Button type="submit" variant="primary" disabled={saving}>
            {saving ? 'Saving…' : 'Save rates'}
          </Button>
          <Button onClick={onCancel}>Cancel</Button>
        </div>
      </form>
    </Card>
  )
}

function History({ rows }: { rows: PriceRow[] }) {
  const superseded = rows.filter((r) => !r.is_current)
  if (superseded.length === 0) return null

  return (
    <Card title="Superseded rates" className="mt-4 max-w-3xl">
      <p className="mb-2 text-[11px] text-text-mute leading-relaxed">
        Kept, not deleted. Usage costed under one of these still shows what it cost then.
      </p>
      <Table
        head={
          <>
            <Th>model</Th>
            <Th align="right">input</Th>
            <Th align="right">output</Th>
            <Th>effective from</Th>
            <Th>source</Th>
          </>
        }
      >
        {superseded.map((r) => (
          <Row key={r.version_id}>
            <Td>{r.model_id}</Td>
            <Td align="right" numeric>
              ${r.input_per_mtok}
            </Td>
            <Td align="right" numeric>
              ${r.output_per_mtok}
            </Td>
            <Td>{r.effective_from.slice(0, 10)}</Td>
            <Td>{r.source}</Td>
          </Row>
        ))}
      </Table>
    </Card>
  )
}

/**
 * Exchange rates, typed in rather than fetched.
 *
 * Fetching one would mean this application making an outbound request, and the
 * privacy claim in Settings is worth more than the convenience — particularly
 * for a number that moves a fraction of a percent a day and is applied to an
 * already-approximate cost.
 */
function FxRates({
  rows,
  selected,
  onSaved,
  onError,
}: {
  rows: FxRow[]
  selected: string
  onSaved: () => void
  onError: (message: string) => void
}) {
  const conn = useConnection()
  const id = useId()
  const [quote, setQuote] = useState<string>(selected === 'USD' ? 'EUR' : selected)
  const [rate, setRate] = useState('')
  const [saving, setSaving] = useState(false)

  const submit = (e: React.FormEvent) => {
    e.preventDefault()
    if (!conn) return
    setSaving(true)
    saveFxRate(conn, { quote_currency: quote, rate: rate.trim() })
      .then(() => {
        setRate('')
        onSaved()
      })
      .catch((err: unknown) => onError(String(err)))
      .finally(() => setSaving(false))
  }

  return (
    <Card title="Exchange rates" className="mt-4 max-w-3xl">
      {rows.length === 0 ? (
        <p className="mb-3 text-text-dim">
          No rates recorded. Amounts are shown in USD until one is entered.
        </p>
      ) : (
        <ul className="mb-3 flex flex-col gap-1">
          {rows.map((r) => (
            <li key={r.quote_currency} className="text-text-dim">
              <span className="numeric font-mono">
                1 USD = {r.rate} {r.quote_currency}
              </span>
              <span className={`ml-2 text-[11px] ${r.is_stale ? 'text-warn' : 'text-text-mute'}`}>
                {r.description}
              </span>
            </li>
          ))}
        </ul>
      )}

      <form onSubmit={submit} className="flex items-end gap-3">
        <Field label="currency" htmlFor={`${id}-cur`}>
          <select
            id={`${id}-cur`}
            className={inputClass}
            value={quote}
            onChange={(e) => setQuote(e.target.value)}
          >
            {CURRENCIES.filter((c) => c !== 'USD').map((c) => (
              <option key={c} value={c}>
                {c}
              </option>
            ))}
          </select>
        </Field>
        <Field label={`${quote} per 1 USD`} htmlFor={`${id}-rate`}>
          <input
            id={`${id}-rate`}
            className={inputClass}
            value={rate}
            onChange={(e) => setRate(e.target.value)}
            placeholder="0.92"
            inputMode="decimal"
            required
          />
        </Field>
        <Button type="submit" disabled={saving}>
          {saving ? 'Saving…' : 'Record rate'}
        </Button>
      </form>

      <p className="mt-3 text-[10px] text-text-mute leading-relaxed">
        Rates are entered, never fetched — this application makes no outbound requests. A converted
        amount is marked calculated rather than exact, and one converted with a rate more than a
        week old is downgraded further and says how old it is.
      </p>
    </Card>
  )
}
