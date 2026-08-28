-- Read-only isolation verification. Execute while connected as the module
-- runtime role, once for each of the four PostgreSQL databases.
--
-- Required psql variables:
--   expected_database, expected_owner, expected_runtime, expected_schema,
--   runtime_inherits_owner (true/false)

\set ON_ERROR_STOP on

WITH expected AS (
  SELECT
    :'expected_database'::text AS database_name,
    :'expected_owner'::text AS owner_name,
    :'expected_runtime'::text AS runtime_name,
    :'expected_schema'::text AS schema_name,
    :'runtime_inherits_owner'::boolean AS inherits_owner
),
module_databases(name) AS (
  VALUES
    ('union_sunshine'),
    ('union_host_monitoring'),
    ('union_sentinel_monitor'),
    ('union_photo_backup')
),
module_runtimes(name) AS (
  VALUES
    ('union_sunshine_runtime'),
    ('union_host_monitoring_runtime'),
    ('union_sentinel_monitor_runtime'),
    ('union_photo_backup_runtime')
),
checks(name, ok) AS (
  SELECT 'connected to expected database', current_database() = e.database_name
  FROM expected e
  UNION ALL
  SELECT 'connected as expected runtime role', current_user = e.runtime_name
  FROM expected e
  UNION ALL
  SELECT 'runtime is a constrained LOGIN',
         r.rolcanlogin AND NOT r.rolsuper AND NOT r.rolcreatedb
         AND NOT r.rolcreaterole AND NOT r.rolreplication AND NOT r.rolbypassrls
  FROM expected e JOIN pg_roles r ON r.rolname = e.runtime_name
  UNION ALL
  SELECT 'owner is constrained NOLOGIN',
         NOT r.rolcanlogin AND NOT r.rolsuper AND NOT r.rolcreatedb
         AND NOT r.rolcreaterole AND NOT r.rolreplication AND NOT r.rolbypassrls
  FROM expected e JOIN pg_roles r ON r.rolname = e.owner_name
  UNION ALL
  SELECT 'database has its dedicated owner', d.datdba = e.owner_name::regrole
  FROM expected e JOIN pg_database d ON d.datname = e.database_name
  UNION ALL
  SELECT 'PUBLIC has no database CONNECT',
         NOT EXISTS (
           SELECT FROM aclexplode(COALESCE(d.datacl, acldefault('d', d.datdba))) acl
           WHERE acl.grantee = 0 AND acl.privilege_type = 'CONNECT'
         )
  FROM expected e JOIN pg_database d ON d.datname = e.database_name
  UNION ALL
  SELECT 'all four dedicated databases exist',
         (SELECT count(*) FROM pg_database d JOIN module_databases m ON m.name = d.datname) = 4
  UNION ALL
  SELECT 'runtime cannot CONNECT to peer module databases',
         COALESCE(bool_and(NOT has_database_privilege(e.runtime_name, d.datname, 'CONNECT')), true)
  FROM expected e
  JOIN pg_database d ON d.datname <> e.database_name
  JOIN module_databases m ON m.name = d.datname
  UNION ALL
  SELECT 'peer runtime roles cannot CONNECT to this database',
         COALESCE(bool_and(NOT has_database_privilege(r.rolname, e.database_name, 'CONNECT')), true)
  FROM expected e
  JOIN pg_roles r ON r.rolname <> e.runtime_name
  JOIN module_runtimes m ON m.name = r.rolname
  GROUP BY e.database_name
  UNION ALL
  SELECT 'migration schema has the expected owner',
         n.nspowner = (
           CASE WHEN e.inherits_owner THEN e.owner_name ELSE e.runtime_name END
         )::regrole
  FROM expected e JOIN pg_namespace n ON n.nspname = e.schema_name
  UNION ALL
  SELECT 'PUBLIC has no migration-schema privileges',
         NOT EXISTS (
           SELECT FROM aclexplode(COALESCE(n.nspacl, acldefault('n', n.nspowner))) acl
           WHERE acl.grantee = 0
         )
  FROM expected e JOIN pg_namespace n ON n.nspname = e.schema_name
  UNION ALL
  SELECT 'runtime can run migrations only in its selected schema',
         has_schema_privilege(e.runtime_name, e.schema_name, 'USAGE')
         AND has_schema_privilege(e.runtime_name, e.schema_name, 'CREATE')
         AND (
           e.schema_name = 'public'
           OR (
             NOT has_schema_privilege(e.runtime_name, 'public', 'USAGE')
             AND NOT has_schema_privilege(e.runtime_name, 'public', 'CREATE')
           )
         )
  FROM expected e
  UNION ALL
  SELECT 'owner membership matches the schema mode',
         pg_has_role(e.runtime_name, e.owner_name, 'MEMBER') = e.inherits_owner
  FROM expected e
)
SELECT
  bool_and(ok) AS isolation_ok,
  jsonb_object_agg(name, ok ORDER BY name)::text AS check_results
FROM checks
\gset verification_

\echo :verification_check_results
\if :verification_isolation_ok
  \echo 'Union module database isolation: PASS'
\else
  \warn 'Union module database isolation: FAIL'
  -- ON_ERROR_STOP turns this deliberate read-only assertion failure into a
  -- non-zero psql exit status without creating any database object.
  SELECT 1 / 0 AS isolation_verification_failed;
\endif
