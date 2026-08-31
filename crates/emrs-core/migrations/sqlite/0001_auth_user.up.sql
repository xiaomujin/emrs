-- 0001_auth_user: user, auth_token, auth_login_event（认证与用户）
-- SQLite: time=TEXT(ISO8601), boolean=INTEGER(0/1), JSON=TEXT

CREATE TABLE "user" (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    username      TEXT    NOT NULL,
    password_hash TEXT    NOT NULL,
    role          TEXT    NOT NULL DEFAULT 'user',
    is_disabled   INTEGER NOT NULL DEFAULT 0,
    last_login_at TEXT,
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE UNIQUE INDEX unx_user_username ON "user" (username);
CREATE INDEX idx_user_role ON "user" (role);

CREATE TABLE auth_token (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    token_hash     TEXT    NOT NULL UNIQUE,
    token_prefix   TEXT,
    kind           TEXT    NOT NULL DEFAULT 'user',
    user_id        INTEGER NOT NULL,
    device_client  TEXT,
    device_name    TEXT,
    device_id      TEXT,
    device_version TEXT,
    created_at     TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    last_used_at   TEXT,
    revoked_at     TEXT
);
CREATE INDEX idx_auth_token_user ON auth_token (user_id);

CREATE TABLE auth_login_event (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    user_id       INTEGER,
    username      TEXT,
    login_type    TEXT    NOT NULL,
    success       INTEGER NOT NULL,
    ip            TEXT    NOT NULL DEFAULT '',
    device_id     TEXT,
    device_name   TEXT,
    device_client TEXT,
    user_agent    TEXT,
    reason        TEXT
);
CREATE INDEX idx_auth_login_created ON auth_login_event (created_at DESC);
CREATE INDEX idx_auth_login_user_time ON auth_login_event (user_id, created_at DESC);
