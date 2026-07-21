-- Logical owner: stage7-sql-failure-test
-- Compatibility phase: expand
-- Irreversible risk: none
-- Transaction: required
-- Lock timeout: 500ms
-- Statement timeout: 5s
-- Backup point: not-required
-- Recovery command: SQLx rolls back the transaction; correct the migration and rerun db up.
CREATE TABLE stage7_sql_failure_preceding (
  id bigint PRIMARY KEY
);
