-- 0004_media: media_source, external_subtitle（媒体源与外挂字幕）
-- 一集多源、一源一集：media_source.item_id 指向所属条目

CREATE TABLE media_source (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    uuid          TEXT    NOT NULL UNIQUE,
    item_id       INTEGER NOT NULL,
    name          TEXT    NOT NULL,
    status        TEXT    NOT NULL DEFAULT 'ok',
    container     TEXT,
    protocol      TEXT    NOT NULL DEFAULT 'file',
    path          TEXT,
    remote_path   TEXT,
    file_size     INTEGER,
    file_duration INTEGER,
    metadata      TEXT,
    chapters      TEXT,
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX idx_media_source_item ON media_source (item_id);
CREATE INDEX idx_media_source_path ON media_source (path);

CREATE TABLE external_subtitle (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    media_source_id INTEGER NOT NULL,
    codec           TEXT,
    display_title   TEXT,
    is_forced       INTEGER NOT NULL DEFAULT 0,
    path            TEXT,
    created_at      TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at      TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX idx_external_subtitle_source ON external_subtitle (media_source_id);
