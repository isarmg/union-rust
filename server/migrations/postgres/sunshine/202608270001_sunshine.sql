CREATE TABLE hosts (
    host_id    TEXT PRIMARY KEY CHECK (char_length(btrim(host_id)) BETWEEN 1 AND 255),
    address    TEXT NOT NULL CHECK (char_length(btrim(address)) BETWEEN 1 AND 253),
    config     JSONB NOT NULL CHECK (jsonb_typeof(config) = 'object'),
    secret     TEXT CHECK (secret IS NULL OR char_length(secret) > 0),
    position   BIGINT NOT NULL DEFAULT 0 CHECK (position >= 0),
    created_at BIGINT NOT NULL DEFAULT ((extract(epoch FROM clock_timestamp()) * 1000000)::BIGINT),
    updated_at BIGINT NOT NULL DEFAULT ((extract(epoch FROM clock_timestamp()) * 1000000)::BIGINT)
);

CREATE INDEX hosts_position ON hosts(position, created_at, host_id);
