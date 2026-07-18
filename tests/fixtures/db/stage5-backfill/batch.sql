WITH selected AS (
  SELECT id
  FROM stage5_backfill_items
  WHERE id > $1::bigint
    AND copied_at IS NULL
  ORDER BY id
  LIMIT $2
  FOR UPDATE SKIP LOCKED
), updated AS (
  UPDATE stage5_backfill_items AS target
  SET copied_at = clock_timestamp()
  FROM selected
  WHERE target.id = selected.id
  RETURNING target.id
)
SELECT coalesce(max(id), $1::bigint)::text AS next_cursor,
       count(*)::integer AS processed_rows
FROM updated
