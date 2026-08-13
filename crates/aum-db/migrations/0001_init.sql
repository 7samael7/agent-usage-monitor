-- Initial schema.
--
-- Conventions, applied everywhere:
--
--   time   TEXT, ISO-8601 UTC with milliseconds ("2026-08-12T19:08:22.002Z").
--          Matches both agents' native format exactly, sorts lexicographically,
--          and stays readable in a database browser.
--   money  INTEGER, nano-units (1e-9). i64 covers ±9.2 billion units exactly.
--          Never REAL: a float cannot represent a cent, let alone a sum of
--          sub-cent per-request costs.
--   ids    TEXT, UUID. Generated in-process so a row can be referenced before
--          it is written.
--
-- Nothing here stores prompt or response text. Content capture is off by
-- default and, when enabled, lands in its own table added by a later migration
-- so that "delete my content" stays a single, obvious operation.

-- ── Catalog ─────────────────────────────────────────────────────────────────

CREATE TABLE provider (
    id           TEXT PRIMARY KEY,          -- 'anthropic' | 'openai'
    display_name TEXT NOT NULL
) STRICT;

CREATE TABLE application (
    id              TEXT PRIMARY KEY,       -- 'claude_code' | 'codex' | 'claude_desktop'
    display_name    TEXT NOT NULL,
    provider_id     TEXT REFERENCES provider(id),
    -- Resolved path, recorded because it is routinely surprising: the Codex
    -- binary lives inside ChatGPT.app rather than on PATH.
    executable_path TEXT,
    version         TEXT,
    detected_at     TEXT
) STRICT;

CREATE TABLE model (
    id          TEXT PRIMARY KEY,           -- provider's own id, e.g. 'claude-opus-5'
    provider_id TEXT REFERENCES provider(id),
    family      TEXT,
    -- Set when the model was seen in real data. Models appear in the wild long
    -- before any price list mentions them, so this is deliberately independent
    -- of whether pricing exists.
    first_seen_at TEXT
) STRICT;

-- ── Pricing (append-only; a row is never UPDATEd) ────────────────────────────
--
-- A benchmark run in March must still show March's numbers in August, so cost
-- rows pin the exact pricing version used. Correcting a price creates a new
-- version and closes the previous one; recalculation is an explicit, audited
-- action, never a side effect of editing a table.

CREATE TABLE pricing_version (
    id       TEXT PRIMARY KEY,
    model_id TEXT NOT NULL REFERENCES model(id),

    -- All rates in nano-USD per million tokens. $15.00/Mtok = 15_000_000_000.
    input_per_mtok        INTEGER NOT NULL,
    output_per_mtok       INTEGER NOT NULL,
    cache_read_per_mtok   INTEGER,
    -- Split by TTL because the multipliers genuinely differ (~1.25x vs ~2x).
    -- Collapsing them understates real Claude sessions by roughly a quarter.
    cache_write_5m_per_mtok  INTEGER,
    cache_write_1h_per_mtok  INTEGER,

    effective_from  TEXT NOT NULL,
    effective_until TEXT,
    -- 'seed' | 'user' | 'updater'. A user override wins at equal specificity.
    source          TEXT NOT NULL,
    note            TEXT,
    created_at      TEXT NOT NULL
) STRICT;

CREATE INDEX ix_pricing_lookup ON pricing_version(model_id, effective_from DESC);

CREATE TABLE exchange_rate (
    id            TEXT PRIMARY KEY,
    quote_currency TEXT NOT NULL,           -- 'EUR' | 'CZK'  (base is always USD)
    -- Rate in nano-units per 1 USD.
    rate_nano     INTEGER NOT NULL,
    as_of         TEXT NOT NULL,
    source        TEXT NOT NULL,            -- 'manual' | provider name
    fetched_at    TEXT NOT NULL
) STRICT;

CREATE INDEX ix_fx_lookup ON exchange_rate(quote_currency, as_of DESC);

-- ── Benchmarks and tasks ────────────────────────────────────────────────────

CREATE TABLE benchmark (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    notes      TEXT,
    started_at TEXT,
    ended_at   TEXT,
    created_at TEXT NOT NULL,
    -- Environment captured at run time, so a past comparison stays
    -- interpretable: OS, CPU, RAM, app/adapter versions, pricing and FX
    -- versions. JSON because it is written once and only ever read whole.
    environment TEXT
) STRICT;

