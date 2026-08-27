# Union host-monitoring worker

This is the process-isolated implementation of Union's existing Agent pairing, telemetry and
console-query contract. It is a private Union module, not a standalone product or public service.
The crate is `publish = false`; the supported build path is a Union compile-time profile.

## Runtime boundary

- Default and only accepted bind class: loopback (`127.0.0.1:18105` by default).
- Storage: PostgreSQL schema `host_monitoring`, owned migrations, no Union SQLite or `AppState`.
- Public ingress: Union's static gateway is the sole public listener. The worker rejects every
  route, including health probes, unless all four `gateway-v1` headers match exactly:
  `X-Union-Module-Protocol`, `X-Union-Module-Audience`, `X-Union-Module-Token` and
  `X-Forwarded-Prefix`.
- The fixed audience is `host-monitoring` and the fixed prefix is `/modules/host-monitoring`.
  The per-process token is 64 lowercase hexadecimal characters supplied by Union's supervisor.
- Agent `Authorization: Bearer/Pairing` remains unchanged. Union adds the gateway proof in
  `X-Union-Internal-Credential`, so deployed Agents remain wire compatible.
- Console cookies must be removed by Union. The worker rejects requests containing `Cookie` and
  records the signed credential subject as the audit actor.
- `/health/live` is process liveness; `/health/ready` additionally probes PostgreSQL. Both echo
  `X-Union-Module-Protocol: gateway-v1` and `X-Union-Module-Audience: host-monitoring`.

Union must remove any inbound copies of all four internal headers before writing its own values.
The token is shared only by Union and this worker and must be regenerated for each worker process.

## Offline/admin commands and local contract testing

```console
union-host-monitoring-worker migrate \
  --database-url postgresql://host_monitoring@127.0.0.1/union

UNION_MODULE_PROTOCOL=gateway-v1 \
UNION_MODULE_AUDIENCE=host-monitoring \
UNION_MODULE_TOKEN=<64-lowercase-hex> \
UNION_MODULE_PREFIX=/modules/host-monitoring \
union-host-monitoring-worker serve \
  --database-url postgresql://host_monitoring@127.0.0.1/union
```

The process refuses non-loopback binds and non-PostgreSQL URLs.
The `serve` example is for local contract tests only. Production operators configure
`UNIONC_HOST_MONITORING_DATABASE_URL` on Union; supervisor supplies the bind and all
`UNION_MODULE_*` values.

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

The crate is a root-workspace member selected by `module-host-monitoring`. Union owns the public
routes, generates the process credential, removes untrusted internal headers/cookies and supervises
this binary at `127.0.0.1:18105`. The former in-process monitoring implementation is not an online
fallback. Its frozen SQLite data is usable only through the offline import/verification procedure
above.
