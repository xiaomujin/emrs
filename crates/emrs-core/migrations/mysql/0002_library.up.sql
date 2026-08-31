-- 0002_library: library, library_path, scan_job（媒体库、挂载点、扫描任务）
-- MySQL: A class = functional CASE unique indexes; B class = plain index; C class = prefix index path(191)

CREATE TABLE library (
    id              BIGINT AUTO_INCREMENT PRIMARY KEY,
    name            VARCHAR(255) NOT NULL,
    collection_type VARCHAR(32) NOT NULL DEFAULT 'tvshows',
    created_at      DATETIME(6) NOT NULL DEFAULT NOW(6),
    updated_at      DATETIME(6) NOT NULL DEFAULT NOW(6)
);

CREATE TABLE library_path (
    id         BIGINT AUTO_INCREMENT PRIMARY KEY,
    library_id BIGINT NOT NULL,
    path       VARCHAR(512) NOT NULL,
    path_type  VARCHAR(16) NOT NULL DEFAULT 'local',
    sort_order INT NOT NULL DEFAULT 0,
    created_at DATETIME(6) NOT NULL DEFAULT NOW(6),
    updated_at DATETIME(6) NOT NULL DEFAULT NOW(6),
    UNIQUE KEY uk_library_path_lib_path (library_id, path)
);
CREATE INDEX idx_library_path_lib_sort ON library_path (library_id, sort_order);
CREATE INDEX idx_library_path_path ON library_path (path(191));

CREATE TABLE scan_job (
    id            BIGINT AUTO_INCREMENT PRIMARY KEY,
    library_id    BIGINT NOT NULL,
    status        VARCHAR(16) NOT NULL DEFAULT 'pending',
    triggered_by  VARCHAR(16) NOT NULL DEFAULT 'manual',
    started_at    DATETIME(6),
    finished_at   DATETIME(6),
    scanned_dirs  INT NOT NULL DEFAULT 0,
    added_items   INT NOT NULL DEFAULT 0,
    updated_items INT NOT NULL DEFAULT 0,
    error         TEXT,
    created_at    DATETIME(6) NOT NULL DEFAULT NOW(6)
);
CREATE INDEX idx_scan_job_lib_status_created ON scan_job (library_id, status, created_at DESC);
