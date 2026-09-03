-- 0006_system: app_setting（KV 系统设置）

CREATE TABLE app_setting (
    key        VARCHAR(128) PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
