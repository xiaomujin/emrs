-- 0004_media: media_source, external_subtitle（媒体源与外挂字幕）
-- PostgreSQL: A class = native partial unique indexes; B class = partial indexes
-- 一集多源、一源一集：media_source.item_id 指向所属条目

CREATE TABLE media_source (
    id            BIGSERIAL PRIMARY KEY,
    uuid          CHAR(36) NOT NULL UNIQUE,
    item_id       BIGINT NOT NULL,
    name          VARCHAR(255) NOT NULL,
    status        VARCHAR(32) NOT NULL DEFAULT 'ok',
    container     VARCHAR(64),
    protocol      VARCHAR(16) NOT NULL DEFAULT 'file',
    path          TEXT,
    remote_path   TEXT,
    file_size     BIGINT,
    file_duration BIGINT,
    metadata      JSONB,
    chapters      JSONB,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_media_source_item ON media_source (item_id);
CREATE INDEX idx_media_source_path ON media_source (path);

CREATE TABLE external_subtitle (
    id              BIGSERIAL PRIMARY KEY,
    media_source_id BIGINT NOT NULL,
    codec           VARCHAR(64),
    display_title   VARCHAR(255),
    is_forced       BOOLEAN NOT NULL DEFAULT false,
    path            TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_external_subtitle_source ON external_subtitle (media_source_id);
