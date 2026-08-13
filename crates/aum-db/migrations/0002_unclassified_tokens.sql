-- Tokens the provider counted but did not classify as input or output.
--
-- Codex's compaction calls report `last_token_usage` with a total of ~13,000
-- alongside `input_tokens: 0` and `output_tokens: 0`. Those tokens were spent
-- and billed; the provider simply does not say on which side. Across this
-- machine's history they amount to 3,082,662 tokens, which the first version of
-- the adapter recorded as zero.
--
-- They are counted here and priced nowhere: input and output rates differ by
-- roughly eight times, so attributing them to either side would be a guess with
-- a material cost consequence.
--
-- A separate migration rather than an edit to 0001, because 0001 has already
-- been applied to a database on this machine and sqlx verifies migration
-- checksums — rewriting history here would break an existing install for no
-- benefit.

ALTER TABLE ai_request  ADD COLUMN unclassified INTEGER NOT NULL DEFAULT 0;
ALTER TABLE token_usage ADD COLUMN unclassified INTEGER NOT NULL DEFAULT 0;
