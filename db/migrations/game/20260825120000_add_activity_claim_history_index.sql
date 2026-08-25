-- Logical owner: game-server
-- Compatibility phase: expand
-- Irreversible risk: none
-- Transaction: required
-- Lock timeout: 5s
-- Statement timeout: 60s
-- Backup point: not-required
-- Recovery command: SQLx rolls back the transaction; rerun after correcting the migration.

-- Player history uses keyset pagination across all activities. The identity column is the
-- deterministic tie-breaker when multiple records share the same timestamp.
CREATE INDEX IF NOT EXISTS idx_activity_claim_history_owner
  ON activity_claim_record (character_id, created_at DESC, id DESC);
