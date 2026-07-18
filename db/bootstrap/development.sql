\set ON_ERROR_STOP on

SELECT format('CREATE DATABASE %I', dbname)
FROM (
  VALUES
    ('myserver_auth'),
    ('myserver_game'),
    ('myserver_chat'),
    ('myserver_announce'),
    ('myserver_mail')
) AS databases(dbname)
WHERE NOT EXISTS (
  SELECT 1 FROM pg_database WHERE datname = databases.dbname
);
\gexec