CREATE TABLE task (
    id           TEXT PRIMARY KEY,
    benchmark_id TEXT REFERENCES benchmark(id) ON DELETE SET NULL,
    name         TEXT NOT NULL,
    adapter_id   TEXT NOT NULL,
    status       TEXT NOT NULL,             -- pending|running|completed|failed|stopped
    working_dir  TEXT,
    -- The command as launched. Environment variables are deliberately NOT
    -- stored: they routinely contain API keys, and a monitoring tool has no
    -- business keeping them.
    command      TEXT,
    model_id     TEXT,
    created_at   TEXT NOT NULL,
    started_at   TEXT,
    ended_at     TEXT,
    exit_code    INTEGER
) STRICT;

CREATE INDEX ix_task_status ON task(status, created_at DESC);
CREATE INDEX ix_task_benchmark ON task(benchmark_id);

-- The anti-cross-attribution guarantee, made structural.
--
-- `session_id` is the PRIMARY KEY: a provider session belongs to at most one
-- task, enforced by the database rather than by careful code. The ingest write
-- path does exactly one indexed equality lookup against this table — there is
-- no fuzzy matching, no scoring, and no nearest-match anywhere in it. Twenty
-- concurrent tasks therefore cannot contaminate one another; a second bind
-- attempt fails loudly instead of silently stealing a session.
CREATE TABLE task_binding (
    session_id TEXT PRIMARY KEY,
    task_id    TEXT NOT NULL REFERENCES task(id) ON DELETE CASCADE,
    adapter_id TEXT NOT NULL,
    -- launched_pinned | launched_stdout | pid_session_file | session_id_exact
    -- | user_assigned.  Deliberately no 'heuristic'.
    method     TEXT NOT NULL,
    evidence   TEXT NOT NULL,               -- JSON: what proved this binding
    bound_at   TEXT NOT NULL
) STRICT;

CREATE INDEX ix_binding_task ON task_binding(task_id);

-- Every provider session we observe, whether or not a task claims it.
CREATE TABLE agent_session (
    id          TEXT PRIMARY KEY,           -- the provider's own session id
    adapter_id  TEXT NOT NULL,
    cwd         TEXT,
    model_id    TEXT,
    originator  TEXT,                       -- e.g. 'Codex Desktop'
    app_version TEXT,
    first_seen_at TEXT NOT NULL,
    last_seen_at  TEXT NOT NULL
) STRICT;

-- One row per sidecar run. Scopes the SSE stream epoch and lets a restart be
-- distinguished from a gap in the data.
CREATE TABLE monitoring_session (
    id           TEXT PRIMARY KEY,
    stream_epoch TEXT NOT NULL,
    app_version  TEXT NOT NULL,
    started_at   TEXT NOT NULL,
    ended_at     TEXT
) STRICT;

CREATE TABLE process (
    id            TEXT PRIMARY KEY,
    task_id       TEXT REFERENCES task(id) ON DELETE CASCADE,
    pid           INTEGER NOT NULL,
    -- PID alone is reusable; the pair is not. Every process identity in this
    -- application is (pid, start_time) for exactly that reason.
    start_time    INTEGER NOT NULL,
    parent_pid    INTEGER,
    executable    TEXT,
    is_root       INTEGER NOT NULL DEFAULT 0,
    observed_at   TEXT NOT NULL,
    exited_at     TEXT
) STRICT;

CREATE INDEX ix_process_task ON process(task_id);

-- ── Usage ───────────────────────────────────────────────────────────────────

