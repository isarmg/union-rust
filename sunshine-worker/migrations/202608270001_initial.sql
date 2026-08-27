-- Owned and versioned by the Sunshine module. The deployment role provisions
-- the `sunshine` schema; sarmg-platform-postgres sets search_path here before
-- SQLx executes this migration.

CREATE TABLE hosts (
    host_id            text PRIMARY KEY
                               CHECK (length(trim(host_id)) BETWEEN 1 AND 255),
    name               text NOT NULL
                               CHECK (length(trim(name)) BETWEEN 1 AND 128),
    address            text NOT NULL
                               CHECK (length(trim(address)) BETWEEN 1 AND 253),
    web_port           integer NOT NULL CHECK (web_port BETWEEN 1 AND 65535),
    username           text NOT NULL
                               CHECK (length(trim(username)) BETWEEN 1 AND 256),
    secret             text,
    verify_tls         boolean NOT NULL,
    position           bigint NOT NULL CHECK (position >= 0),
    created_at_micros  bigint NOT NULL,
    updated_at_micros  bigint NOT NULL,
    CHECK (secret IS NULL OR length(secret) > 0)
);

CREATE INDEX hosts_position_idx
    ON hosts(position, created_at_micros, host_id);

CREATE TABLE audit_logs (
    audit_id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    action             text NOT NULL CHECK (length(action) BETWEEN 1 AND 128),
    target             text NOT NULL CHECK (length(target) BETWEEN 1 AND 255),
    detail             text,
    actor              text NOT NULL CHECK (length(actor) BETWEEN 1 AND 128),
    created_at_micros  bigint NOT NULL
);

CREATE INDEX audit_logs_created_at_idx ON audit_logs(created_at_micros DESC);

-- Import batches are both the verification record and the exact rollback
-- journal. Values contain only module ciphertext, never plaintext credentials.
CREATE TABLE import_batches (
    batch_id            uuid PRIMARY KEY,
    source_fingerprint  text NOT NULL CHECK (length(source_fingerprint) = 64),
    source_row_count    integer NOT NULL CHECK (source_row_count >= 0),
    before_state        jsonb NOT NULL,
    imported_state      jsonb NOT NULL,
    status              text NOT NULL CHECK (status IN ('applied', 'rolled_back')),
    imported_at_micros  bigint NOT NULL,
    verified_at_micros  bigint,
    rolled_back_at_micros bigint
);

