#!/usr/bin/env bun
/**
 * Turn a real agent transcript into a committable test fixture.
 *
 * Golden-file tests need real data — the fan-out, the synthetic failure
 * markers, the retry chains and the compaction boundaries are all things that
 * are hard to invent convincingly and easy to get subtly wrong. But real
 * transcripts contain prompts, responses, source code and absolute paths, and
 * this repository has a remote.
 *
 * So: **every number and every structural field is preserved exactly**, and
 * **all free text is replaced**. Token counts, ids, timestamps, models, types
 * and flags survive; message content, tool inputs and outputs, reasoning text
 * and file paths do not.
 *
 * Redaction happens on the way *in* to `tests/fixtures/`, never as a later
 * cleanup pass — a file committed unredacted once stays in history forever.
 *
 * Usage:
 *   bun run scripts/redact-fixture.ts <input.jsonl> <output.jsonl> [--max-lines N]
 */

import { existsSync, readFileSync, writeFileSync } from 'node:fs'
import path from 'node:path'

/** Keys whose values are free text and must not survive. */
const TEXT_KEYS = new Set([
  'text',
  'thinking',
  'signature',
  'content',
  'input',
  'output',
  'command',
  'description',
  'prompt',
  'summary',
  'message',
  'error',
  'detail',
  'stdout',
  'stderr',
  'result',
  'toolUseResult',
  'lastPrompt',
  'customTitle',
  'aiTitle',
  'title',
  'name',
  'displayName',
  'instructions',
  'base_instructions',
  'developer_instructions',
])

/** Keys that are paths: replaced with a stable synthetic path, not blanked. */
const PATH_KEYS = new Set([
  'cwd',
  'path',
  'file_path',
  'filePath',
  'workspace_roots',
  'transcript_path',
  'sqlite_home',
  'log_dir',
])

/**
 * Keys that must survive verbatim. Everything the parser reads and every
 * number a test asserts on.
 */
const KEEP_KEYS = new Set([
  'type',
  'subtype',
  'level',
  'role',
  'model',
  'sessionId',
  'uuid',
  'parentUuid',
  'requestId',
  'promptId',
  'timestamp',
  'version',
  'gitBranch',
  'entrypoint',
  'userType',
  'effort',
  'isSidechain',
  'isMeta',
  'isApiErrorMessage',
  'agentId',
  'attributionAgent',
  'retryAttempt',
  'maxRetries',
  'retryInMs',
  'stop_reason',
  'stopReason',
  'id',
  'usage',
  'preTokens',
  'postTokens',
  'cumulativeDroppedTokens',
  'input_tokens',
  'output_tokens',
  'cache_creation_input_tokens',
  'cache_read_input_tokens',
  'cache_creation',
  'ephemeral_5m_input_tokens',
  'ephemeral_1h_input_tokens',
  'server_tool_use',
  'web_search_requests',
  'web_fetch_requests',
  'service_tier',
  'speed',
  'inference_geo',
  'iterations',
  'total_token_usage',
  'last_token_usage',
  'cached_input_tokens',
  'cache_write_input_tokens',
  'reasoning_output_tokens',
  'total_tokens',
  'model_context_window',
  'rate_limits',
  'payload',
  'info',
])

/** Stable pseudonyms, so the same input maps to the same output every run. */
const pathAliases = new Map<string, string>()

function aliasPath(original: string): string {
  const existing = pathAliases.get(original)
  if (existing) return existing
  const alias = `/redacted/path-${pathAliases.size + 1}`
  pathAliases.set(original, alias)
  return alias
}

/**
 * Longest replacement string kept. Length is preserved below this so a fixture
 * still resembles real data; beyond it the shape stops mattering and the bytes
 * are just repository weight. The tailer's oversize and chunk-boundary tests
 * generate their own inputs and do not depend on this.
 */
const MAX_REDACTED_LEN = 200

function redactText(value: string): string {
  if (value.length === 0) return value
  // Short values are enum-like and structural rather than prose.
  if (value.length <= 3) return value
  return 'x'.repeat(Math.min(value.length, MAX_REDACTED_LEN))
}

function redact(value: unknown, key?: string): unknown {
  if (value === null || value === undefined) return value

  if (Array.isArray(value)) {
    return value.map((v) => redact(v, key))
  }

  if (typeof value === 'object') {
    const out: Record<string, unknown> = {}
    for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
      out[k] = redact(v, k)
    }
    return out
  }

  // Numbers and booleans are the whole point of the fixture: never touched.
  if (typeof value !== 'string') return value

  if (key && KEEP_KEYS.has(key)) return value
  if (key && PATH_KEYS.has(key)) return aliasPath(value)
  if (key && TEXT_KEYS.has(key)) return redactText(value)

  // Unknown string key: redact by default. Being wrong in this direction costs
  // a less realistic fixture; being wrong in the other direction publishes
  // someone's source code.
  if (value.startsWith('/') || value.includes('/Users/')) return aliasPath(value)
  return redactText(value)
}

function main(): void {
  const [input, output, ...rest] = process.argv.slice(2)
  if (!input || !output) {
    console.error('usage: redact-fixture.ts <input.jsonl> <output.jsonl> [--max-lines N]')
    process.exit(1)
  }
  if (!existsSync(input)) {
    console.error(`no such file: ${input}`)
    process.exit(1)
  }

  const maxIdx = rest.indexOf('--max-lines')
  const maxLines = maxIdx === -1 ? Number.POSITIVE_INFINITY : Number(rest[maxIdx + 1] ?? 0)

  const lines = readFileSync(input, 'utf8').split('\n').filter((l) => l.trim() !== '')
  const kept: string[] = []

  for (const line of lines) {
    if (kept.length >= maxLines) break
    try {
      kept.push(JSON.stringify(redact(JSON.parse(line))))
    } catch {
      // A line we cannot parse cannot be redacted, so it cannot be published.
    }
  }

  writeFileSync(output, `${kept.join('\n')}\n`)
  console.log(
    `${path.basename(output)}: ${kept.length} lines, ${pathAliases.size} paths pseudonymised`,
  )
}

main()
