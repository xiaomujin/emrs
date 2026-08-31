-- 0003_item: item（单表多态）+ genre / people / studio / tag 规范表及关联表 + item_image
-- MySQL: A class = functional CASE unique indexes; B class = plain index
-- item.type ∈ movie/series/season/episode，parent_id 自引用（episode→season→series）

CREATE TABLE item (
    id               BIGINT AUTO_INCREMENT PRIMARY KEY,
    type             VARCHAR(16) NOT NULL,
    parent_id        BIGINT,
    library_id       BIGINT NOT NULL,
    is_virtual       TINYINT(1) NOT NULL DEFAULT 0,
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
    date_air         DATETIME(6),
    end_date         DATETIME(6),
    runtime          INT,
    production_year  INT,
    status           VARCHAR(32),
    season_number    INT,
    episode_number   INT,
    community_rating DECIMAL(3,1),
    official_rating  VARCHAR(16),
    created_at       DATETIME(6) NOT NULL DEFAULT NOW(6),
    updated_at       DATETIME(6) NOT NULL DEFAULT NOW(6)
);
CREATE UNIQUE INDEX unx_item_movie_tmdb     ON item ((CASE WHEN type='movie'  AND tmdb_id IS NOT NULL THEN tmdb_id END));
CREATE UNIQUE INDEX unx_item_series_tmdb    ON item ((CASE WHEN type='series' AND tmdb_id IS NOT NULL THEN tmdb_id END));
CREATE UNIQUE INDEX unx_item_season_parent  ON item ((CASE WHEN type='season'  THEN parent_id END), (CASE WHEN type='season' THEN season_number END));
CREATE UNIQUE INDEX unx_item_episode_parent ON item ((CASE WHEN type='episode' THEN parent_id END), (CASE WHEN type='episode' THEN episode_number END));
CREATE INDEX idx_item_parent ON item (parent_id);
CREATE INDEX idx_item_library ON item (library_id);
CREATE INDEX idx_item_scrape_status ON item (scrape_status);
CREATE INDEX idx_item_type ON item (type);

CREATE TABLE genre (
    id         BIGINT AUTO_INCREMENT PRIMARY KEY,
    tmdb_id    VARCHAR(64),
    name       VARCHAR(255) NOT NULL,
    created_at DATETIME(6) NOT NULL DEFAULT NOW(6),
    updated_at DATETIME(6) NOT NULL DEFAULT NOW(6)
);
CREATE UNIQUE INDEX unx_genre_tmdb ON genre ((CASE WHEN tmdb_id IS NOT NULL THEN tmdb_id END));
CREATE INDEX idx_genre_name ON genre (name);

CREATE TABLE item_genre (
    item_id  BIGINT NOT NULL,
    genre_id BIGINT NOT NULL,
    PRIMARY KEY (item_id, genre_id)
);
CREATE INDEX idx_item_genre_genre ON item_genre (genre_id);

CREATE TABLE people (
    id            BIGINT AUTO_INCREMENT PRIMARY KEY,
    tmdb_id       VARCHAR(64) NOT NULL,
    name          VARCHAR(255) NOT NULL,
    original_name VARCHAR(255),
    gender        SMALLINT NOT NULL DEFAULT 0,
    description   TEXT,
    birthday      DATE,
    deathday      DATE,
    created_at    DATETIME(6) NOT NULL DEFAULT NOW(6),
    updated_at    DATETIME(6) NOT NULL DEFAULT NOW(6),
    UNIQUE KEY uk_people_tmdb (tmdb_id)
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
    id         BIGINT AUTO_INCREMENT PRIMARY KEY,
    tmdb_id    VARCHAR(64),
    name       VARCHAR(255) NOT NULL,
    created_at DATETIME(6) NOT NULL DEFAULT NOW(6),
    updated_at DATETIME(6) NOT NULL DEFAULT NOW(6)
);
CREATE UNIQUE INDEX unx_studio_tmdb ON studio ((CASE WHEN tmdb_id IS NOT NULL THEN tmdb_id END));
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
    id         BIGINT AUTO_INCREMENT PRIMARY KEY,
    tmdb_id    VARCHAR(64),
    name       VARCHAR(255) NOT NULL,
    created_at DATETIME(6) NOT NULL DEFAULT NOW(6),
    updated_at DATETIME(6) NOT NULL DEFAULT NOW(6)
);
CREATE UNIQUE INDEX unx_tag_tmdb ON tag ((CASE WHEN tmdb_id IS NOT NULL THEN tmdb_id END));
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
    id          BIGINT AUTO_INCREMENT PRIMARY KEY,
    parent_type VARCHAR(16) NOT NULL,
    parent_id   BIGINT NOT NULL,
    image_type  VARCHAR(16) NOT NULL,
    path_type   VARCHAR(16),
    path_url    TEXT,
    created_at  DATETIME(6) NOT NULL DEFAULT NOW(6),
    updated_at  DATETIME(6) NOT NULL DEFAULT NOW(6)
);
CREATE UNIQUE INDEX unx_item_image_primary ON item_image ((CASE WHEN image_type='primary' THEN parent_type END), (CASE WHEN image_type='primary' THEN parent_id END));
CREATE INDEX idx_item_image_parent_type ON item_image (parent_type, parent_id, image_type);
CREATE INDEX idx_item_image_parent ON item_image (parent_type, parent_id);
