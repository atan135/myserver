INSERT INTO id_origins (origin_id, origin_key, metadata_json)
VALUES (0, 'local-dev', '{"type":"local"}'::jsonb)
ON CONFLICT (origin_id) DO NOTHING;

INSERT INTO worlds (world_id, world_key, active_origin_id, metadata_json)
VALUES (0, 'local-dev', 0, '{"type":"local"}'::jsonb)
ON CONFLICT (world_id) DO NOTHING;

INSERT INTO world_origin_memberships (world_id, origin_id, joined_at)
SELECT 0, 0, current_timestamp
WHERE NOT EXISTS (
  SELECT 1 FROM world_origin_memberships WHERE world_id = 0 AND origin_id = 0
);
