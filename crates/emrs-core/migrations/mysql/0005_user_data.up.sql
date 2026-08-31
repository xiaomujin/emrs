-- 0005_user_data: user_item_data（收藏 / 已看 / 播放进度）
-- MySQL: A class = functional CASE unique indexes; B class = plain index
-- playback_position_ticks 本身是 ticks 直接存；mark_started 开播即 play_count+1

CREATE TABLE user_item_data (
    id                      BIGINT AUTO_INCREMENT PRIMARY KEY,
    user_id                 BIGINT NOT NULL,
    item_id                 BIGINT NOT NULL,
    played                  TINYINT(1) NOT NULL DEFAULT 0,
    play_count              INT NOT NULL DEFAULT 0,
    playback_position_ticks BIGINT,
    last_played_date        DATETIME(6),
    is_favorite             TINYINT(1) NOT NULL DEFAULT 0,
    created_at              DATETIME(6) NOT NULL DEFAULT NOW(6),
    updated_at              DATETIME(6) NOT NULL DEFAULT NOW(6),
    UNIQUE KEY uk_user_item_data (user_id, item_id)
);
CREATE INDEX idx_uid_user_updated ON user_item_data (user_id, updated_at DESC);
CREATE INDEX idx_uid_user_favorite ON user_item_data (user_id);
CREATE INDEX idx_uid_user_played ON user_item_data (user_id);
CREATE INDEX idx_uid_item ON user_item_data (item_id);
-- Resume / NextUp frontier：B 类条件索引，MySQL 降级为普通复合索引（无部分索引）。
CREATE INDEX idx_uid_user_played_recent ON user_item_data (user_id, play_count, updated_at DESC);
