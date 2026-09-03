-- 0001_auth_user: user, auth_token, auth_login_event（认证与用户）
-- PostgreSQL: time=TIMESTAMPTZ, boolean=BOOLEAN, JSONB=JSONB
-- A class: native partial unique indexes; partition tables with RANGE

CREATE TABLE "user" (
    id            BIGSERIAL PRIMARY KEY,
    username      VARCHAR(255) NOT NULL,
    password_hash VARCHAR(255) NOT NULL,
    role          VARCHAR(16) NOT NULL DEFAULT 'user',
    is_disabled   BOOLEAN NOT NULL DEFAULT false,
    last_login_at TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX unx_user_username ON "user" (username);
CREATE INDEX idx_user_role ON "user" (role);

CREATE TABLE auth_token (
    id             BIGSERIAL PRIMARY KEY,
    token_hash     CHAR(64) NOT NULL UNIQUE,
    token_prefix   VARCHAR(16),
    kind           VARCHAR(16) NOT NULL DEFAULT 'user',
    user_id        BIGINT NOT NULL,
    device_client  VARCHAR(255),
    device_name    VARCHAR(255),
    device_id      VARCHAR(255),
    device_version VARCHAR(255),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at   TIMESTAMPTZ,
    revoked_at     TIMESTAMPTZ
);
CREATE INDEX idx_auth_token_user ON auth_token (user_id);

CREATE TABLE auth_login_event (
    id            BIGSERIAL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    user_id       BIGINT,
    username      VARCHAR(255),
    login_type    VARCHAR(32) NOT NULL,
    success       BOOLEAN NOT NULL,
    ip            VARCHAR(64) NOT NULL DEFAULT '',
    device_id     VARCHAR(255),
    device_name   VARCHAR(255),
    device_client VARCHAR(255),
    user_agent    TEXT,
    reason        VARCHAR(255),
    PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);
CREATE TABLE auth_login_event_p_init PARTITION OF auth_login_event FOR VALUES FROM ('2026-01-01') TO ('2026-09-01');
CREATE INDEX idx_auth_login_created ON auth_login_event (created_at DESC);
CREATE INDEX idx_auth_login_user_time ON auth_login_event (user_id, created_at DESC);
