-- Logical owner: auth-http
-- Compatibility phase: expand
-- Irreversible risk: none
-- Transaction: required
-- Lock timeout: 5s
-- Statement timeout: 60s
-- Backup point: not-required
-- Recovery command: stop control-plane writers, then remove only unreferenced operation-protocol objects in a reviewed follow-up migration.
-- admin-api owns the protocol service. This migration deliberately stores hashes and
-- minimal summaries instead of raw control-plane payloads or plaintext preflight nonces.

CREATE TABLE IF NOT EXISTS admin_operation_requests (
  operation_id uuid PRIMARY KEY,
  request_id varchar(128) NOT NULL UNIQUE,
  actor_admin_id bigint NOT NULL REFERENCES admin_accounts(id),
  actor_subject varchar(128) NOT NULL,
  permission_key varchar(128) NOT NULL REFERENCES admin_permissions(permission_key),
  risk_level varchar(16) NOT NULL,
  authorization_scope_json jsonb NOT NULL,
  requested_scope_json jsonb NOT NULL,
  scope_sha256 char(64) NOT NULL,
  target_summary_json jsonb NOT NULL,
  target_sha256 char(64) NOT NULL,
  payload_sha256 char(64) NOT NULL,
  semantic_sha256 char(64) NOT NULL,
  reason varchar(512) NOT NULL,
  trace_id varchar(128) NOT NULL,
  status varchar(32) NOT NULL,
  approval_status varchar(16) NOT NULL,
  execution_claimed_at timestamptz NULL,
  completed_at timestamptz NULL,
  result_summary_json jsonb NULL,
  error_summary_json jsonb NULL,
  created_at timestamptz NOT NULL DEFAULT current_timestamp,
  updated_at timestamptz NOT NULL DEFAULT current_timestamp,
  CONSTRAINT ck_admin_operation_request_id CHECK (request_id ~ '^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$'),
  CONSTRAINT ck_admin_operation_actor_subject CHECK (btrim(actor_subject) <> ''),
  CONSTRAINT ck_admin_operation_risk CHECK (risk_level IN ('low', 'medium', 'high', 'emergency')),
  CONSTRAINT ck_admin_operation_authorization_scope CHECK (admin_policy_scope_is_valid(authorization_scope_json)),
  CONSTRAINT ck_admin_operation_requested_scope CHECK (jsonb_typeof(requested_scope_json) = 'object'),
  CONSTRAINT ck_admin_operation_target_summary CHECK (jsonb_typeof(target_summary_json) = 'object'),
  CONSTRAINT ck_admin_operation_reason CHECK (btrim(reason) <> ''),
  CONSTRAINT ck_admin_operation_trace_id CHECK (trace_id ~ '^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$'),
  CONSTRAINT ck_admin_operation_status CHECK (status IN (
    'preflighted', 'approved', 'executing', 'succeeded', 'failed', 'execution_uncertain', 'cancelled'
  )),
  CONSTRAINT ck_admin_operation_approval_status CHECK (approval_status IN ('not_required', 'pending', 'approved', 'rejected')),
  CONSTRAINT ck_admin_operation_hashes CHECK (
    scope_sha256 ~ '^[0-9a-f]{64}$'
    AND target_sha256 ~ '^[0-9a-f]{64}$'
    AND payload_sha256 ~ '^[0-9a-f]{64}$'
    AND semantic_sha256 ~ '^[0-9a-f]{64}$'
  ),
  CONSTRAINT ck_admin_operation_result_summary CHECK (result_summary_json IS NULL OR jsonb_typeof(result_summary_json) = 'object'),
  CONSTRAINT ck_admin_operation_error_summary CHECK (error_summary_json IS NULL OR jsonb_typeof(error_summary_json) = 'object'),
  CONSTRAINT ck_admin_operation_completion CHECK (
    (status IN ('preflighted', 'approved', 'executing') AND completed_at IS NULL)
    OR (status IN ('succeeded', 'failed', 'execution_uncertain', 'cancelled') AND completed_at IS NOT NULL)
  )
);

