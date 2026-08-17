# Privacy

This application is private and local-first. That is not a promise made in a
document; where it can be made structural, it has been.

It also got considerably simpler. There was a desktop shell and a local HTTP
server, and most of this page used to be about defending that port. There is no
port now, and no window: `aum` is one process that reads files, writes one
database, and draws on your terminal.

## What it is

- **No account.** There is nothing to sign in to.
- **No telemetry, no analytics, no crash reporting.** Nothing is sent anywhere
  about how the application is used.
- **No cloud database, no remote backend, no server.** Nothing listens on a
  socket, so nothing on the machine — or on the network — can talk to it.

## Where data lives

One SQLite file under the operating system's application-support directory
(`~/Library/Application Support/agent-usage-monitor/capture.sqlite3` on macOS,
with the platform equivalents elsewhere; `AUM_DATA_DIR` overrides it). Deleting
it deletes everything the application knows.

## What is stored

Metadata only:

- timestamps, adapter, model, token counts by band, cost, status
- which session a request belonged to
- the provider's own usage object, verbatim, as an audit trail — roughly 400
  bytes per request, containing counts and nothing else

**Not stored:** prompt text, response text, tool inputs, tool outputs, reasoning
text, or file contents. The audit trail deliberately keeps only the usage object
rather than the whole transcript line, and a test asserts that no
conversation-carrying key survives into it.

There is no setting that turns content capture on. If one is ever added it will
carry an explicit warning, because AI conversations routinely contain
credentials, private source code and customer data.

## Reading your files

The application reads two directories:

```
~/.claude/projects/                                   Claude Code transcripts, sub-agents included
~/.codex/sessions/                                    Codex rollout files
~/Library/Application Support/Claude/buddy-tokens.json  Claude Desktop's own daily counter
```

It opens them read-only, never modifies them, and copies nothing out of them
beyond the usage objects described above.

## What leaves this machine

Nothing.

There is no exchange-rate fetch and no price download: prices and FX rates are
entered by you, with `aum price` and `aum fx`, and stored with the date and a
note saying where the figure came from. An earlier design fetched both, and this
page used to describe the settings that disabled them.

The strongest version of this claim that is actually true: **the dependency
graph contains no HTTP client and no TLS library** — no `reqwest`, `hyper`,
`ureq`, `rustls`, `native-tls` or `openssl` — and no code in this workspace opens
a socket. `cargo tree` shows it, and `Cargo.lock` is committed.

What that is *not* is a proof that the binary is incapable of networking:
`sqlx` enables `tokio/net` transitively, because it ships drivers for network
databases next to the SQLite one this uses. The capability is linked in; nothing
here reaches for it. Stating this as "the binary physically cannot open a socket"
would be the same kind of overclaim the rest of the project exists to avoid.

## The interface

The interface is a terminal program in the same process as the engine. It has no
browser, no renderer, no JavaScript and no content to sandbox, so the whole
category of "can the UI phone home" does not arise — it is the same process, and
that process has nothing to phone home with.

Exports (`--json`, and `e` in the interface) contain what the tables contain:
counts, costs and identifiers. There is no content to export.

## Environment variables

The monitor reads three: `AUM_DATA_DIR` (where the database lives), `NO_COLOR`,
and `PATH` — the last only to report on the **Apps** tab where each agent's
executable is, which is why Codex shows up as living inside `ChatGPT.app`. It
does not read, store, display or pass on API keys, and none of the three is ever
written to the database.

This section used to be much longer. When the monitor could launch agents, it
had to accept environment variables for the child process, and the rule was that
they were write-only and never logged. It launches nothing now, so it holds no
credentials at all — the safest way to handle a secret turned out to be not
having a reason to touch one.

## Test fixtures

Golden-file tests are built from real transcripts, which contain prompts,
responses and source code. Every fixture goes through `cargo xtask redact`
**before** it is written into the repository, never as a later cleanup pass — a
file committed unredacted once stays in history.

Redaction keeps every number and structural field and replaces all free text.
Three things enforce it:

1. the tool checks its own output and writes **nothing** if any string would
   still be published, naming the key and the line but not quoting the text;
2. `cargo test fixtures_are_redacted` re-checks every `.jsonl` already committed
   under `tests/fixtures/`, so a file that arrived some other way is still
   caught;
3. `.gitignore` blocks `*.jsonl` everywhere except that directory.

The key list is a list, not a pattern: a field is published because someone put
its name in `KEEP_KEYS`, not because it happened to match a rule. Anything
unrecognised is redacted.
