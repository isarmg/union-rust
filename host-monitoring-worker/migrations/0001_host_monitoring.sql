CREATE SCHEMA IF NOT EXISTS host_monitoring;

CREATE TABLE IF NOT EXISTS host_monitoring.import_batches (
    import_id       uuid PRIMARY KEY,
    source_path     text NOT NULL,
    source_sha256   text NOT NULL CHECK (source_sha256 ~ '^[0-9a-f]{64}$'),
    manifest        jsonb NOT NULL CHECK (jsonb_typeof(manifest) = 'object'),
    status          text NOT NULL CHECK (status IN ('complete', 'rolled_back')),
    imported_at     timestamptz NOT NULL DEFAULT now(),
    rolled_back_at  timestamptz
);

CREATE TABLE IF NOT EXISTS host_monitoring.monitored_hosts (
    host_id                  uuid PRIMARY KEY,
    name                     text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 255),
    os                       text NOT NULL CHECK (length(btrim(os)) BETWEEN 1 AND 64),
    os_version               text,
    kernel_version           text,
    arch                     text NOT NULL CHECK (length(btrim(arch)) BETWEEN 1 AND 64),
    agent_version            text NOT NULL CHECK (length(btrim(agent_version)) BETWEEN 1 AND 128),
    capabilities             jsonb NOT NULL DEFAULT '[]'::jsonb
                                 CHECK (jsonb_typeof(capabilities) = 'array'),
    registered_at            timestamptz NOT NULL DEFAULT now(),
    last_seen_at             timestamptz NOT NULL DEFAULT now(),
    latest_report_id         uuid,
    latest_collected_at      timestamptz,
    latest_interval_seconds  double precision,
    source_import_id         uuid,
    CHECK (latest_interval_seconds IS NULL OR
           (latest_interval_seconds > 0 AND latest_interval_seconds <= 3600))
);

CREATE INDEX IF NOT EXISTS monitored_hosts_registered
    ON host_monitoring.monitored_hosts(registered_at, host_id);
CREATE INDEX IF NOT EXISTS monitored_hosts_last_seen
    ON host_monitoring.monitored_hosts(last_seen_at DESC);
CREATE INDEX IF NOT EXISTS monitored_hosts_source_import
    ON host_monitoring.monitored_hosts(source_import_id) WHERE source_import_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS host_monitoring.agent_metric_reports (
    report_id                             uuid PRIMARY KEY,
    host_id                               uuid NOT NULL
                                             REFERENCES host_monitoring.monitored_hosts(host_id)
                                             ON DELETE CASCADE,
    schema_version                        integer NOT NULL CHECK (schema_version > 0),
    collected_at                          timestamptz NOT NULL,
    received_at                           timestamptz NOT NULL DEFAULT now(),
    interval_seconds                      double precision NOT NULL
                                             CHECK (interval_seconds > 0 AND interval_seconds <= 3600),
    payload                               jsonb CHECK (payload IS NULL OR jsonb_typeof(payload) = 'object'),
    cpu_usage_percent                     double precision,
    memory_usage_percent                  double precision,
    network_received_bytes_per_second     double precision,
    network_transmitted_bytes_per_second  double precision,
    disk_read_bytes_per_second            double precision,
    disk_written_bytes_per_second         double precision,
    max_temperature_celsius               double precision,
    gpu_utilization_percent               double precision,
    gpu_memory_usage_percent              double precision,
    source_import_id                      uuid
);

CREATE INDEX IF NOT EXISTS agent_metric_reports_host_collected
    ON host_monitoring.agent_metric_reports(host_id, collected_at DESC, report_id DESC);
CREATE INDEX IF NOT EXISTS agent_metric_reports_received
    ON host_monitoring.agent_metric_reports(received_at);
CREATE INDEX IF NOT EXISTS agent_metric_reports_source_import
    ON host_monitoring.agent_metric_reports(source_import_id) WHERE source_import_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS host_monitoring.agent_credentials (
    credential_id   uuid PRIMARY KEY,
    host_id          uuid NOT NULL
                         REFERENCES host_monitoring.monitored_hosts(host_id) ON DELETE CASCADE,
    token_hash       text NOT NULL UNIQUE CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    issued_at        timestamptz NOT NULL DEFAULT now(),
    last_used_at     timestamptz,
    source_import_id uuid
);

