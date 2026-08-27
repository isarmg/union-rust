CREATE TABLE monitored_hosts (
    host_id                 UUID PRIMARY KEY,
    name                    TEXT NOT NULL CHECK (char_length(btrim(name)) BETWEEN 1 AND 255),
    os                      TEXT NOT NULL CHECK (char_length(btrim(os)) BETWEEN 1 AND 64),
    os_version              TEXT,
    kernel_version          TEXT,
    arch                    TEXT NOT NULL CHECK (char_length(btrim(arch)) BETWEEN 1 AND 64),
    agent_version           TEXT NOT NULL CHECK (char_length(btrim(agent_version)) BETWEEN 1 AND 128),
    capabilities            JSONB NOT NULL DEFAULT '[]'::JSONB CHECK (jsonb_typeof(capabilities) = 'array'),
    registered_at           BIGINT NOT NULL DEFAULT ((extract(epoch FROM clock_timestamp()) * 1000000)::BIGINT),
    last_seen_at            BIGINT NOT NULL DEFAULT ((extract(epoch FROM clock_timestamp()) * 1000000)::BIGINT),
    latest_report_id        UUID,
    latest_collected_at     BIGINT,
    latest_interval_seconds DOUBLE PRECISION
);

CREATE INDEX monitored_hosts_last_seen_at ON monitored_hosts(last_seen_at DESC);

CREATE TABLE metric_reports (
    report_id                            UUID PRIMARY KEY,
    host_id                              UUID NOT NULL REFERENCES monitored_hosts(host_id) ON DELETE CASCADE,
    schema_version                       BIGINT NOT NULL CHECK (schema_version > 0),
    collected_at                         BIGINT NOT NULL,
    received_at                          BIGINT NOT NULL DEFAULT ((extract(epoch FROM clock_timestamp()) * 1000000)::BIGINT),
    interval_seconds                     DOUBLE PRECISION NOT NULL CHECK (interval_seconds > 0 AND interval_seconds <= 3600),
    payload                              JSONB CHECK (payload IS NULL OR jsonb_typeof(payload) = 'object'),
    cpu_usage_percent                    DOUBLE PRECISION,
    memory_usage_percent                 DOUBLE PRECISION,
    network_received_bytes_per_second    DOUBLE PRECISION,
    network_transmitted_bytes_per_second DOUBLE PRECISION,
    disk_read_bytes_per_second           DOUBLE PRECISION,
    disk_written_bytes_per_second        DOUBLE PRECISION,
    max_temperature_celsius              DOUBLE PRECISION,
    gpu_utilization_percent              DOUBLE PRECISION,
    gpu_memory_usage_percent             DOUBLE PRECISION
);

ALTER TABLE monitored_hosts
    ADD CONSTRAINT monitored_hosts_latest_report
    FOREIGN KEY (latest_report_id) REFERENCES metric_reports(report_id)
    ON DELETE SET NULL DEFERRABLE INITIALLY DEFERRED;

CREATE INDEX metric_reports_host_collected_at ON metric_reports(host_id, collected_at DESC, report_id);
CREATE INDEX metric_reports_received_at ON metric_reports(received_at);

CREATE TABLE agent_credentials (
    credential_id UUID PRIMARY KEY,
    host_id        UUID NOT NULL REFERENCES monitored_hosts(host_id) ON DELETE CASCADE,
    token_hash     TEXT NOT NULL UNIQUE CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    issued_at      BIGINT NOT NULL DEFAULT ((extract(epoch FROM clock_timestamp()) * 1000000)::BIGINT),
    last_used_at   BIGINT
);

CREATE INDEX agent_credentials_host ON agent_credentials(host_id);

CREATE TABLE instance_invites (
    invite_id            UUID PRIMARY KEY,
    instance_id          UUID NOT NULL,
    activation_code_hash TEXT NOT NULL UNIQUE CHECK (activation_code_hash ~ '^[0-9a-f]{64}$'),
    display_name         TEXT NOT NULL CHECK (char_length(btrim(display_name)) BETWEEN 1 AND 255),
    status               TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'active', 'cancelled')),
    expires_at           BIGINT NOT NULL,
    created_at           BIGINT NOT NULL DEFAULT ((extract(epoch FROM clock_timestamp()) * 1000000)::BIGINT),
    activated_at         BIGINT,
    cancelled_at         BIGINT,
    CHECK (expires_at > created_at),
    CHECK (
        (status = 'pending' AND activated_at IS NULL AND cancelled_at IS NULL)
        OR (status = 'active' AND activated_at IS NOT NULL AND cancelled_at IS NULL)
        OR (status = 'cancelled' AND cancelled_at IS NOT NULL)
    )
);

CREATE INDEX instance_invites_created_at ON instance_invites(created_at DESC);
CREATE UNIQUE INDEX instance_invites_one_pending_per_instance
    ON instance_invites(instance_id) WHERE status = 'pending';

CREATE TABLE pairing_requests (
    request_id          UUID PRIMARY KEY,
    requested_host_id   UUID NOT NULL,
    os                  TEXT NOT NULL CHECK (char_length(btrim(os)) BETWEEN 1 AND 64),
    os_version          TEXT,
    kernel_version      TEXT,
    arch                TEXT NOT NULL CHECK (char_length(btrim(arch)) BETWEEN 1 AND 64),
    agent_version       TEXT NOT NULL CHECK (char_length(btrim(agent_version)) BETWEEN 1 AND 128),
    token_hash          TEXT NOT NULL UNIQUE CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    polling_secret_hash TEXT NOT NULL UNIQUE CHECK (polling_secret_hash ~ '^[0-9a-f]{64}$'),
    status              TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'active', 'denied')),
    invite_id           UUID UNIQUE REFERENCES instance_invites(invite_id),
    instance_id         UUID REFERENCES monitored_hosts(host_id) ON DELETE CASCADE,
    expires_at          BIGINT NOT NULL,
    created_at          BIGINT NOT NULL DEFAULT ((extract(epoch FROM clock_timestamp()) * 1000000)::BIGINT),
    activated_at        BIGINT,
    CHECK (expires_at > created_at),
    CHECK (
        (status = 'pending' AND invite_id IS NULL AND instance_id IS NULL AND activated_at IS NULL)
        OR (status = 'active' AND invite_id IS NOT NULL AND instance_id IS NOT NULL AND activated_at IS NOT NULL)
        OR status = 'denied'
    )
);

CREATE INDEX pairing_requests_expires_at ON pairing_requests(expires_at) WHERE status = 'pending';
