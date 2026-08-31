-- 0001_auth_user: user, auth_token, auth_login_event（认证与用户）
-- MySQL: time=DATETIME(6), boolean=TINYINT(1), JSON=JSON
-- A class: functional unique indexes using CASE expressions (MySQL 8.0.13+)

CREATE TABLE `user` (
    id            BIGINT AUTO_INCREMENT PRIMARY KEY,
    username      VARCHAR(255) NOT NULL,
    password_hash VARCHAR(255) NOT NULL,
    role          VARCHAR(16) NOT NULL DEFAULT 'user',
    is_disabled   TINYINT(1) NOT NULL DEFAULT 0,
    last_login_at DATETIME(6),
    created_at    DATETIME(6) NOT NULL DEFAULT NOW(6),
    updated_at    DATETIME(6) NOT NULL DEFAULT NOW(6),
    UNIQUE KEY uk_user_username (username)
);
CREATE INDEX idx_user_role ON `user` (role);

CREATE TABLE auth_token (
    id             BIGINT AUTO_INCREMENT PRIMARY KEY,
    token_hash     CHAR(64) NOT NULL,
    token_prefix   VARCHAR(16),
    kind           VARCHAR(16) NOT NULL DEFAULT 'user',
    user_id        BIGINT NOT NULL,
    device_client  VARCHAR(255),
    device_name    VARCHAR(255),
    device_id      VARCHAR(255),
    device_version VARCHAR(255),
    created_at     DATETIME(6) NOT NULL DEFAULT NOW(6),
    last_used_at   DATETIME(6),
    revoked_at     DATETIME(6),
    UNIQUE KEY uk_auth_token_hash (token_hash)
);
CREATE INDEX idx_auth_token_user ON auth_token (user_id);

CREATE TABLE auth_login_event (
    id            BIGINT NOT NULL AUTO_INCREMENT,
    created_at    DATETIME(6) NOT NULL DEFAULT NOW(6),
    user_id       BIGINT,
    username      VARCHAR(255),
    login_type    VARCHAR(32) NOT NULL,
    success       TINYINT(1) NOT NULL,
    ip            VARCHAR(64) NOT NULL DEFAULT '',
    device_id     VARCHAR(255),
    device_name   VARCHAR(255),
    device_client VARCHAR(255),
    user_agent    TEXT,
    reason        VARCHAR(255),
    PRIMARY KEY (id, created_at)
)
PARTITION BY RANGE (TO_DAYS(created_at)) (
    PARTITION p_init VALUES LESS THAN (TO_DAYS('2026-09-01'))
);
CREATE INDEX idx_auth_login_created ON auth_login_event (created_at DESC);
CREATE INDEX idx_auth_login_user_time ON auth_login_event (user_id, created_at DESC);
