-- 0005_user_data: user_item_data（收藏 / 已看 / 播放进度）
-- PostgreSQL: A class = native partial unique indexes; B class = partial indexes
-- playback_position_ticks 本身是 ticks 直接存；mark_started 开播即 play_count+1

CREATE TABLE user_item_data (
    id                      BIGSERIAL PRIMARY KEY,
    user_id                 BIGINT NOT NULL,
    item_id                 BIGINT NOT NULL,
    played                  BOOLEAN NOT NULL DEFAULT false,
    play_count              INT NOT NULL DEFAULT 0,
    playback_position_ticks BIGINT,
    last_played_date        TIMESTAMPTZ,
    is_favorite             BOOLEAN NOT NULL DEFAULT false,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX unx_user_item_data ON user_item_data (user_id, item_id);
CREATE INDEX idx_uid_user_updated ON user_item_data (user_id, updated_at DESC);
CREATE INDEX idx_uid_user_favorite ON user_item_data (user_id) WHERE is_favorite=true;
CREATE INDEX idx_uid_user_played ON user_item_data (user_id) WHERE played=true;
CREATE INDEX idx_uid_item ON user_item_data (item_id);
-- Resume / NextUp frontier：追过的剧按 updated_at 排序（play_count>0），加速 Step A 聚合与 movie 分支。
CREATE INDEX idx_uid_user_played_recent ON user_item_data (user_id, updated_at DESC) WHERE play_count > 0;
