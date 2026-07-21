-- Canonical input for SHA-256 catalog fingerprints. The caller must sort the
-- object_kind, object_name and definition fields before hashing UTF-8 JSON.
-- object_identity is for drift comparison only and is not part of the baseline hash.
SELECT 'table' AS object_kind,
       format('%I.%I', n.nspname, c.relname) AS object_name,
       format('%I.%I', n.nspname, c.relname) AS object_identity,
       c.relkind::text AS definition
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = 'public'
  AND c.relkind IN ('r', 'p')
  AND c.relname NOT IN ('_sqlx_migrations', '_myserver_migration_audit', '_myserver_backfill_state', '_myserver_backfill_audit')
UNION ALL
SELECT 'column' AS object_kind,
       format('%I.%I', n.nspname, c.relname) AS object_name,
       format('%I.%I.%I', n.nspname, c.relname, a.attname) AS object_identity,
       format('%I %s %s %s', a.attname, pg_catalog.format_type(a.atttypid, a.atttypmod),
         CASE WHEN a.attnotnull THEN 'not null' ELSE 'null' END,
         coalesce(pg_get_expr(ad.adbin, ad.adrelid), '')) AS definition
FROM pg_attribute a
JOIN pg_class c ON c.oid = a.attrelid
JOIN pg_namespace n ON n.oid = c.relnamespace
LEFT JOIN pg_attrdef ad ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum
WHERE n.nspname = 'public'
  AND c.relkind IN ('r', 'p')
  AND c.relname NOT IN ('_sqlx_migrations', '_myserver_migration_audit', '_myserver_backfill_state', '_myserver_backfill_audit')
  AND a.attnum > 0
  AND NOT a.attisdropped
UNION ALL
SELECT 'constraint' AS object_kind, format('%I.%I', n.nspname, c.relname) AS object_name, format('%I.%I.%I', n.nspname, c.relname, con.conname) AS object_identity, pg_get_constraintdef(con.oid, true) AS definition
FROM pg_constraint con
JOIN pg_class c ON c.oid = con.conrelid
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = 'public'
  AND c.relname NOT IN ('_sqlx_migrations', '_myserver_migration_audit', '_myserver_backfill_state', '_myserver_backfill_audit')
UNION ALL
SELECT 'index' AS object_kind, format('%I.%I', n.nspname, i.relname) AS object_name, format('%I.%I', n.nspname, i.relname) AS object_identity, pg_get_indexdef(i.oid) AS definition
FROM pg_index ix
JOIN pg_class c ON c.oid = ix.indrelid
JOIN pg_class i ON i.oid = ix.indexrelid
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = 'public'
  AND c.relname NOT IN ('_sqlx_migrations', '_myserver_migration_audit', '_myserver_backfill_state', '_myserver_backfill_audit')
UNION ALL
SELECT 'trigger' AS object_kind, format('%I.%I', n.nspname, c.relname) AS object_name, format('%I.%I.%I', n.nspname, c.relname, t.tgname) AS object_identity, pg_get_triggerdef(t.oid, true) AS definition
FROM pg_trigger t
JOIN pg_class c ON c.oid = t.tgrelid
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = 'public'
  AND c.relname NOT IN ('_sqlx_migrations', '_myserver_migration_audit', '_myserver_backfill_state', '_myserver_backfill_audit')
  AND NOT t.tgisinternal
UNION ALL
SELECT 'function' AS object_kind, format('%I.%I(%s)', n.nspname, p.proname, pg_get_function_identity_arguments(p.oid)) AS object_name, format('%I.%I(%s)', n.nspname, p.proname, pg_get_function_identity_arguments(p.oid)) AS object_identity, pg_get_functiondef(p.oid) AS definition
FROM pg_proc p
JOIN pg_namespace n ON n.oid = p.pronamespace
WHERE n.nspname = 'public';
