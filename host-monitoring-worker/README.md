# Union host-monitoring worker

This is the process-isolated implementation of Union's existing Agent pairing, telemetry and
console-query contract. It is a private Union module, not a standalone product or public service.
The crate is `publish = false`; Builder packages it independently when the Union release profile
includes the module. Its business code is not linked into Core or Web Shell.

The source package contract is described by `manifest.json`, `permissions.json`,
`config/schema.json`, `version.json`, `frontend/` and the module-owned `migrations/`. Builder
decides whether the package is included in an immutable Union release; Union Runtime decides
whether that already-included private process is enabled. Runtime installation or downloading
new business code is outside this contract.

## Runtime boundary

- Default and only accepted bind class: loopback (`127.0.0.1:18105` by default).
- Storage: a module-owned PostgreSQL database and `host_monitoring` schema with owned migrations;
  no Core database, another module's database, Union SQLite or `AppState` access.
- Public ingress: Union's Manifest-driven gateway is the sole public listener. The worker rejects every
  route, including health probes, unless all four `gateway-v1` headers match exactly:
  `X-Union-Module-Protocol`, `X-Union-Module-Audience`, `X-Union-Module-Token` and
  `X-Forwarded-Prefix`.
- The fixed audience is `host-monitoring` and the fixed prefix is `/api/modules/host-monitoring`.
  The per-process token is 64 lowercase hexadecimal characters supplied by Union's supervisor.
- Agent report, pairing create/read/status and capability activation retain their module-owned
  Bearer/Pairing or one-time-code checks. Union adds its separate per-process gateway proof, so
  deployed Agents never receive the worker credential.
- Browser activation lives at `/modules/host-monitoring/activate/:requestId`. It submits to the
  separate `/agent/v2/activate-admin` platform route protected by Core login,
  `host-monitoring.agents.write` and CSRF, while the worker still verifies and consumes the same
  kind of one-time activation code. Agent/Tray activation continues to use `/agent/v2/activate`
  without requiring a browser session.
- Console cookies must be removed by Union. The worker rejects requests containing `Cookie`,
  requires one canonical `X-Union-Principal`, and records that real operator as the audit actor.
- `/health/live` is process liveness; `/health/ready` additionally probes PostgreSQL. Both echo
  `X-Union-Module-Protocol: gateway-v1` and `X-Union-Module-Audience: host-monitoring`.

Union must remove any inbound copies of all four internal headers before writing its own values.
The token is shared only by Union and this worker and must be regenerated for each worker process.

## Offline/admin commands and local contract testing

```console
union-host-monitoring-worker migrate \
  --database-url postgresql://host_monitoring@127.0.0.1/host_monitoring

UNION_MODULE_PROTOCOL=gateway-v1 \
UNION_MODULE_AUDIENCE=host-monitoring \
UNION_MODULE_TOKEN=<64-lowercase-hex> \
UNION_MODULE_PREFIX=/api/modules/host-monitoring \
union-host-monitoring-worker serve \
  --database-url postgresql://host_monitoring@127.0.0.1/host_monitoring
```

The process refuses non-loopback binds and non-PostgreSQL URLs.
The `serve` example is for local contract tests only. Production operators use the module
configuration center; Runtime supplies `UNION_PLUGIN_BIND`, the legacy bind alias and all
`UNION_MODULE_*` gateway values.

## SQLite cutover and rollback

Stop legacy monitoring writes first and checkpoint/copy the Union SQLite database together with
its WAL. Import into an empty `host_monitoring` domain:

```console
union-host-monitoring-worker import-sqlite \
  --database-url "$UNION_HOST_MONITORING_DATABASE_URL" \
  --sqlite /var/lib/union/unionc.db \
  --evidence ./host-monitoring-import.json
```

The import is one PostgreSQL transaction. Evidence contains the source file SHA-256, deterministic
logical source digests, target JSONB digests, row counts and an `import_id`. It is written with
create-new semantics so existing evidence cannot be overwritten. Revalidate after cutover:

```console
union-host-monitoring-worker verify-import \
  --database-url "$UNION_HOST_MONITORING_DATABASE_URL" \
  --import-id <uuid>
```

Rollback is deliberately refused if imported rows no longer match their recorded target digest;
that prevents deleting post-cutover changes under the guise of rollback. During the no-write
cutover window, rollback removes only rows carrying that `import_id`, leaves the import audit row,
and creates separate evidence:

```console
union-host-monitoring-worker rollback-import \
  --database-url "$UNION_HOST_MONITORING_DATABASE_URL" \
  --import-id <uuid> \
  --evidence ./host-monitoring-rollback.json
```

After accepting writes in PostgreSQL, rollback means a reverse migration or restoring the frozen
SQLite copy; this tool correctly refuses to pretend that new PostgreSQL telemetry can be discarded.

## Union integration

The crate is a root-workspace member assembled through its Manifest-defined package. Union owns
the public routes, generates the process credential, removes untrusted internal headers/cookies and
supervises this binary on its Manifest-reserved loopback endpoint. The former in-process implementation is not an online
fallback. Its frozen SQLite data is usable only through the offline import/verification procedure
above.
