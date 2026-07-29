-- Logical owner: game-server
-- Compatibility phase: expand
-- Irreversible risk: none
-- Transaction: required
-- Lock timeout: 5s
-- Statement timeout: 60s
-- Backup point: not-required
-- Recovery command: SQLx rolls back the transaction; correct the migration and rerun db up.

-- A legacy game-server startup path rewrote this guard with a non-canonical
-- function body. Restore the migration-owned definition before the runtime
-- DDL path is removed from the application image.
CREATE OR REPLACE FUNCTION reject_character_asset_ledger_mutation()
RETURNS trigger AS $$
BEGIN
  RAISE EXCEPTION 'character_asset_ledger is append-only; write a compensating asset transaction instead';
END;
$$ LANGUAGE plpgsql;
