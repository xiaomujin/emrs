-- 0003_item: item（单表多态）+ genre / people / studio / tag 规范表及关联表 + item_image
-- PostgreSQL: A class = native partial unique indexes; B class = partial indexes
-- item.type ∈ movie/series/season/episode，parent_id 自引用（episode→season→series）

CREATE TABLE item (
    id               BIGSERIAL PRIMARY KEY,
    type             VARCHAR(16) NOT NULL,
    parent_id        BIGINT,
    library_id       BIGINT NOT NULL,
    is_virtual       BOOLEAN NOT NULL DEFAULT false,
    scrape_status    VARCHAR(16) NOT NULL DEFAULT 'pending',
    scrape_attempts  BIGINT NOT NULL DEFAULT 0,
    tmdb_id          VARCHAR(64),
    imdb_id          VARCHAR(64),
    tvdb_id          VARCHAR(64),
    title            VARCHAR(255) NOT NULL,
    sort_title       VARCHAR(255),
    -- series 专用：剧集在磁盘上的目录路径（归一化正斜杠绝对路径），作跨扫描/新增文件的
    -- series 稳定身份锚（title 会被刮削改写，不能去重）。movie/season/episode 恒 NULL。
    source_dir       TEXT,
    description      TEXT,
    tagline          TEXT,
    date_air         TIMESTAMPTZ,
    end_date         TIMESTAMPTZ,
    runtime          INT,
    production_year  INT,
    status           VARCHAR(32),
    season_number    INT,
    episode_number   INT,
    community_rating NUMERIC(3,1),
    official_rating  VARCHAR(16),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
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
    id         BIGSERIAL PRIMARY KEY,
    tmdb_id    VARCHAR(64),
    name       VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX unx_genre_tmdb ON genre (tmdb_id) WHERE tmdb_id IS NOT NULL;
CREATE INDEX idx_genre_name ON genre (name);

CREATE TABLE item_genre (
    item_id  BIGINT NOT NULL,
    genre_id BIGINT NOT NULL,
    PRIMARY KEY (item_id, genre_id)
);
CREATE INDEX idx_item_genre_genre ON item_genre (genre_id);

CREATE TABLE people (
    id            BIGSERIAL PRIMARY KEY,
    tmdb_id       VARCHAR(64) NOT NULL UNIQUE,
    name          VARCHAR(255) NOT NULL,
    original_name VARCHAR(255),
    gender        SMALLINT NOT NULL DEFAULT 0,
    description   TEXT,
    birthday      DATE,
    deathday      DATE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE item_people (
    item_id        BIGINT NOT NULL,
    people_id      BIGINT NOT NULL,
    role           VARCHAR(64) NOT NULL DEFAULT 'Actor',
    character_name VARCHAR(255),
    sort_order     INT NOT NULL DEFAULT 0,
    PRIMARY KEY (item_id, people_id, role)
);
CREATE INDEX idx_item_people_people ON item_people (people_id);
CREATE INDEX idx_item_people_item_sort ON item_people (item_id, sort_order);

CREATE TABLE studio (
    id         BIGSERIAL PRIMARY KEY,
    tmdb_id    VARCHAR(64),
    name       VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX unx_studio_tmdb ON studio (tmdb_id) WHERE tmdb_id IS NOT NULL;
CREATE INDEX idx_studio_name ON studio (name);

CREATE TABLE item_studio (
    item_id    BIGINT NOT NULL,
    studio_id  BIGINT NOT NULL,
    sort_order INT NOT NULL DEFAULT 0,
    PRIMARY KEY (item_id, studio_id)
);
CREATE INDEX idx_item_studio_studio ON item_studio (studio_id);
CREATE INDEX idx_item_studio_item_sort ON item_studio (item_id, sort_order);

CREATE TABLE tag (
    id         BIGSERIAL PRIMARY KEY,
    tmdb_id    VARCHAR(64),
    name       VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX unx_tag_tmdb ON tag (tmdb_id) WHERE tmdb_id IS NOT NULL;
CREATE INDEX idx_tag_name ON tag (name);

CREATE TABLE item_tag (
    item_id    BIGINT NOT NULL,
    tag_id     BIGINT NOT NULL,
    sort_order INT NOT NULL DEFAULT 0,
    PRIMARY KEY (item_id, tag_id)
);
CREATE INDEX idx_item_tag_tag ON item_tag (tag_id);
CREATE INDEX idx_item_tag_item_sort ON item_tag (item_id, sort_order);

CREATE TABLE item_image (
    id          BIGSERIAL PRIMARY KEY,
    parent_type VARCHAR(16) NOT NULL,
    parent_id   BIGINT NOT NULL,
    image_type  VARCHAR(16) NOT NULL,
    path_type   VARCHAR(16),
    path_url    TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX unx_item_image_primary ON item_image (parent_type, parent_id) WHERE image_type='primary';
CREATE INDEX idx_item_image_parent_type ON item_image (parent_type, parent_id, image_type);
CREATE INDEX idx_item_image_parent ON item_image (parent_type, parent_id);