CREATE INDEX IF NOT EXISTS agent_credentials_host
    ON host_monitoring.agent_credentials(host_id);
CREATE INDEX IF NOT EXISTS agent_credentials_source_import
    ON host_monitoring.agent_credentials(source_import_id) WHERE source_import_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS host_monitoring.agent_instance_invites (
    invite_id             uuid PRIMARY KEY,
    instance_id           uuid NOT NULL,
    activation_code_hash  text NOT NULL UNIQUE CHECK (activation_code_hash ~ '^[0-9a-f]{64}$'),
    display_name          text NOT NULL CHECK (length(btrim(display_name)) BETWEEN 1 AND 255),
    status                text NOT NULL DEFAULT 'pending'
                               CHECK (status IN ('pending', 'active', 'cancelled')),
    expires_at            timestamptz NOT NULL,
    created_at            timestamptz NOT NULL DEFAULT now(),
    activated_at          timestamptz,
    cancelled_at          timestamptz,
    source_import_id      uuid,
    CHECK (expires_at > created_at),
    CHECK (
      (status = 'pending' AND activated_at IS NULL AND cancelled_at IS NULL) OR
      (status = 'active' AND activated_at IS NOT NULL AND cancelled_at IS NULL) OR
      (status = 'cancelled' AND cancelled_at IS NOT NULL)
    )
);

CREATE INDEX IF NOT EXISTS agent_instance_invites_created
    ON host_monitoring.agent_instance_invites(created_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS agent_instance_invites_one_pending
    ON host_monitoring.agent_instance_invites(instance_id) WHERE status = 'pending';
CREATE INDEX IF NOT EXISTS agent_instance_invites_source_import
    ON host_monitoring.agent_instance_invites(source_import_id) WHERE source_import_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS host_monitoring.agent_pairing_requests (
    request_id           uuid PRIMARY KEY,
    requested_host_id    uuid NOT NULL,
    os                   text NOT NULL CHECK (length(btrim(os)) BETWEEN 1 AND 64),
    os_version           text,
    kernel_version       text,
    arch                 text NOT NULL CHECK (length(btrim(arch)) BETWEEN 1 AND 64),
    agent_version        text NOT NULL CHECK (length(btrim(agent_version)) BETWEEN 1 AND 128),
    token_hash           text NOT NULL UNIQUE CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    polling_secret_hash  text NOT NULL UNIQUE CHECK (polling_secret_hash ~ '^[0-9a-f]{64}$'),
    status               text NOT NULL DEFAULT 'pending'
                              CHECK (status IN ('pending', 'active', 'denied')),
    invite_id            uuid UNIQUE REFERENCES host_monitoring.agent_instance_invites(invite_id),
    instance_id          uuid REFERENCES host_monitoring.monitored_hosts(host_id) ON DELETE CASCADE,
    expires_at           timestamptz NOT NULL,
    created_at           timestamptz NOT NULL DEFAULT now(),
    activated_at         timestamptz,
    source_import_id     uuid,
    CHECK (expires_at > created_at),
    CHECK (
      (status = 'pending' AND invite_id IS NULL AND instance_id IS NULL AND activated_at IS NULL) OR
      (status = 'active' AND invite_id IS NOT NULL AND instance_id IS NOT NULL AND activated_at IS NOT NULL) OR
      status = 'denied'
    )
);

CREATE INDEX IF NOT EXISTS agent_pairing_requests_expiry
    ON host_monitoring.agent_pairing_requests(expires_at) WHERE status = 'pending';
CREATE INDEX IF NOT EXISTS agent_pairing_requests_source_import
    ON host_monitoring.agent_pairing_requests(source_import_id) WHERE source_import_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS host_monitoring.audit_events (
    event_id      bigserial PRIMARY KEY,
    action        text NOT NULL CHECK (length(action) BETWEEN 1 AND 128),
    target        text NOT NULL CHECK (length(target) BETWEEN 1 AND 128),
    detail        text,
    actor         text NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS audit_events_created
    ON host_monitoring.audit_events(created_at DESC);

