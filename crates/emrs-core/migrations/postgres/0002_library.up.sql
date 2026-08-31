-- 0002_library: library, library_path, scan_job（媒体库、挂载点、扫描任务）
-- PostgreSQL: A class = native partial unique indexes; B class = partial indexes

CREATE TABLE library (
    id              BIGSERIAL PRIMARY KEY,
    name            VARCHAR(255) NOT NULL,
    collection_type VARCHAR(32) NOT NULL DEFAULT 'tvshows',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE library_path (
    id         BIGSERIAL PRIMARY KEY,
    library_id BIGINT NOT NULL,
    path       VARCHAR(512) NOT NULL,
    path_type  VARCHAR(16) NOT NULL DEFAULT 'local',
    sort_order INT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX unx_library_path_lib_path ON library_path (library_id, path);
CREATE INDEX idx_library_path_lib_sort ON library_path (library_id, sort_order);
CREATE INDEX idx_library_path_path ON library_path (path);

CREATE TABLE scan_job (
    id            BIGSERIAL PRIMARY KEY,
    library_id    BIGINT NOT NULL,
    status        VARCHAR(16) NOT NULL DEFAULT 'pending',
    triggered_by  VARCHAR(16) NOT NULL DEFAULT 'manual',
    started_at    TIMESTAMPTZ,
    finished_at   TIMESTAMPTZ,
    scanned_dirs  INT NOT NULL DEFAULT 0,
    added_items   INT NOT NULL DEFAULT 0,
    updated_items INT NOT NULL DEFAULT 0,
    error         TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_scan_job_lib_status_created ON scan_job (library_id, status, created_at DESC);
