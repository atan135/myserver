-- Logical owner: auth-http
-- Compatibility phase: expand
-- Irreversible risk: none
-- Transaction: required
-- Lock timeout: 5s
-- Statement timeout: 60s
-- Backup point: not-required
-- Recovery command: SQLx rolls back the transaction; correct the permission catalog and rerun db up.

INSERT INTO admin_permissions (
  permission_key, resource, action, risk_level, scope_dimensions, description
) VALUES
  ('activities.read', 'activity', 'read', 'low', ARRAY['target_ids'], 'Read activity definitions and version summaries'),
  ('activities.write', 'activity', 'write', 'medium', ARRAY['target_ids'], 'Create and update unpublished activity drafts'),
  ('activities.publish', 'activity', 'publish', 'high', ARRAY['target_ids'], 'Preflight and publish immutable activity versions'),
  ('activities.offline', 'activity', 'offline', 'high', ARRAY['target_ids'], 'Take a published activity version offline'),
  ('activities.records.read', 'activity', 'read_records', 'medium', ARRAY['target_ids'], 'Read activity claim, draw and reward grant records')
ON CONFLICT (permission_key) DO UPDATE
SET resource = EXCLUDED.resource,
    action = EXCLUDED.action,
    risk_level = EXCLUDED.risk_level,
    scope_dimensions = EXCLUDED.scope_dimensions,
    description = EXCLUDED.description,
    active = true,
    updated_at = current_timestamp;

INSERT INTO admin_role_permissions (role_key, permission_key)
VALUES ('viewer', 'activities.read')
ON CONFLICT DO NOTHING;

INSERT INTO admin_role_permissions (role_key, permission_key)
SELECT role_key, permission_key
FROM (
  VALUES
    ('admin', 'activities.read'),
    ('admin', 'activities.write'),
    ('admin', 'activities.publish'),
    ('admin', 'activities.offline'),
    ('admin', 'activities.records.read'),
    ('super_admin', 'activities.read'),
    ('super_admin', 'activities.write'),
    ('super_admin', 'activities.publish'),
    ('super_admin', 'activities.offline'),
    ('super_admin', 'activities.records.read')
) AS grants(role_key, permission_key)
ON CONFLICT DO NOTHING;
