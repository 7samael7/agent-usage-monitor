/**
 * The wire contract, as TypeScript.
 *
 * These types mirror `crates/aum-contract`. From the next milestone they are
 * generated from `openapi.json` (produced by `cargo xtask contract-gen`) and a
 * snapshot test fails if the generated output drifts from what is committed.
 * Until then they are hand-written and reviewed against the Rust side.
 *
 * Two invariants matter more than the shapes themselves:
 *
 *  - `Money` is a **string**. It is a decimal amount, and parsing it into a
 *    JavaScript `number` reintroduces exactly the precision loss the string
 *    encoding exists to prevent. Format it, compare it, but do not do
 *    arithmetic on it as a float.
 *  - `Measured<T>` has `value: T | null`, and `null` means *no measurement
 *    exists* — never zero. Rendering it as `0` is the single highest-impact bug
 *    available in this product.
 */

export type { HealthResponse, MetaResponse } from './dto'
export * from './measurement'
export * from './tokens'
export * from './events'
export * from './dto'