-- One logical model turn.
--
-- `dedup_key` is computed by the adapter and is the identity that makes
-- re-ingest safe. It differs per provider because the providers differ:
--   Claude Code -> requestId + message id  (one response is written to as many
--                  as 21 JSONL lines, each repeating the whole usage object;
--                  summing lines inflates totals 2-3x)
--   Codex       -> session + event ordinal (no per-request id exists)
CREATE TABLE ai_request (
    id         TEXT PRIMARY KEY,
    adapter_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    -- NULL means unattributed: observed, counted, and shown, but not claimed by
    -- any task. Never guessed into one.
    task_id    TEXT REFERENCES task(id) ON DELETE SET NULL,
    dedup_key  TEXT NOT NULL,

    model_id   TEXT,
    occurred_at TEXT NOT NULL,

    -- provider_exact | provider_cumulative_delta | provider_off_ledger |
    -- provider_inconsistent | otel | stream_json | estimated_tokenizer | unknown
    measurement_source TEXT NOT NULL,
    -- turn | off_ledger | coalesced | failed
    request_kind       TEXT NOT NULL DEFAULT 'turn',
    -- How the task binding was established, so the UI can show attribution
    -- strength rather than implying all rows are equally certain.
    attribution_method TEXT,

    -- Denormalized winning measurement, for the dashboard's hot path. The
    -- normalized rows in token_usage remain the source of truth.
    input_fresh             INTEGER NOT NULL DEFAULT 0,
    cache_read              INTEGER NOT NULL DEFAULT 0,
    cache_write_5m          INTEGER NOT NULL DEFAULT 0,
    cache_write_1h          INTEGER NOT NULL DEFAULT 0,
    cache_write_unspecified INTEGER NOT NULL DEFAULT 0,
    output_total            INTEGER NOT NULL DEFAULT 0,
    -- NULL means the provider does not report reasoning. Never 0, which would
    -- assert that no reasoning happened.
    reasoning               INTEGER,

    -- Sub-agent attribution (Claude Code sidechains carry the parent session id).
    is_sidechain INTEGER NOT NULL DEFAULT 0,
    agent_id     TEXT,
    agent_type   TEXT,

    created_at TEXT NOT NULL,

    UNIQUE(adapter_id, session_id, dedup_key)
) STRICT;

CREATE INDEX ix_req_task_time ON ai_request(task_id, occurred_at);
CREATE INDEX ix_req_sess_time ON ai_request(session_id, occurred_at);
-- Partial index: the Unattributed view is a first-class screen, not an
-- afterthought, because what the application declined to guess is information.
CREATE INDEX ix_req_unattributed ON ai_request(occurred_at) WHERE task_id IS NULL;

-- Usage per (request, source).
--
-- Kept normalized rather than folded into ai_request because Claude Code can
-- report the same request three ways — transcript, OTEL, and stream-json — and
-- comparing them is precisely the accuracy signal this application exists to
-- surface.
CREATE TABLE token_usage (
    ai_request_id      TEXT NOT NULL REFERENCES ai_request(id) ON DELETE CASCADE,
    measurement_source TEXT NOT NULL,

    input_fresh             INTEGER NOT NULL DEFAULT 0,
    cache_read              INTEGER NOT NULL DEFAULT 0,
    cache_write_5m          INTEGER NOT NULL DEFAULT 0,
    cache_write_1h          INTEGER NOT NULL DEFAULT 0,
    cache_write_unspecified INTEGER NOT NULL DEFAULT 0,
    output_total            INTEGER NOT NULL DEFAULT 0,
    reasoning               INTEGER,

    -- The provider's object, verbatim (~400 bytes/row). This is the audit
    -- trail: it lets any displayed number be traced to its source bytes, and
    -- lets a parsing mistake be corrected by re-deriving from here instead of
    -- re-reading 1.3 GiB of transcripts.
    raw_json    TEXT,
    observed_at TEXT NOT NULL,

    PRIMARY KEY (ai_request_id, measurement_source)
) STRICT;

-- Three genuinely different quantities, one row each. Never summed together,
-- never collapsed into a single "cost" column.
CREATE TABLE cost_calculation (
    ai_request_id TEXT NOT NULL REFERENCES ai_request(id) ON DELETE CASCADE,
    -- api_equivalent_computed | provider_reported | actual_billed
    basis         TEXT NOT NULL,

    -- NULL with a reason means unavailable, which is a real and common answer:
    -- both agents here are subscription-billed, so `actual_billed` genuinely
    -- cannot be known. NULL is never rendered as zero.
    amount_nano_usd    INTEGER,
    unavailable_reason TEXT,

    -- Pinned so historical runs stay reproducible.
    pricing_version_id TEXT REFERENCES pricing_version(id),
    exchange_rate_id   TEXT REFERENCES exchange_rate(id),
    calculated_at      TEXT NOT NULL,

    PRIMARY KEY (ai_request_id, basis)
) STRICT;

-- ── Telemetry and operations ────────────────────────────────────────────────

