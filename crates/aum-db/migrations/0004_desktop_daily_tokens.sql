-- Claude Desktop's daily token counter, sampled.
--
-- The application keeps one running total for the current day and resets it at
-- midnight, so yesterday's figure is gone unless something was watching. A
-- monitor that runs continuously can keep the history the application does not.
--
-- Deliberately its own table rather than a row in `ai_request`. This number has
-- no model, no input/output split, no session and no request boundary, so it
-- cannot be attributed to a task or priced. Putting it anywhere the aggregates
-- can reach would let it leak into a total that claims to be per-task or
-- per-model, which is precisely the confident-but-wrong number this application
-- refuses to produce.
CREATE TABLE desktop_daily_tokens (
    -- Local calendar day as the application writes it, 'YYYY-MM-DD'.
    day           TEXT PRIMARY KEY,
    -- Highest value seen for that day. Within a day the counter only grows, so
    -- a maximum is both correct and idempotent: sampling more often cannot
    -- inflate it, and a sample taken just after midnight cannot lower the day
    -- that just ended.
    tokens        INTEGER NOT NULL,
    -- Which application's counter this is, so a second desktop app later gets
    -- its own rows rather than fighting over these.
    source        TEXT NOT NULL DEFAULT 'claude_desktop',
    first_seen_at TEXT NOT NULL,
    last_seen_at  TEXT NOT NULL
) STRICT;
