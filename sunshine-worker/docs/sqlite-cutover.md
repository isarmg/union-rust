# Sunshine SQLite → PostgreSQL cutover

This is an offline, reversible migration. Do not run it while the old Union
process can still modify `external_hosts`.

## Provision once

As a PostgreSQL administrator, create a dedicated login/owner. Use the local
secret-management mechanism rather than placing the password in source code:

```sql
CREATE ROLE sunshine_runtime LOGIN;
CREATE SCHEMA sunshine AUTHORIZATION sunshine_runtime;
REVOKE ALL ON SCHEMA sunshine FROM PUBLIC;
```

The role needs `CONNECT` to the selected database and owns only its schema. Do
not grant it access to `core`, `host_monitoring`, `sentinel`, `photo_backup` or
`public` business tables.

## Import and prove the mapping

1. Stop Union and make a filesystem-level copy of the SQLite database, WAL and
   SHM files. Retain the original Union secret-key material.
2. Set `SUNSHINE_DATABASE_URL`, `SUNSHINE_CREDENTIAL_KEY`,
   `SUNSHINE_CREDENTIAL_KEY_ID`, `UNIONC_SECRET_KEY`,
   `UNIONC_SECRET_KEY_ID` and, when needed, `UNIONC_SECRET_KEY_PREVIOUS`.
3. Run:

   ```bash
   sunshine-worker import-sqlite --sqlite /path/to/union.db
   sunshine-worker verify-import --batch <reported-uuid>
   ```

The source is opened read-only and must pass SQLite `quick_check`. The mapping
preserves host ID, name, address, port, username, logical password, TLS policy,
position and both microsecond timestamps. The report contains a keyed source
fingerprint and an exact destination verification result.

Passwords exist in process memory only for decryption/re-encryption. The
PostgreSQL rollback journal contains destination ciphertext, not plaintext.

## Cut over

Only after `verify-import` reports `exact_match: true`, enable the compiled
Sunshine worker/profile and Union's private gateway adapter. Keep the old
SQLite copy read-only for the rollback window. There must never be two writers.

## Roll back

Stop Union and the worker, then run:

```bash
sunshine-worker rollback-import --batch <reported-uuid>
```

Rollback first compares every current row byte-for-byte with the recorded
import result. If any row was edited after cutover, it refuses to overwrite it.
Otherwise, overwritten rows are restored exactly and newly introduced rows are
deleted in one PostgreSQL transaction. The command then verifies the restored
state and records `rolled_back_at_micros` plus an audit entry. Re-enable the old
Union process only after this reports `exact_match: true`.

