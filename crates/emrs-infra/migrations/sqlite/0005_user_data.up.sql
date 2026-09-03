-- 0005_user_data: user_item_data（收藏 / 已看 / 播放进度）
-- playback_position_ticks 本身是 ticks 直接存；mark_started 开播即 play_count+1

CREATE TABLE user_item_data (
    id                       INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id                  INTEGER NOT NULL,
    item_id                  INTEGER NOT NULL,
    played                   INTEGER NOT NULL DEFAULT 0,
    play_count               INTEGER NOT NULL DEFAULT 0,
    playback_position_ticks  INTEGER,
    last_played_date         TEXT,
    is_favorite              INTEGER NOT NULL DEFAULT 0,
    created_at               TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at               TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE UNIQUE INDEX unx_user_item_data ON user_item_data (user_id, item_id);
CREATE INDEX idx_uid_user_updated ON user_item_data (user_id, updated_at DESC);
CREATE INDEX idx_uid_user_favorite ON user_item_data (user_id) WHERE is_favorite=1;
CREATE INDEX idx_uid_user_played ON user_item_data (user_id) WHERE played=1;
CREATE INDEX idx_uid_item ON user_item_data (item_id);
-- Resume / NextUp frontier：追过的剧按 updated_at 排序（play_count>0），加速 Step A 聚合与 movie 分支。
CREATE INDEX idx_uid_user_played_recent ON user_item_data (user_id, updated_at DESC) WHERE play_count > 0;
