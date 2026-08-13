/**
 * The small set of primitives every screen uses.
 *
 * Deliberately hand-rolled and few. Seven dense screens of tables and numbers
 * do not need a component library, and one would bring styling opinions that
 * would then have to be argued with.
 */

import type { ReactNode } from 'react'

export function Screen({
  title,
  subtitle,
  actions,
  children,
}: {
  title: string
  subtitle?: string
  actions?: ReactNode
  children: ReactNode
}) {
  return (
    <div className="p-6">
      <div className="mb-5 flex items-start justify-between gap-4">
        <div>
          <h1 className="mb-1 font-semibold text-[15px]">{title}</h1>
          {subtitle && <p className="max-w-2xl text-text-mute leading-relaxed">{subtitle}</p>}
        </div>
        {actions && <div className="flex shrink-0 gap-2">{actions}</div>}
      </div>
      {children}
    </div>
  )
}

export function Card({
  title,
  children,
  className = '',
}: {
  title?: string
  children: ReactNode
  className?: string
}) {
  return (
    <div className={`rounded-md border border-border bg-surface p-4 ${className}`}>
      {title && (
        <h2 className="mb-3 font-medium text-[11px] text-text-mute uppercase tracking-wide">
          {title}
        </h2>
      )}
      {children}
    </div>
  )
}

export function Stat({
  label,
  value,
  tone,
  hint,
}: {
  label: string
  value: ReactNode
  tone?: 'warn' | 'neg'
  hint?: string
}) {
  const colour = tone === 'warn' ? 'text-warn' : tone === 'neg' ? 'text-neg' : 'text-text'
  return (
    <div className="rounded-md border border-border bg-surface px-3 py-2" title={hint}>
      <div className="mb-0.5 text-[10px] text-text-mute uppercase tracking-wide">{label}</div>
      <div className={`numeric text-[16px] ${colour}`}>{value}</div>
    </div>
  )
}

export function Button({
  children,
  onClick,
  variant = 'default',
  disabled,
  type = 'button',
}: {
  children: ReactNode
  onClick?: () => void
  variant?: 'default' | 'primary' | 'danger'
  disabled?: boolean
  type?: 'button' | 'submit'
}) {
  const styles = {
    default: 'border-border-strong bg-surface-2 text-text hover:bg-surface-3',
    primary: 'border-accent-dim bg-accent-dim/30 text-accent hover:bg-accent-dim/50',
    danger: 'border-neg/40 bg-neg/10 text-neg hover:bg-neg/20',
  }[variant]

  return (
    <button
      type={type}
      onClick={onClick}
      disabled={disabled}
      className={`no-drag rounded border px-3 py-1.5 transition-colors disabled:cursor-not-allowed disabled:opacity-40 ${styles}`}
    >
      {children}
    </button>
  )
}

/**
 * A labelled form field.
 *
 * `htmlFor` associates the label with a single control. Where a field holds
 * several controls — a path box beside a browse button, or the name/value pair
 * of an environment variable — there is no single input to point at, so the
 * label becomes a group caption and the controls carry their own `aria-label`.
 * Wrapping several inputs in one `<label>` would associate it with whichever
 * came first, which is worse than not associating it at all.
 */
export function Field({
  label,
  hint,
  htmlFor,
  children,
}: {
  label: string
  hint?: string
  htmlFor?: string
  children: ReactNode
}) {
  const body = (
    <>
      <span className="text-[11px] text-text-dim">{label}</span>
      {children}
      {hint && <span className="text-[10px] text-text-mute leading-relaxed">{hint}</span>}
    </>
  )

  return htmlFor ? (
    <label htmlFor={htmlFor} className="flex flex-col gap-1">
      {body}
    </label>
  ) : (
    <fieldset className="flex flex-col gap-1 border-0 p-0">{body}</fieldset>
  )
}

export const inputClass =
  'rounded border border-border bg-surface-inset px-2 py-1.5 text-text ' +
  'outline-none focus:border-accent-dim'

export function Table({ head, children }: { head: ReactNode; children: ReactNode }) {
  return (
    <div className="overflow-x-auto rounded-md border border-border">
      <table className="w-full border-collapse text-left">
        <thead>
          <tr className="border-border border-b bg-surface-2 text-[10px] text-text-mute uppercase tracking-wide">
            {head}
          </tr>
        </thead>
        <tbody>{children}</tbody>
      </table>
    </div>
  )
}

export function Th({ children, align }: { children?: ReactNode; align?: 'right' }) {
  return (
    <th className={`px-3 py-1.5 font-medium ${align === 'right' ? 'text-right' : ''}`}>
      {children}
    </th>
  )
}

export function Td({
  children,
  align,
  numeric,
  title,
}: {
  children: ReactNode
  align?: 'right'
  numeric?: boolean
  title?: string
}) {
  return (
    <td
      title={title}
      className={`px-3 py-1.5 ${align === 'right' ? 'text-right' : ''} ${numeric ? 'numeric' : ''}`}
    >
      {children}
    </td>
  )
}

export function Row({ children }: { children: ReactNode }) {
  return <tr className="border-border/60 border-b last:border-0">{children}</tr>
}

export function Empty({ children }: { children: ReactNode }) {
  return <p className="text-text-mute leading-relaxed">{children}</p>
}

export function StatusDot({ status }: { status: string }) {
  const colour =
    status === 'running'
      ? 'bg-exact'
      : status === 'failed'
        ? 'bg-neg'
        : status === 'completed'
          ? 'bg-info'
          : 'bg-na'
  return <span className={`inline-block size-1.5 rounded-full ${colour}`} aria-hidden />
}
