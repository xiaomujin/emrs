-- 0003_item: item（单表多态）+ genre / people / studio / tag 规范表及关联表 + item_image
-- item.type ∈ movie/series/season/episode，parent_id 自引用（episode→season→series）

CREATE TABLE item (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    type             TEXT    NOT NULL,
    parent_id        INTEGER,
    library_id       INTEGER NOT NULL,
    is_virtual       INTEGER NOT NULL DEFAULT 0,
    scrape_status    TEXT    NOT NULL DEFAULT 'pending',
    scrape_attempts  INTEGER NOT NULL DEFAULT 0,
    tmdb_id          TEXT,
    imdb_id          TEXT,
    tvdb_id          TEXT,
    title            TEXT    NOT NULL,
    sort_title       TEXT,
    description      TEXT,
    tagline          TEXT,
    date_air         TEXT,
    end_date         TEXT,
    runtime          INTEGER,
    production_year  INTEGER,
    status           TEXT,
    season_number    INTEGER,
    episode_number   INTEGER,
    community_rating REAL,
    official_rating  TEXT,
    created_at       TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at       TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE UNIQUE INDEX unx_item_movie_tmdb     ON item (tmdb_id) WHERE type='movie'  AND tmdb_id IS NOT NULL;
CREATE UNIQUE INDEX unx_item_series_tmdb    ON item (tmdb_id) WHERE type='series' AND tmdb_id IS NOT NULL;
CREATE UNIQUE INDEX unx_item_season_parent  ON item (parent_id, season_number) WHERE type='season';
CREATE UNIQUE INDEX unx_item_episode_parent ON item (parent_id, episode_number) WHERE type='episode';
CREATE INDEX idx_item_parent ON item (parent_id);
CREATE INDEX idx_item_library ON item (library_id);
CREATE INDEX idx_item_scrape_status ON item (scrape_status);
CREATE INDEX idx_item_type ON item (type);

CREATE TABLE genre (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    tmdb_id    TEXT,
    name       TEXT    NOT NULL,
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE UNIQUE INDEX unx_genre_tmdb ON genre (tmdb_id) WHERE tmdb_id IS NOT NULL;
CREATE INDEX idx_genre_name ON genre (name);

CREATE TABLE item_genre (
    item_id  INTEGER NOT NULL,
    genre_id INTEGER NOT NULL,
    PRIMARY KEY (item_id, genre_id)
);
CREATE INDEX idx_item_genre_genre ON item_genre (genre_id);

CREATE TABLE people (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    tmdb_id       TEXT    NOT NULL UNIQUE,
    name          TEXT    NOT NULL,
    original_name TEXT,
    gender        INTEGER NOT NULL DEFAULT 0,
    description   TEXT,
    birthday      TEXT,
    deathday      TEXT,
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE item_people (
    item_id        INTEGER NOT NULL,
    people_id      INTEGER NOT NULL,
    role           TEXT    NOT NULL DEFAULT 'Actor',
    character_name TEXT,
    sort_order     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (item_id, people_id, role)
);
CREATE INDEX idx_item_people_people ON item_people (people_id);
CREATE INDEX idx_item_people_item_sort ON item_people (item_id, sort_order);

CREATE TABLE studio (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    tmdb_id    TEXT,
    name       TEXT    NOT NULL,
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE UNIQUE INDEX unx_studio_tmdb ON studio (tmdb_id) WHERE tmdb_id IS NOT NULL;
CREATE INDEX idx_studio_name ON studio (name);

CREATE TABLE item_studio (
    item_id    INTEGER NOT NULL,
    studio_id  INTEGER NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (item_id, studio_id)
);
CREATE INDEX idx_item_studio_studio ON item_studio (studio_id);
CREATE INDEX idx_item_studio_item_sort ON item_studio (item_id, sort_order);

CREATE TABLE tag (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    tmdb_id    TEXT,
    name       TEXT    NOT NULL,
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE UNIQUE INDEX unx_tag_tmdb ON tag (tmdb_id) WHERE tmdb_id IS NOT NULL;
CREATE INDEX idx_tag_name ON tag (name);

CREATE TABLE item_tag (
    item_id    INTEGER NOT NULL,
    tag_id     INTEGER NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (item_id, tag_id)
);
CREATE INDEX idx_item_tag_tag ON item_tag (tag_id);
CREATE INDEX idx_item_tag_item_sort ON item_tag (item_id, sort_order);

CREATE TABLE item_image (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    parent_type TEXT    NOT NULL,
    parent_id   INTEGER NOT NULL,
    image_type  TEXT    NOT NULL,
    path_type   TEXT,
    path_url    TEXT,
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE UNIQUE INDEX unx_item_image_primary ON item_image (parent_type, parent_id) WHERE image_type='primary';
CREATE INDEX idx_item_image_parent_type ON item_image (parent_type, parent_id, image_type);
CREATE INDEX idx_item_image_parent ON item_image (parent_type, parent_id);
