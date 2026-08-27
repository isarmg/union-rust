# UnionC Sunshine worker

This crate is a compile-time Union module and a runtime private process. It is
not an independently supported product, public HTTP service, crate or binary
release. `union-builder` selects and packages it; Union supervises it and is the
only public gateway.

## Boundary

- Default and documented endpoint: `127.0.0.1:18104`.
- Any configured non-loopback bind is rejected.
- Every request, including `/health/live` and `/health/ready`, requires the
  shared `gateway-v1` four-header proof: protocol, audience `sunshine`, the
  process-scoped 64-hex token and prefix `/modules/sunshine`.
- Health responses echo the protocol and audience headers so Union does not
  open its proxy until it has proved the exact worker contract.
- Browser `Cookie` headers are always rejected. Union consumes its own session,
  signs the internal request and strips browser credentials before forwarding.
- The worker owns only PostgreSQL schema `sunshine`, its migration history and
  its encrypted Sunshine upstream passwords. It cannot read Union SQLite,
  sessions or `AppState`.

The worker preserves the existing console route contract under
`/api/services/sunshine/hosts`. This lets Union use a static proxy adapter while
the existing frontend stays unchanged.

## Supervisor-supplied worker environment

```text
SUNSHINE_DATABASE_URL=postgresql://sunshine_runtime:...@127.0.0.1/sarmg_platform
UNION_MODULE_PROTOCOL=gateway-v1
UNION_MODULE_AUDIENCE=sunshine
UNION_MODULE_TOKEN=<64 lowercase hexadecimal characters; supplied by Union>
UNION_MODULE_PREFIX=/modules/sunshine
SUNSHINE_CREDENTIAL_KEY=<base64 32 bytes; module-owned encryption key>
SUNSHINE_CREDENTIAL_KEY_ID=primary
SUNSHINE_BIND=127.0.0.1:18104
SUNSHINE_PRODUCTION=true
```

Operators do not launch this binary with that block. They set
`UNIONC_SUNSHINE_DATABASE_URL`, `UNIONC_SUNSHINE_CREDENTIAL_KEY` and optional
`UNIONC_SUNSHINE_CREDENTIAL_KEY_ID` on Union; supervisor maps the allowlisted values and creates
the gateway identity. The expanded names above document the private process contract.

The PostgreSQL administrator must create schema `sunshine` owned by the module
role. Startup applies only migrations from `migrations/`; it deliberately does
not create roles, databases or schemas.

## Legacy data cutover

See [docs/sqlite-cutover.md](docs/sqlite-cutover.md). The importer decrypts the
legacy row with the supplied Union key, immediately re-encrypts it with the
Sunshine key, and stores no plaintext migration copy. Exact before/after module
ciphertext is retained as a rollback journal.

## Development

This crate is a root-workspace member. Running it directly is only a developer
test; production always obtains its gateway identity and lifecycle from Union:

```bash
cargo fmt -p unionc-sunshine-worker
cargo test -p unionc-sunshine-worker
```
