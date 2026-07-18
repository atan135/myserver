-- Logical owner: stage4-rollout-test
-- Compatibility phase: expand
-- Irreversible risk: none
-- Transaction: required
-- Lock timeout: 5s
-- Statement timeout: 60s
-- Backup point: not-required
-- Recovery command: SQLx rolls back the transaction; correct the migration and rerun db up.
ALTER TABLE stage4_rollout_accounts ADD COLUMN display_name text;
