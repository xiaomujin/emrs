-- 0004_media: media_source, external_subtitle（媒体源与外挂字幕）
-- MySQL: C class = prefix index path(191)
-- 一集多源、一源一集：media_source.item_id 指向所属条目

CREATE TABLE media_source (
    id            BIGINT AUTO_INCREMENT PRIMARY KEY,
    uuid          CHAR(36) NOT NULL,
    item_id       BIGINT NOT NULL,
    name          VARCHAR(255) NOT NULL,
    status        VARCHAR(32) NOT NULL DEFAULT 'ok',
    container     VARCHAR(64),
    protocol      VARCHAR(16) NOT NULL DEFAULT 'file',
    path          TEXT,
    remote_path   TEXT,
    file_size     BIGINT,
    file_duration BIGINT,
    metadata      JSON,
    chapters      JSON,
    created_at    DATETIME(6) NOT NULL DEFAULT NOW(6),
    updated_at    DATETIME(6) NOT NULL DEFAULT NOW(6),
    UNIQUE KEY uk_media_source_uuid (uuid)
);
CREATE INDEX idx_media_source_item ON media_source (item_id);
CREATE INDEX idx_media_source_path ON media_source (path(191));

CREATE TABLE external_subtitle (
    id              BIGINT AUTO_INCREMENT PRIMARY KEY,
    media_source_id BIGINT NOT NULL,
    codec           VARCHAR(64),
    display_title   VARCHAR(255),
    is_forced       TINYINT(1) NOT NULL DEFAULT 0,
    path            TEXT,
    created_at      DATETIME(6) NOT NULL DEFAULT NOW(6),
    updated_at      DATETIME(6) NOT NULL DEFAULT NOW(6)
);
CREATE INDEX idx_external_subtitle_source ON external_subtitle (media_source_id);
