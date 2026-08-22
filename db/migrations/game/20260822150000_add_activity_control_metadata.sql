-- Logical owner: game-server
-- Compatibility phase: expand
-- Irreversible risk: none
-- Transaction: required
-- Lock timeout: 5s
-- Statement timeout: 60s
-- Backup point: not-required
-- Recovery command: SQLx rolls back the transaction; correct the migration and rerun db up.

-- The admin control plane persists editable metadata beside the same normalized version rows
-- consumed by game-server. These fields do not create a second configuration snapshot.
ALTER TABLE activity_version
  ADD COLUMN IF NOT EXISTS change_reason varchar(512) NOT NULL
    DEFAULT 'legacy activity configuration';

ALTER TABLE activity_stage
  ADD COLUMN IF NOT EXISTS display_json jsonb NOT NULL DEFAULT '{}'::jsonb;

ALTER TABLE activity_stage
  DROP CONSTRAINT IF EXISTS ck_activity_stage_display;
ALTER TABLE activity_stage
  ADD CONSTRAINT ck_activity_stage_display CHECK (jsonb_typeof(display_json) = 'object');

-- The digest covers public/type JSON only. A forked version may intentionally start with the
-- same snapshot, and schedule-only changes also retain the digest, so it is not a version key.
DROP INDEX IF EXISTS uk_activity_version_digest;
CREATE INDEX IF NOT EXISTS idx_activity_version_digest
  ON activity_version (activity_id, config_digest);

ALTER TABLE activity_audit_log
  DROP CONSTRAINT IF EXISTS ck_activity_audit_event_type;
ALTER TABLE activity_audit_log
  ADD CONSTRAINT ck_activity_audit_event_type CHECK (
    event_type IN (
      'draft_created',
      'draft_updated',
      'draft_forked',
      'preflight',
      'published',
      'offlined',
      'records_read',
      'archived',
      'config_changed',
      'reward_changed'
    )
  );
