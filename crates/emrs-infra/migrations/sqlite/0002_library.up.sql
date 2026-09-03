-- 0002_library: library, library_path, scan_job（媒体库、挂载点、扫描任务）

CREATE TABLE library (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    name            TEXT    NOT NULL,
    collection_type TEXT    NOT NULL DEFAULT 'tvshows',
    created_at      TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at      TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE library_path (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    library_id INTEGER NOT NULL,
    path       TEXT    NOT NULL,
    path_type  TEXT    NOT NULL DEFAULT 'local',
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE UNIQUE INDEX unx_library_path_lib_path ON library_path (library_id, path);
CREATE INDEX idx_library_path_lib_sort ON library_path (library_id, sort_order);
CREATE INDEX idx_library_path_path ON library_path (path);

CREATE TABLE scan_job (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    library_id    INTEGER NOT NULL,
    status        TEXT    NOT NULL DEFAULT 'pending',
    triggered_by  TEXT    NOT NULL DEFAULT 'manual',
    started_at    TEXT,
    finished_at   TEXT,
    scanned_dirs  INTEGER NOT NULL DEFAULT 0,
    added_items   INTEGER NOT NULL DEFAULT 0,
    updated_items INTEGER NOT NULL DEFAULT 0,
    error         TEXT,
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX idx_scan_job_lib_status_created ON scan_job (library_id, status, created_at DESC);
