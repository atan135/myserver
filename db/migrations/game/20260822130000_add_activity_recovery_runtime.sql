-- Logical owner: game-server
-- Compatibility phase: expand
-- Irreversible risk: none
-- Transaction: required
-- Lock timeout: 5s
-- Statement timeout: 60s
-- Backup point: not-required
-- Recovery command: SQLx rolls back the transaction; correct the migration and rerun db up.

ALTER TABLE activity_claim_record
  ADD COLUMN IF NOT EXISTS reward_request_id varchar(128) NULL,
  ADD COLUMN IF NOT EXISTS runtime_key varchar(320) NULL,
  ADD COLUMN IF NOT EXISTS order_snapshot_json jsonb NOT NULL DEFAULT '{}'::jsonb,
  ADD COLUMN IF NOT EXISTS result_json jsonb NOT NULL DEFAULT 'null'::jsonb,
  ADD COLUMN IF NOT EXISTS notification_failed boolean NOT NULL DEFAULT false,
  ADD COLUMN IF NOT EXISTS attempt_count integer NOT NULL DEFAULT 0,
  ADD COLUMN IF NOT EXISTS updated_at timestamptz NOT NULL DEFAULT current_timestamp;

ALTER TABLE activity_claim_record
  DROP CONSTRAINT IF EXISTS ck_activity_claim_status;
ALTER TABLE activity_claim_record
  ADD CONSTRAINT ck_activity_claim_status CHECK (
    status IN (
      'processing',
      'granted',
      'retryable_failure',
      'permanent_failure',
      'reconciliation_pending',
      'blocked_capacity',
      'manual_review'
    )
  );

-- Rows created before durable order snapshots cannot be retried safely. Keep them visible for
-- operator inspection, but never infer or reconstruct a reward order from partial legacy data.
UPDATE activity_claim_record
SET status = 'manual_review',
    error_code = COALESCE(error_code, 'ACTIVITY_LEGACY_CLAIM_REQUIRES_REVIEW'),
    updated_at = current_timestamp
WHERE reward_request_id IS NULL
  AND order_snapshot_json = '{}'::jsonb;

ALTER TABLE activity_claim_record
  DROP CONSTRAINT IF EXISTS ck_activity_claim_order_snapshot;
ALTER TABLE activity_claim_record
  ADD CONSTRAINT ck_activity_claim_order_snapshot CHECK (
    jsonb_typeof(order_snapshot_json) = 'object'
    AND (status = 'manual_review' OR order_snapshot_json <> '{}'::jsonb)
  );
ALTER TABLE activity_claim_record
  DROP CONSTRAINT IF EXISTS ck_activity_claim_result;
ALTER TABLE activity_claim_record
  ADD CONSTRAINT ck_activity_claim_result CHECK (
    jsonb_typeof(result_json) IN ('object', 'null')
  );
ALTER TABLE activity_claim_record
  DROP CONSTRAINT IF EXISTS ck_activity_claim_attempt_count;
ALTER TABLE activity_claim_record
  ADD CONSTRAINT ck_activity_claim_attempt_count CHECK (attempt_count >= 0);

CREATE INDEX IF NOT EXISTS idx_activity_claim_recovery
  ON activity_claim_record (status, updated_at)
  WHERE status IN (
    'processing',
    'retryable_failure',
    'reconciliation_pending',
    'blocked_capacity',
    'manual_review'
  );

CREATE UNIQUE INDEX IF NOT EXISTS uk_activity_claim_runtime_key
  ON activity_claim_record (runtime_key)
  WHERE runtime_key IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS uk_activity_claim_active_draw
  ON activity_claim_record (character_id, activity_id, version_no)
  WHERE action_type = 'draw'
    AND status IN ('processing', 'retryable_failure', 'reconciliation_pending');

ALTER TABLE reward_grant_ledger
  DROP CONSTRAINT IF EXISTS ck_reward_grant_ledger_status;
ALTER TABLE reward_grant_ledger
  ADD CONSTRAINT ck_reward_grant_ledger_status CHECK (
    status IN (
      'pending',
      'granted',
      'retryable_failure',
      'permanent_failure',
      'reconciliation_pending',
      'blocked_capacity',
      'manual_review'
    )
  );

ALTER TABLE reward_mail_outbox
  ADD COLUMN IF NOT EXISTS attempt_count integer NOT NULL DEFAULT 0,
  ADD COLUMN IF NOT EXISTS next_attempt_at timestamptz NOT NULL DEFAULT current_timestamp,
  ADD COLUMN IF NOT EXISTS lease_owner varchar(128) NULL,
  ADD COLUMN IF NOT EXISTS lease_expires_at timestamptz NULL,
  ADD COLUMN IF NOT EXISTS last_error_code varchar(64) NULL,
  ADD COLUMN IF NOT EXISTS response_json jsonb NOT NULL DEFAULT 'null'::jsonb,
  ADD COLUMN IF NOT EXISTS delivered_at timestamptz NULL,
  ADD COLUMN IF NOT EXISTS updated_at timestamptz NOT NULL DEFAULT current_timestamp;

ALTER TABLE reward_mail_outbox
  DROP CONSTRAINT IF EXISTS ck_reward_mail_outbox_status;
ALTER TABLE reward_mail_outbox
  ADD CONSTRAINT ck_reward_mail_outbox_status CHECK (
    status IN (
      'pending',
      'processing',
      'retryable_failure',
      'delivered',
      'permanent_failure',
      'manual_review'
    )
  );
ALTER TABLE reward_mail_outbox
  DROP CONSTRAINT IF EXISTS ck_reward_mail_outbox_attempt_count;
ALTER TABLE reward_mail_outbox
  ADD CONSTRAINT ck_reward_mail_outbox_attempt_count CHECK (attempt_count >= 0);
ALTER TABLE reward_mail_outbox
  DROP CONSTRAINT IF EXISTS ck_reward_mail_outbox_response;
ALTER TABLE reward_mail_outbox
  ADD CONSTRAINT ck_reward_mail_outbox_response CHECK (
    jsonb_typeof(response_json) IN ('object', 'null')
  );

CREATE INDEX IF NOT EXISTS idx_reward_mail_outbox_dispatch
  ON reward_mail_outbox (next_attempt_at, created_at)
  WHERE status IN ('pending', 'processing', 'retryable_failure');

CREATE TABLE IF NOT EXISTS activity_claim_review (
  id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
  character_id varchar(128) NOT NULL,
  activity_id varchar(64) NOT NULL,
  version_no integer NOT NULL,
  semantic_claim_key varchar(320) NOT NULL,
  client_request_id varchar(128) NULL,
  reason_code varchar(64) NOT NULL,
  order_snapshot_json jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT current_timestamp,
  CONSTRAINT fk_activity_claim_review_version FOREIGN KEY (activity_id, version_no)
    REFERENCES activity_version(activity_id, version_no),
  CONSTRAINT ck_activity_claim_review_order CHECK (jsonb_typeof(order_snapshot_json) = 'object')
);

CREATE INDEX IF NOT EXISTS idx_activity_claim_review_lookup
  ON activity_claim_review (character_id, activity_id, version_no, created_at DESC);

CREATE UNIQUE INDEX IF NOT EXISTS uk_activity_claim_review_request_reason
  ON activity_claim_review (
    character_id,
    activity_id,
    version_no,
    semantic_claim_key,
    client_request_id,
    reason_code
  )
  WHERE client_request_id IS NOT NULL;
