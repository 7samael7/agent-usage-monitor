# Privacy

This application is private and local-first. That is not a promise made in a
document; where it can be made structural, it has been.

## What it is

- **No account.** There is nothing to sign in to.
- **No telemetry, no analytics, no crash reporting.** Nothing is sent anywhere
  about how the application is used.
- **No cloud database, no remote backend.** The backend is a process on this
  machine listening on loopback.

## Where data lives

One SQLite file under the operating system's application-support directory
(`~/Library/Application Support/agent-usage-monitor/capture.sqlite3` on macOS,
with the platform equivalents elsewhere). Deleting it deletes everything the
application knows.

## What is stored

Metadata only, by default:

- timestamps, provider, model, token counts, cost, duration, status
- which task and which application a request belonged to
- the provider's own usage object, verbatim, as an audit trail — roughly 400
  bytes per request, containing counts and nothing else

**Not stored by default:** prompt text, response text, tool inputs, tool
outputs, reasoning text, or file contents. The audit trail deliberately keeps
only the usage object rather than the whole transcript line, and a test asserts
that no conversation-carrying key survives into it.

If content capture is ever enabled, it will carry an explicit warning: AI
conversations routinely contain credentials, private source code, and customer
data.

## Environment variables

An agent launched by the monitor may need environment variables — API keys,
endpoints, feature flags. They are passed to the child process and **never
stored, never logged, and never returned by the API**. In the interface they are
write-only: entered once, and shown afterwards only as `NAME = ••••`.

A monitoring tool that accumulated other people's API keys would be a worse
liability than the problem it solves.

## The interface cannot reach the network

This is enforced rather than intended. In the desktop shell:

- a Content-Security-Policy restricts `connect-src` to the local backend;
- a request filter in the main process **cancels** any request from the
  interface that is not the local backend, and counts the attempts;
- the count is shown in Settings, so the claim is checkable rather than trusted.

All outbound network access belongs to the backend, behind settings that can be
turned off individually. Today that means exchange-rate and pricing updates, and
nothing else.

## The local port is still a boundary

The backend listens on `127.0.0.1`, which is not by itself private: any local
process can reach it, and so can any web page you happen to visit, via DNS
rebinding. Four layers apply:

1. an ephemeral port, so there is no fixed target;
2. a 256-bit bearer token, compared in constant time, passed to the backend in
   its environment and never in its command line — a command line is readable by
   every process on the machine;
3. a `Host` allowlist, which is what actually defeats rebinding: an attacker can
   point their domain at `127.0.0.1`, but the browser still sends *their*
   hostname;
4. an `Origin` allowlist with no wildcard.

## Reading your files

The application reads the agents' own transcript files. It does not read
anything else, does not modify them, and does not copy their contents into its
database beyond the usage objects described above.

## Test fixtures

Golden-file tests are built from real transcripts, which contain prompts,
responses and source code. Every fixture is put through
`scripts/redact-fixture.ts` **before** it is written into the repository, never
as a later cleanup pass — a file committed unredacted once stays in history.
Redaction keeps every number and structural field and replaces all free text; a
test asserts that no path or prose survives, and `.gitignore` blocks `*.jsonl`
outside the fixtures directory.

## What leaves this machine

Nothing, unless you enable exchange-rate or pricing updates, which fetch public
rate and price data and send nothing about you.
