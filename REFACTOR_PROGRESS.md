# 五层重构执行进度（配套 REFACTOR_PLAN.md）

> 状态：**全部完成（Phase 0–5）**。更新时间：2026-09-03。
> 执行记录保留供回溯；回退锚点见文末提交历史。

## 完成概览

- 分支 `refactor/layered-arch`；基线 201 测试全绿（`baseline-test.log`）→ 终态 201/201 全绿、
  `cargo clippy --workspace --all-targets -- -D warnings` 通过。
- 终态 5 crate：`emby-proto`（纯协议）← `emrs-core`（trait+纯逻辑）← `emrs-infra`（IO 实现）
  ← `emrs-service`（编排）← `emrs-server`（HTTP 装配）。`cargo tree` 验证无回指。
- Phase 0/1：commit `78382fe`；Phase 2 主体（B1–B6 合并原子批）+ Phase 4 路径收窄：`926541a`；
  Phase 4 C11 DTO 上移：`523b014`；Phase 3 auth_service：`7272267`；Phase 5 裁剪：`cf6f512`。

## 关键裁定偏离（备案于 commit `926541a` / `7272267`）

1. playback 无 Proxy 后端 / PlaybackBackend trait（方案快照与实际代码有出入）→ B4 无实体可移；
   `block_cache.rs`（磁盘 IO + filetime）归 infra（core 禁用清单优先），PlaybackRouter 留 core。
2. scanner（fs 遍历）整体归 infra（风险表放宽通道），filename/nfo/strm 随唯一消费者归 infra；
   scanner 保持公开构造器，未做 C6 `with_tmdb` 注入（infra 内构造无隐藏耦合）。
3. watcher（infra）→ Pipeline（service）冲突：watcher 直接经 infra store 入队
   （`library_store::library_id_for_path` + `scan_job_store::create`），唤醒走 core 新增
   `scan.rs::ScanWaker` trait，service 的 Pipeline 实现、server 注入。
4. C3：`UserItemData` 归 infra 后固有 impl 受孤儿规则限制 → server `emby/user_data.rs`
   自由函数 `to_views_user_data(&UserItemData)`。
5. `DriverRegistry::new(db,cfg)` 参数本被忽略 → core `new()` 无参 + infra `cloud::build_registry()`。
6. C10 全部 ≥3 元元组命名化（15 个行类型；COUNT/COALESCE 列补 AS 别名，语义零变更）。
7. service 不直写 SQL：pipeline/stages 内联 SQL 逐字上收 infra store（11 个新 store 函数）。
8. playback_service 按方案跳过（选择树薄且与 axum 深度纠缠）；auth_service 已抽取
   （`emrs_service::auth::admin_login`，HTTP 映射留 server）。

## 已知遗留（非本重构范围）

- 测试隔离脆弱点：`emrs-server/tests/*` 的临时库目录名 `emrs-{suite}-{pid}-{n}` 在 Windows
  pid 复用且 %TEMP% 残留旧目录时会撞库（`mode=rwc` 打开旧库 → UNIQUE 冲突），偶发假失败；
  清理 %TEMP%/emrs-* 后稳定全绿。建议后续给目录名加时间戳/清理钩子（test-only 改动）。
- C4：错误体系仍为 anyhow（AppError 单独立项，方案已排除）。

## 验收命令（复验用）

```bash
cargo tree -p emrs-core | grep -E "sqlx|redis|reqwest|notify|ffmpeg|moka"   # 无输出
cargo tree -p emrs-infra | grep axum                                        # 无输出
cargo tree -p emrs-service | grep axum                                      # 无输出
cargo test --workspace        # 201/201，与 baseline-test.log 持平
cargo clippy --workspace --all-targets -- -D warnings
```
