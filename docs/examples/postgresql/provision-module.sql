-- Provision one Union PostgreSQL module database.
--
-- Run as a controlled PostgreSQL cluster superuser with ON_ERROR_STOP. Required psql
-- variables:
--   module_database, module_owner, module_runtime, module_schema,
--   schema_owner, runtime_inherits_owner (true/false)
--
-- This template deliberately creates the LOGIN role with PASSWORD NULL. Set or
-- rotate its credential afterwards with psql's hidden \password prompt or an
-- approved secret-manager integration. Never commit a password to this file.

\set ON_ERROR_STOP on
\set ECHO errors

SELECT format(
  'CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS',
  :'module_owner'
)
WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = :'module_owner')
\gexec

SELECT format(
  'ALTER ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS',
  :'module_owner'
)
\gexec

SELECT format(
  'CREATE ROLE %I LOGIN INHERIT PASSWORD NULL NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS',
  :'module_runtime'
)
WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = :'module_runtime')
\gexec

SELECT format(
  'ALTER ROLE %I LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS',
  :'module_runtime'
)
\gexec

SELECT format(
  'CREATE DATABASE %I OWNER %I ENCODING %L TEMPLATE template0',
  :'module_database',
  :'module_owner',
  'UTF8'
)
WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = :'module_database')
\gexec

SELECT format('ALTER DATABASE %I OWNER TO %I', :'module_database', :'module_owner')
\gexec
SELECT format('REVOKE ALL ON DATABASE %I FROM PUBLIC', :'module_database')
\gexec

-- Remove CONNECT/TEMPORARY from every other non-superuser LOGIN. This closes
-- stale ACLs if the template is reapplied to an existing dedicated database.
SELECT format('REVOKE ALL ON DATABASE %I FROM %I', :'module_database', rolname)
FROM pg_roles
WHERE rolcanlogin
  AND NOT rolsuper
  AND rolname <> :'module_runtime'
\gexec

SELECT format('GRANT CONNECT ON DATABASE %I TO %I', :'module_database', :'module_runtime')
\gexec

\if :runtime_inherits_owner
  SELECT format('GRANT %I TO %I', :'module_owner', :'module_runtime') \gexec
\else
  SELECT format('REVOKE %I FROM %I', :'module_owner', :'module_runtime') \gexec
\endif

\connect :module_database

-- The database owner guards public. Sunshine and Host use a separate named
-- schema owned by their runtime role. Sentinel and Photo intentionally pass
-- module_schema=public, schema_owner=<module_owner>, runtime_inherits_owner=true.
SELECT format('ALTER SCHEMA public OWNER TO %I', :'module_owner') \gexec
REVOKE ALL ON SCHEMA public FROM PUBLIC;

SELECT format('CREATE SCHEMA IF NOT EXISTS %I AUTHORIZATION %I', :'module_schema', :'schema_owner')
\gexec
SELECT format('ALTER SCHEMA %I OWNER TO %I', :'module_schema', :'schema_owner')
\gexec
SELECT format('REVOKE ALL ON SCHEMA %I FROM PUBLIC', :'module_schema')
\gexec

-- Remove direct schema ACLs for other non-superuser LOGIN roles. The selected
-- runtime reaches Sentinel/Photo public only through its own NOLOGIN owner role.
SELECT format('REVOKE ALL ON SCHEMA %I FROM %I', :'module_schema', rolname)
FROM pg_roles
WHERE rolcanlogin
  AND NOT rolsuper
  AND rolname <> :'module_runtime'
\gexec

SELECT format('GRANT USAGE, CREATE ON SCHEMA %I TO %I', :'module_schema', :'schema_owner')
\gexec

\if :runtime_inherits_owner
\else
  SELECT format('REVOKE ALL ON SCHEMA public FROM %I', :'module_runtime') \gexec
\endif

SELECT format(
  'ALTER ROLE %I IN DATABASE %I SET search_path = %I, pg_catalog',
  :'module_runtime',
  :'module_database',
  :'module_schema'
)
\gexec

SELECT format('REVOKE ALL ON ALL TABLES IN SCHEMA %I FROM PUBLIC', :'module_schema') \gexec
SELECT format('REVOKE ALL ON ALL SEQUENCES IN SCHEMA %I FROM PUBLIC', :'module_schema') \gexec
SELECT format('REVOKE EXECUTE ON ALL FUNCTIONS IN SCHEMA %I FROM PUBLIC', :'module_schema') \gexec
SELECT format(
  'ALTER DEFAULT PRIVILEGES FOR ROLE %I IN SCHEMA %I REVOKE ALL ON TABLES FROM PUBLIC',
  :'module_runtime',
  :'module_schema'
) \gexec
SELECT format(
  'ALTER DEFAULT PRIVILEGES FOR ROLE %I IN SCHEMA %I REVOKE ALL ON SEQUENCES FROM PUBLIC',
  :'module_runtime',
  :'module_schema'
) \gexec
SELECT format(
  'ALTER DEFAULT PRIVILEGES FOR ROLE %I IN SCHEMA %I REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC',
  :'module_runtime',
  :'module_schema'
) \gexec

SELECT
  current_database() AS provisioned_database,
  :'module_owner' AS no_login_owner,
  :'module_runtime' AS runtime_login,
  :'module_schema' AS migration_schema,
  (SELECT rolpassword IS NULL FROM pg_authid WHERE rolname = :'module_runtime')
    AS credential_still_disabled;
