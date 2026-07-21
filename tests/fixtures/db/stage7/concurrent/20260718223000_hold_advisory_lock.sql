-- Logical owner: stage7-concurrent-test
-- Compatibility phase: migrate
-- Irreversible risk: none
-- Transaction: required
-- Lock timeout: 500ms
-- Statement timeout: 5s
-- Backup point: not-required
-- Recovery command: Wait for the current migration holder to finish, then rerun db up.
SELECT pg_sleep(2) /* stage7-concurrent-hold */;
CREATE TABLE stage7_concurrent_fixture (
  id bigint PRIMARY KEY
);
