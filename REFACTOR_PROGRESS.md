# 五层重构执行进度（配套 REFACTOR_PLAN.md）

> 更新时间：2026-09-03。执行 AI 换手时先读本文件再读 REFACTOR_PLAN.md。
> 约定：每批完成 = 移动 + 路径修正 + 测试迁移 + `cargo test --workspace` 全绿 + commit。

## 当前状态

- 分支：`refactor/layered-arch`；基线：201 个测试全绿（`baseline-test.log`）、clippy 干净（`baseline-clippy.log`）。
- **Phase 0（基线锁定）✅**、**Phase 1（骨架）✅**（commit `78382fe`）、
  **Phase 2 全部六批 + Phase 4 的路径收窄部分已合并为一个原子批完成 ✅**（commit `926541a`）。
- 当前测试 201/201 全绿，clippy --workspace --all-targets 0 警告。

## 实际终态 vs 方案快照（执行中确认的偏离，已在 commit 926541a 备案）

1. playback 无 `PlaybackBackend` trait / Proxy 后端（方案 §1 快照有误）→ B4 无实体可移；
   `block_cache.rs`（磁盘 IO + filetime）归 **infra**（core 禁用清单优先），PlaybackRouter 留 core。
2. `scanner`（fs 遍历）整体归 **infra**（风险表放宽通道），`filename/nfo/strm` 随唯一消费者归 infra；
   scanner 保持公开构造器（`new/with_outbound/with_rate`），未引入 C6 的 `with_tmdb` 注入
   （infra 内构造，无隐藏耦合）。
3. `watcher` 归 infra 但其依赖的 Pipeline/ScanStage 归 service → 拆解：
   watcher 直接经 infra store 入队（`library_store::library_id_for_path` + `scan_job_store::create`，
   新函数收编原 `Scanner::library_id_for_path`/`ScanStage::enqueue_library_scan` 的 watch 路径），
   唤醒走 **core 新增 `scan.rs::ScanWaker` trait**（`fn wake_scan`），service 的 Pipeline 实现，
   server main.rs `LibraryWatcher::with_waker(db, pipeline)` 注入。
4. C3：`UserItemData` 归 infra 后固有 impl 受孤儿规则限制 → server `emby/user_data.rs`
   **自由函数** `to_views_user_data(&UserItemData)`（emby/mod.rs 有 `pub(crate) use`）。
5. `DriverRegistry::new(db,cfg)` 参数本被忽略 → core `new()` 无参 + infra `cloud::build_registry()`。
6. C10 全部 ≥3 元元组已命名化（PendingScrapeRow/ParentRow/MediaBatchRow/EpisodeCountRow/
   ItemNameRow/ItemPersonRow/ItemCountsRow/PendingScanJobRow/JobProgressRow/UserAuthRow/
   TokenVerifyRow/PeopleExistingRow/SourcePathRow/LibraryBriefRow/ImageIdRow）。
   COUNT/COALESCE 列加了 AS 别名以供 FromRow 映射（语义零变更）。
7. service 不直写 SQL：pipeline/stages 的内联 SQL 已逐字上收 infra store
   （media_store 7 个新函数、item_store 2 个、library_store 2 个）。

## 剩余工作

### Phase 3（service 成形，其余部分）
- [ ] playback_service：server `routes/playback.rs`/`routes/items/playback_info.rs` 的后端选择
      逻辑若可抽则抽为 service 纯函数；与 axum 深度纠缠则记录跳过（方案允许）。
- [ ] auth_service：`routes/admin/login.rs` 登录编排若可平移则抽 `login(db,username,password,device) ->
      LoginOutcome`；不可直接平移则记录跳过。
- [ ] C6 的 scanner 注入规格已裁定不做（见上偏离 2），无需再动。

### Phase 4（server 收窄，剩余项）
- [x] 全局 use 路径修正（已在 926541a 完成）。
- [ ] **C11 DTO 上移**：`emrs-server/src/emby/dto.rs` 中 `PersonDetailDto`/`PersonItemDto`/
      `ExternalUrlDto`/`RequiredHttpHeaders`（约 200 行，不引用 Db/ItemRow/AppState 的纯定义）
      → emby-proto（新增 person.rs 或并入 base.rs，serde 属性原样）；`ItemDto`/`MediaSourceDto`
      构造逻辑**原地不动**。
- [ ] 检查 `middleware.rs` 认证是否需要改调 service（若抽了 auth_service）。

### Phase 5（依赖裁剪 + 最终验收）
- [ ] 按方案 §4 终态矩阵裁剪 core/infra/service 的 Cargo.toml（逐个删 + cargo check 驱动）。
      注意：service 现依赖 chrono/regex/quick-xml（pipeline/scrape 用），jsonwebtoken 无；
      infra 实际用到 dashmap（block_cache）、无 jsonwebtoken。core 实际需要：tracing（playback 用）、
      dashmap（无？核对 block_cache 已走→core 可能不需要 dashmap/quick-xml（nfo 已走）/regex（filename 已走）/
      sha2（block_cache 已走）——逐个用 cargo check 验证后删。
- [ ] 最终验收（全过）：
      - `cargo tree -p emrs-core | rg "sqlx|redis|reqwest|notify|ffmpeg|moka"` 无输出
      - `cargo tree -p emrs-infra | rg axum`、`cargo tree -p emrs-service | rg axum` 无输出
      - `cargo test --workspace` ≥201 全绿（对比 baseline-test.log）
      - `cargo clippy --workspace -- -D warnings`
      - DoD 清单（方案 §6）：5 crate 方向无回指；core rg 干净；infra query_as 元组 ≤2 元；
        emby-proto 无 Db/ItemRow 引用；ItemDto 构造留 server；测试数量不减。

## 提交历史
1. `9bc0006` docs: 方案文档
2. `78382fe` Phase 1 骨架
3. `926541a` Phase 2 主体（B1-B6 合并）+ Phase 4 路径收窄
