-- Logical owner: stage7-interruption-test
-- Compatibility phase: migrate
-- Irreversible risk: none
-- Transaction: required
-- Lock timeout: 500ms
-- Statement timeout: 5s
-- Backup point: not-required
-- Recovery command: Inspect the failed session, then rerun the reviewed migration after the connection is restored.
SELECT pg_sleep(3) /* stage7-interruption-hold */;
CREATE TABLE stage7_connection_interruption_sentinel (
  id bigint PRIMARY KEY
);
