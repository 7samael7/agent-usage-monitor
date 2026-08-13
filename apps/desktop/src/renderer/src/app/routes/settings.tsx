/**
 * Settings, and the privacy position stated plainly.
 */

import { useEffect, useState } from 'react'
import { useBackend } from '../../backend/backend-provider'
import { type AppInfo, bridge } from '../../platform/bridge'
import { Card, Field, Screen, inputClass } from '../../viz/ui'
import { CURRENCIES, type CurrencyCode, setCurrency, useCurrency } from '../preferences'

export function Settings() {
  const { info } = useBackend()
  const [app, setApp] = useState<AppInfo | null>(null)
  const currency = useCurrency()

  useEffect(() => {
    void bridge.app.info().then(setApp)
  }, [])

  return (
    <Screen title="Settings">
      <div className="flex max-w-2xl flex-col gap-4">
        <Card title="Privacy">
          <ul className="flex flex-col gap-1.5 text-text-dim leading-relaxed">
            <li>No account, no telemetry, no analytics, no cloud database, no remote backend.</li>
            <li>
              Prompt text, response text and tool input/output are <strong>not recorded</strong>.
              Only counts, models, timings and costs are stored.
            </li>
            <li>
              Everything lives in one local database file
              {app?.userDataPath && (
                <span className="font-mono text-[11px] text-text-mute"> ({app.userDataPath})</span>
              )}
              .
            </li>
            <li>
              The interface cannot reach the network at all. Requests to anything other than the
              local backend are blocked in the desktop shell
              {app && (
                <span className={app.blockedNetworkRequests > 0 ? 'text-warn' : 'text-exact'}>
                  {' '}
                  ({app.blockedNetworkRequests} blocked so far)
                </span>
              )}
              .
            </li>
            <li>
              Environment variables passed to an agent are used and discarded. They are never stored
              and never sent back to the interface.
            </li>
          </ul>
        </Card>

        <Card title="Currency">
          <Field
            label="show amounts in"
            htmlFor="currency"
            hint="Providers publish in USD, so anything else is converted. A converted amount is marked calculated rather than exact, and one converted with a rate more than a week old is downgraded again and says how old it is."
          >
            <select
              id="currency"
              className={`${inputClass} w-40`}
              value={currency}
              onChange={(e) => setCurrency(e.target.value as CurrencyCode)}
            >
              {CURRENCIES.map((c) => (
                <option key={c} value={c}>
                  {c}
                </option>
              ))}
            </select>
          </Field>
          {currency !== 'USD' && (
            <p className="mt-2 text-[11px] text-text-mute leading-relaxed">
              Conversion needs a rate for {currency}. Enter one under Models &amp; Pricing — this
              application does not fetch rates, because doing so would mean making a network request
              on your behalf. Without a rate, amounts say so instead of showing a dollar figure
              under a {currency} label.
            </p>
          )}
        </Card>

        <Card title="Backend">
          <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">
            <Row label="state" value={info.phase} />
            <Row label="contract" value={info.contractVersion ?? '—'} />
            <Row label="generation" value={String(info.generation)} />
            {app && <Row label="app version" value={app.version} />}
            {app && <Row label="platform" value={`${app.platform} ${app.arch}`} />}
          </dl>
        </Card>
      </div>
    </Screen>
  )
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt className="text-text-mute">{label}</dt>
      <dd className="numeric font-mono text-[12px] text-text-dim">{value}</dd>
    </>
  )
}
