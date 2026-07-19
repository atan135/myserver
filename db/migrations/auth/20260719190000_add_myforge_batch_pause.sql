-- Logical owner: auth-http
-- Compatibility phase: migrate
-- Irreversible risk: none
-- Transaction: required
-- Lock timeout: 5s
-- Statement timeout: 60s
-- Backup point: not-required
-- Recovery command: stop MyForge writers, resume or cancel paused tasks, then restore the prior status CHECK in a reviewed follow-up migration.
-- Persisted queue pause state keeps a paused task out of dispatch across process restarts.
ALTER TABLE myforge_task_runs
  DROP CONSTRAINT IF EXISTS ck_myforge_tasks_status;

ALTER TABLE myforge_task_runs
  ADD CONSTRAINT ck_myforge_tasks_status
  CHECK (status IN (
    'queued', 'paused', 'dispatched', 'running',
    'completed', 'completed_with_errors', 'failed', 'cancelled'
  ));
