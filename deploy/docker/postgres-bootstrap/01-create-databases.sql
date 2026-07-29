-- Production bootstrap creates only empty logical databases. SQLx migrations own every schema.
SELECT format('CREATE DATABASE %I', database_name)
FROM (
  VALUES
    ('myserver_auth'),
    ('myserver_game'),
    ('myserver_chat'),
    ('myserver_announce'),
    ('myserver_mail')
) AS required_databases(database_name)
WHERE NOT EXISTS (
  SELECT 1 FROM pg_database WHERE datname = database_name
)
\gexec
