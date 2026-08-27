CREATE TABLE audit_logs (
    id         BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    action     TEXT NOT NULL CHECK (char_length(action) BETWEEN 1 AND 128),
    target     TEXT NOT NULL CHECK (char_length(target) BETWEEN 1 AND 128),
    detail     TEXT,
    actor      TEXT NOT NULL DEFAULT 'system',
    request_id TEXT,
    created_at BIGINT NOT NULL DEFAULT ((extract(epoch FROM clock_timestamp()) * 1000000)::BIGINT)
);

CREATE INDEX audit_logs_created_at ON audit_logs(created_at);
