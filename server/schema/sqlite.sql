-- UnionC SQLite schema.
--
-- This final-state schema is the sole schema for new installations. All
-- timestamps are Unix microseconds, UUIDs are canonical text, and JSON is
-- validated text. STRICT tables strengthen SQLite's default affinity rules.

CREATE TABLE audit_logs (
    id         INTEGER PRIMARY KEY,
    action     TEXT NOT NULL CHECK (length(action) BETWEEN 1 AND 128),
    target     TEXT NOT NULL CHECK (length(target) BETWEEN 1 AND 128),
    detail     TEXT,
    actor      TEXT NOT NULL DEFAULT 'system',
    request_id TEXT,
    created_at INTEGER NOT NULL
                   DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER))
) STRICT;

CREATE INDEX idx_audit_logs_created_at ON audit_logs(created_at);

CREATE TABLE external_hosts (
    kind       TEXT NOT NULL CHECK (kind = 'sunshine'),
    host_id    TEXT NOT NULL CHECK (length(trim(host_id)) BETWEEN 1 AND 255),
    address    TEXT NOT NULL CHECK (length(trim(address)) BETWEEN 1 AND 253),
    config     TEXT NOT NULL
                    CHECK (json_valid(config) AND json_type(config) = 'object'),
    secret     TEXT CHECK (secret IS NULL OR length(secret) > 0),
    position   INTEGER NOT NULL DEFAULT 0 CHECK (position >= 0),
    created_at INTEGER NOT NULL
                   DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)),
    updated_at INTEGER NOT NULL
                   DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)),
    PRIMARY KEY (kind, host_id)
) STRICT;

CREATE INDEX idx_external_hosts_kind_position
    ON external_hosts(kind, position, created_at, host_id);

CREATE TABLE monitored_hosts (
    host_id                 TEXT PRIMARY KEY CHECK (length(host_id) = 36),
    name                    TEXT NOT NULL CHECK (length(trim(name)) BETWEEN 1 AND 255),
    os                      TEXT NOT NULL CHECK (length(trim(os)) BETWEEN 1 AND 64),
    os_version              TEXT,
    kernel_version          TEXT,
    arch                    TEXT NOT NULL CHECK (length(trim(arch)) BETWEEN 1 AND 64),
    agent_version           TEXT NOT NULL CHECK (length(trim(agent_version)) BETWEEN 1 AND 128),
    capabilities            TEXT NOT NULL DEFAULT '[]'
                                CHECK (
                                    json_valid(capabilities)
                                    AND json_type(capabilities) = 'array'
                                ),
    registered_at           INTEGER NOT NULL
                                DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)),
    last_seen_at            INTEGER NOT NULL
                                DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)),
    latest_report_id        TEXT,
    latest_collected_at     INTEGER,
    latest_interval_seconds REAL,
    lifecycle_status        TEXT NOT NULL DEFAULT 'active'
                                CHECK (lifecycle_status IN ('active', 'revoked')),
    revoked_at              INTEGER,
    CHECK (
        (lifecycle_status = 'active' AND revoked_at IS NULL)
        OR (lifecycle_status = 'revoked' AND revoked_at IS NOT NULL)
    ),
    FOREIGN KEY (latest_report_id)
        REFERENCES agent_metric_reports(report_id)
        ON DELETE SET NULL
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE INDEX idx_monitored_hosts_last_seen_at
    ON monitored_hosts(last_seen_at DESC);

CREATE TABLE agent_metric_reports (
    report_id                            TEXT PRIMARY KEY CHECK (length(report_id) = 36),
    host_id                              TEXT NOT NULL
                                              REFERENCES monitored_hosts(host_id)
                                              ON DELETE CASCADE,
    schema_version                       INTEGER NOT NULL CHECK (schema_version > 0),
    collected_at                         INTEGER NOT NULL,
    received_at                          INTEGER NOT NULL
                                              DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)),
    interval_seconds                     REAL NOT NULL
                                              CHECK (interval_seconds > 0 AND interval_seconds <= 3600),
    payload                              TEXT CHECK (
                                              payload IS NULL
                                              OR (json_valid(payload) AND json_type(payload) = 'object')
                                          ),
    cpu_usage_percent                    REAL,
    memory_usage_percent                 REAL,
    network_received_bytes_per_second    REAL,
    network_transmitted_bytes_per_second REAL,
    disk_read_bytes_per_second           REAL,
    disk_written_bytes_per_second        REAL,
    max_temperature_celsius              REAL,
    gpu_utilization_percent              REAL,
    gpu_memory_usage_percent             REAL
) STRICT;