CREATE INDEX IF NOT EXISTS idx_admin_operation_requests_actor_created
  ON admin_operation_requests (actor_admin_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_admin_operation_requests_status_created
  ON admin_operation_requests (status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_admin_operation_requests_permission_created
  ON admin_operation_requests (permission_key, created_at DESC);

CREATE TABLE IF NOT EXISTS admin_operation_previews (
  preview_id uuid PRIMARY KEY,
  operation_id uuid NOT NULL UNIQUE REFERENCES admin_operation_requests(operation_id),
  nonce_sha256 char(64) NOT NULL UNIQUE,
  impact_summary_json jsonb NOT NULL,
  summary_sha256 char(64) NOT NULL,
  target_sha256 char(64) NOT NULL,
  payload_sha256 char(64) NOT NULL,
  expires_at timestamptz NOT NULL,
  consumed_at timestamptz NULL,
  created_at timestamptz NOT NULL DEFAULT current_timestamp,
  CONSTRAINT ck_admin_operation_preview_summary CHECK (jsonb_typeof(impact_summary_json) = 'object'),
  CONSTRAINT ck_admin_operation_preview_hashes CHECK (
    nonce_sha256 ~ '^[0-9a-f]{64}$'
    AND summary_sha256 ~ '^[0-9a-f]{64}$'
    AND target_sha256 ~ '^[0-9a-f]{64}$'
    AND payload_sha256 ~ '^[0-9a-f]{64}$'
  ),
  CONSTRAINT ck_admin_operation_preview_expiry CHECK (expires_at > created_at)
);

CREATE INDEX IF NOT EXISTS idx_admin_operation_previews_expiry
  ON admin_operation_previews (expires_at)
  WHERE consumed_at IS NULL;

CREATE TABLE IF NOT EXISTS admin_operation_approvals (
  operation_id uuid PRIMARY KEY REFERENCES admin_operation_requests(operation_id),
  status varchar(16) NOT NULL,
  requested_at timestamptz NOT NULL DEFAULT current_timestamp,
  decided_at timestamptz NULL,
  decided_by_admin_id bigint NULL REFERENCES admin_accounts(id),
  decided_by_subject varchar(128) NULL,
  evidence_summary_json jsonb NOT NULL DEFAULT '{}'::jsonb,
  rejection_reason varchar(512) NULL,
  updated_at timestamptz NOT NULL DEFAULT current_timestamp,
  CONSTRAINT ck_admin_operation_approval_status CHECK (status IN ('not_required', 'pending', 'approved', 'rejected')),
  CONSTRAINT ck_admin_operation_approval_evidence CHECK (jsonb_typeof(evidence_summary_json) = 'object'),
  CONSTRAINT ck_admin_operation_approval_decision CHECK (
    (status IN ('not_required', 'pending') AND decided_at IS NULL AND decided_by_admin_id IS NULL AND decided_by_subject IS NULL AND rejection_reason IS NULL)
    OR (status = 'approved' AND decided_at IS NOT NULL AND decided_by_subject IS NOT NULL AND btrim(decided_by_subject) <> '' AND rejection_reason IS NULL)
    OR (status = 'rejected' AND decided_at IS NOT NULL AND decided_by_subject IS NOT NULL AND btrim(decided_by_subject) <> '' AND rejection_reason IS NOT NULL AND btrim(rejection_reason) <> '')
  )
);

CREATE INDEX IF NOT EXISTS idx_admin_operation_approvals_pending
  ON admin_operation_approvals (requested_at)
  WHERE status = 'pending';

CREATE TABLE IF NOT EXISTS admin_breakglass_grants (
  grant_id uuid PRIMARY KEY,
  activation_request_id varchar(128) NOT NULL UNIQUE,
  actor_admin_id bigint NOT NULL REFERENCES admin_accounts(id),
  actor_subject varchar(128) NOT NULL,
  permission_key varchar(128) NOT NULL REFERENCES admin_permissions(permission_key),
  scope_json jsonb NOT NULL,
  scope_sha256 char(64) NOT NULL,
  target_summary_json jsonb NOT NULL,
  target_sha256 char(64) NOT NULL,
  semantic_sha256 char(64) NOT NULL,
  reason varchar(512) NOT NULL,
  activated_at timestamptz NOT NULL DEFAULT current_timestamp,
  expires_at timestamptz NOT NULL,
  revoked_at timestamptz NULL,
  revoked_by_admin_id bigint NULL REFERENCES admin_accounts(id),
  revoked_by_subject varchar(128) NULL,
  revocation_reason varchar(512) NULL,
  CONSTRAINT ck_admin_breakglass_request_id CHECK (activation_request_id ~ '^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$'),
  CONSTRAINT ck_admin_breakglass_actor_subject CHECK (btrim(actor_subject) <> ''),
  CONSTRAINT ck_admin_breakglass_scope CHECK (admin_policy_scope_is_valid(scope_json) AND scope_sha256 ~ '^[0-9a-f]{64}$'),
  CONSTRAINT ck_admin_breakglass_target CHECK (
    jsonb_typeof(target_summary_json) = 'object'
    AND target_sha256 ~ '^[0-9a-f]{64}$'
    AND semantic_sha256 ~ '^[0-9a-f]{64}$'
  ),
  CONSTRAINT ck_admin_breakglass_reason CHECK (btrim(reason) <> ''),
  CONSTRAINT ck_admin_breakglass_ttl CHECK (expires_at > activated_at AND expires_at <= activated_at + interval '15 minutes'),
  CONSTRAINT ck_admin_breakglass_revocation CHECK (
    (revoked_at IS NULL AND revoked_by_admin_id IS NULL AND revoked_by_subject IS NULL AND revocation_reason IS NULL)
    OR (revoked_at IS NOT NULL AND revoked_by_subject IS NOT NULL AND btrim(revoked_by_subject) <> '' AND revocation_reason IS NOT NULL AND btrim(revocation_reason) <> '')
  )
);

CREATE INDEX IF NOT EXISTS idx_admin_breakglass_active
  ON admin_breakglass_grants (actor_admin_id, permission_key, expires_at)
  WHERE revoked_at IS NULL;

CREATE TABLE IF NOT EXISTS admin_operation_audit_events (
  id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
  operation_id uuid NULL REFERENCES admin_operation_requests(operation_id),
  breakglass_grant_id uuid NULL REFERENCES admin_breakglass_grants(grant_id),
  event_type varchar(64) NOT NULL,
  actor_admin_id bigint NULL REFERENCES admin_accounts(id),
  actor_subject varchar(128) NOT NULL,
  request_id varchar(128) NULL,
  permission_key varchar(128) NULL REFERENCES admin_permissions(permission_key),
  risk_level varchar(16) NULL,
  trace_id varchar(128) NULL,
  reason varchar(512) NOT NULL,
  target_summary_json jsonb NULL,
  result_summary_json jsonb NULL,
  details_json jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT current_timestamp,
  CONSTRAINT ck_admin_operation_audit_event_type CHECK (event_type IN (
    'preflight_created', 'approval_approved', 'approval_rejected', 'execution_claimed',
    'execution_succeeded', 'execution_failed', 'execution_uncertain', 'execution_cancelled',
    'breakglass_activated', 'breakglass_revoked'
  )),
  CONSTRAINT ck_admin_operation_audit_actor_subject CHECK (btrim(actor_subject) <> ''),
  CONSTRAINT ck_admin_operation_audit_request_id CHECK (request_id IS NULL OR request_id ~ '^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$'),
  CONSTRAINT ck_admin_operation_audit_risk CHECK (risk_level IS NULL OR risk_level IN ('low', 'medium', 'high', 'emergency')),
  CONSTRAINT ck_admin_operation_audit_trace_id CHECK (trace_id IS NULL OR trace_id ~ '^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$'),
  CONSTRAINT ck_admin_operation_audit_reason CHECK (btrim(reason) <> ''),
  CONSTRAINT ck_admin_operation_audit_target CHECK (target_summary_json IS NULL OR jsonb_typeof(target_summary_json) = 'object'),
  CONSTRAINT ck_admin_operation_audit_result CHECK (result_summary_json IS NULL OR jsonb_typeof(result_summary_json) = 'object'),
  CONSTRAINT ck_admin_operation_audit_details CHECK (jsonb_typeof(details_json) = 'object')
);

CREATE INDEX IF NOT EXISTS idx_admin_operation_audit_operation
  ON admin_operation_audit_events (operation_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_admin_operation_audit_actor
  ON admin_operation_audit_events (actor_admin_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_admin_operation_audit_request
  ON admin_operation_audit_events (request_id, created_at DESC);

CREATE OR REPLACE FUNCTION reject_admin_operation_audit_mutation()
RETURNS trigger AS $$
BEGIN
  RAISE EXCEPTION 'admin_operation_audit_events is append-only';
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_admin_operation_audit_immutable ON admin_operation_audit_events;
CREATE TRIGGER trg_admin_operation_audit_immutable
  BEFORE UPDATE OR DELETE ON admin_operation_audit_events
  FOR EACH ROW EXECUTE FUNCTION reject_admin_operation_audit_mutation();

DROP TRIGGER IF EXISTS trg_admin_operation_audit_no_truncate ON admin_operation_audit_events;
CREATE TRIGGER trg_admin_operation_audit_no_truncate
  BEFORE TRUNCATE ON admin_operation_audit_events
  FOR EACH STATEMENT EXECUTE FUNCTION reject_admin_operation_audit_mutation();

REVOKE UPDATE, DELETE, TRUNCATE ON admin_operation_audit_events FROM PUBLIC;

-- Service shutdown was not in the first permission seed. It is deliberately emergency-only,
-- excluded from the ordinary admin role, and becomes usable only through a direct grant or
-- a narrowly scoped break-glass grant.
INSERT INTO admin_permissions (
  permission_key, resource, action, risk_level, scope_dimensions, description
) VALUES (
  'service.shutdown', 'service', 'shutdown', 'emergency', ARRAY['service_names', 'instance_ids'],
  'Shut down a registered service instance'
)
ON CONFLICT (permission_key) DO UPDATE
SET resource = EXCLUDED.resource,
    action = EXCLUDED.action,
    risk_level = EXCLUDED.risk_level,
    scope_dimensions = EXCLUDED.scope_dimensions,
    description = EXCLUDED.description,
    active = true,
    updated_at = current_timestamp;

INSERT INTO admin_role_permissions (role_key, permission_key)
VALUES ('super_admin', 'service.shutdown')
ON CONFLICT DO NOTHING;
