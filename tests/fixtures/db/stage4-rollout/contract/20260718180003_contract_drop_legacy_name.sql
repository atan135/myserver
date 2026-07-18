-- Logical owner: stage4-rollout-test
-- Compatibility phase: contract
-- Irreversible risk: data-loss
-- Transaction: required
-- Lock timeout: 5s
-- Statement timeout: 60s
-- Backup point: stage4-rollout-legacy-name-before-contract
-- Recovery command: Restore stage4_rollout_accounts_backup, recreate legacy_name, and repopulate it before returning old service traffic.
-- Risk summary: Dropping legacy_name discards values still required by an old service version.
ALTER TABLE stage4_rollout_accounts DROP COLUMN legacy_name;