CREATE TABLE metric_sample (
    id         TEXT PRIMARY KEY,
    task_id    TEXT REFERENCES task(id) ON DELETE CASCADE,
    sampled_at TEXT NOT NULL,
    cpu_percent      REAL,
    rss_bytes        INTEGER,
    process_count    INTEGER,
    -- The monitor measuring itself, so its own overhead is visible rather than
    -- assumed negligible.
    is_self    INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE INDEX ix_metric_task_time ON metric_sample(task_id, sampled_at);

-- Durable event log, backing SSE `Last-Event-ID` replay.
CREATE TABLE event (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    monitoring_session_id TEXT NOT NULL REFERENCES monitoring_session(id) ON DELETE CASCADE,
    task_id    TEXT REFERENCES task(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL,
    payload    TEXT NOT NULL,
    created_at TEXT NOT NULL
) STRICT;

CREATE INDEX ix_event_task ON event(task_id, seq);

-- ── Ingest bookkeeping ──────────────────────────────────────────────────────

-- Files are identified by (device, inode), never by path: paths change when a
-- worktree moves or a directory is renamed, and the project-directory naming
-- scheme used by Claude Code is lossy anyway (a literal '-' in a path is
-- indistinguishable from a '/').
CREATE TABLE ingest_file (
    id          TEXT PRIMARY KEY,
    adapter_id  TEXT NOT NULL,
    device      INTEGER NOT NULL,
    inode       INTEGER NOT NULL,
    path        TEXT NOT NULL,              -- a label, may change
    first_seen_at TEXT NOT NULL,
    last_seen_at  TEXT NOT NULL,
    UNIQUE(device, inode)
) STRICT;

CREATE TABLE ingest_cursor (
    file_id     TEXT PRIMARY KEY REFERENCES ingest_file(id) ON DELETE CASCADE,
    -- Always positioned just past the last complete newline. A trailing partial
    -- line is never persisted, which is what makes a half-written JSONL line a
    -- non-issue: on restart it is simply re-read from disk.
    byte_offset INTEGER NOT NULL DEFAULT 0,
    line_ordinal INTEGER NOT NULL DEFAULT 0,
    size_seen   INTEGER NOT NULL DEFAULT 0,
    -- Detects truncate-and-replace, where size alone would not.
    head_sha256 TEXT,
    -- Per-file adapter state, e.g. Codex's previous cumulative counters.
    adapter_state TEXT,
    updated_at  TEXT NOT NULL
) STRICT;

-- Things that did not add up. Recorded and surfaced; never silently corrected.
-- An anomaly on a session makes every aggregate containing it non-exact, which
-- is the entire point of keeping them.
CREATE TABLE ingest_anomaly (
    id         TEXT PRIMARY KEY,
    adapter_id TEXT NOT NULL,
    session_id TEXT,
    file_id    TEXT REFERENCES ingest_file(id) ON DELETE SET NULL,
    kind       TEXT NOT NULL,
    detail     TEXT NOT NULL,
    occurred_at TEXT NOT NULL
) STRICT;

CREATE INDEX ix_anomaly_session ON ingest_anomaly(session_id, occurred_at);

-- What each adapter was observed to actually support, with the evidence.
-- Rewritten on each probe; never hand-authored, so the capability matrix in the
-- UI reflects reality rather than optimism.
CREATE TABLE adapter_capability (
    adapter_id TEXT NOT NULL,
    capability TEXT NOT NULL,
    -- supported | degraded | unsupported | unknown
    state      TEXT NOT NULL,
    evidence   TEXT,
    caveat     TEXT,
    probed_at  TEXT NOT NULL,
    PRIMARY KEY (adapter_id, capability)
) STRICT;

-- Extension point for later benchmark quality evaluation. Deliberately empty
-- of judgement logic for now: the first implementation measures consumption,
-- and scoring code quality is a separate problem that should not be guessed at.
CREATE TABLE benchmark_score (
    id           TEXT PRIMARY KEY,
    task_id      TEXT NOT NULL REFERENCES task(id) ON DELETE CASCADE,
    scorer       TEXT NOT NULL,
    score        REAL,
    detail       TEXT,
    scored_at    TEXT NOT NULL
) STRICT;

-- ── Seed data ───────────────────────────────────────────────────────────────

INSERT INTO provider (id, display_name) VALUES
    ('anthropic', 'Anthropic'),
    ('openai',    'OpenAI');

INSERT INTO application (id, display_name, provider_id) VALUES
    ('claude_code',    'Claude Code',    'anthropic'),
    ('codex',          'Codex',          'openai'),
    ('claude_desktop', 'Claude Desktop', 'anthropic');
