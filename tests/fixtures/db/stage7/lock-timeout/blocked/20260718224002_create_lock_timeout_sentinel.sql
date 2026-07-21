-- Logical owner: stage7-lock-timeout-test
-- Compatibility phase: expand
-- Irreversible risk: none
-- Transaction: required
-- Lock timeout: 500ms
-- Statement timeout: 5s
-- Backup point: not-required
-- Recovery command: Release the reviewed table lock, then rerun db up.
CREATE TABLE stage7_lock_timeout_sentinel (
  id bigint PRIMARY KEY
);