CREATE INDEX idx_agent_metric_reports_host_collected_at
    ON agent_metric_reports(host_id, collected_at DESC, report_id);

CREATE INDEX idx_agent_metric_reports_received_at
    ON agent_metric_reports(received_at);

CREATE TABLE agent_credentials (
    credential_id TEXT PRIMARY KEY CHECK (length(credential_id) = 36),
    host_id        TEXT NOT NULL
                        REFERENCES monitored_hosts(host_id) ON DELETE CASCADE,
    token_hash     TEXT NOT NULL UNIQUE CHECK (
                       length(token_hash) = 64
                       AND token_hash NOT GLOB '*[^0-9a-f]*'
                   ),
    issued_at      INTEGER NOT NULL
                       DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)),
    last_used_at   INTEGER,
    revoked_at     INTEGER
) STRICT;

CREATE INDEX idx_agent_credentials_host_active
    ON agent_credentials(host_id)
    WHERE revoked_at IS NULL;

CREATE TABLE agent_instance_invites (
    invite_id            TEXT PRIMARY KEY CHECK (length(invite_id) = 36),
    instance_id          TEXT NOT NULL CHECK (length(instance_id) = 36),
    activation_code_hash TEXT NOT NULL UNIQUE CHECK (
                             length(activation_code_hash) = 64
                             AND activation_code_hash NOT GLOB '*[^0-9a-f]*'
                         ),
    display_name         TEXT NOT NULL CHECK (length(trim(display_name)) BETWEEN 1 AND 255),
    status               TEXT NOT NULL DEFAULT 'pending'
                             CHECK (status IN ('pending', 'active', 'revoked')),
    expires_at           INTEGER NOT NULL,
    created_at           INTEGER NOT NULL
                             DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)),
    activated_at         INTEGER,
    revoked_at           INTEGER,
    CHECK (expires_at > created_at),
    CHECK (
        (status = 'pending' AND activated_at IS NULL AND revoked_at IS NULL)
        OR (status = 'active' AND activated_at IS NOT NULL AND revoked_at IS NULL)
        OR (status = 'revoked' AND revoked_at IS NOT NULL)
    )
) STRICT;

CREATE INDEX idx_agent_instance_invites_created_at
    ON agent_instance_invites(created_at DESC);

CREATE UNIQUE INDEX idx_agent_instance_invites_one_pending_per_instance
    ON agent_instance_invites(instance_id)
    WHERE status = 'pending';

CREATE TABLE agent_pairing_requests (
    request_id          TEXT PRIMARY KEY CHECK (length(request_id) = 36),
    requested_host_id   TEXT NOT NULL CHECK (length(requested_host_id) = 36),
    name                TEXT NOT NULL CHECK (length(trim(name)) BETWEEN 1 AND 255),
    os                  TEXT NOT NULL CHECK (length(trim(os)) BETWEEN 1 AND 64),
    os_version          TEXT,
    kernel_version      TEXT,
    arch                TEXT NOT NULL CHECK (length(trim(arch)) BETWEEN 1 AND 64),
    agent_version       TEXT NOT NULL CHECK (length(trim(agent_version)) BETWEEN 1 AND 128),
    token_hash          TEXT NOT NULL UNIQUE CHECK (
                            length(token_hash) = 64
                            AND token_hash NOT GLOB '*[^0-9a-f]*'
                        ),
    polling_secret_hash TEXT NOT NULL UNIQUE CHECK (
                            length(polling_secret_hash) = 64
                            AND polling_secret_hash NOT GLOB '*[^0-9a-f]*'
                        ),
    status              TEXT NOT NULL DEFAULT 'pending'
                            CHECK (status IN ('pending', 'active', 'denied')),
    invite_id           TEXT UNIQUE REFERENCES agent_instance_invites(invite_id),
    instance_id         TEXT REFERENCES monitored_hosts(host_id) ON DELETE CASCADE,
    expires_at          INTEGER NOT NULL,
    created_at          INTEGER NOT NULL
                            DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)),
    activated_at        INTEGER,
    CHECK (expires_at > created_at),
    CHECK (
        (status = 'pending' AND invite_id IS NULL AND instance_id IS NULL AND activated_at IS NULL)
        OR (status = 'active' AND invite_id IS NOT NULL AND instance_id IS NOT NULL AND activated_at IS NOT NULL)
        OR status = 'denied'
    )
) STRICT;

CREATE INDEX idx_agent_pairing_requests_expires_at
    ON agent_pairing_requests(expires_at)
    WHERE status = 'pending';
