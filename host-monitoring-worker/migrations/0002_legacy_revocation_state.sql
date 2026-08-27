-- Preserve security state imported from Union's former revocation lifecycle.
ALTER TABLE host_monitoring.monitored_hosts
    ADD COLUMN IF NOT EXISTS lifecycle_status text NOT NULL DEFAULT 'active'
        CHECK (lifecycle_status IN ('active', 'revoked')),
    ADD COLUMN IF NOT EXISTS revoked_at timestamptz;

ALTER TABLE host_monitoring.agent_credentials
    ADD COLUMN IF NOT EXISTS kind text NOT NULL DEFAULT 'pairing'
        CHECK (kind IN ('legacy', 'pairing')),
    ADD COLUMN IF NOT EXISTS revoked_at timestamptz;

CREATE INDEX IF NOT EXISTS agent_credentials_active_token
    ON host_monitoring.agent_credentials(token_hash) WHERE revoked_at IS NULL;
