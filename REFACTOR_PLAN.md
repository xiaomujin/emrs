# emrs 五层重构执行方案（交付版）

> 本文档面向独立执行的 AI。目标：将现有 3-crate 工作区重构为 5-crate 严格分层架构。
> **本方案不含任何过渡/兼容设计**：不允许 re-export 转发层、不允许 feature flag、不允许双路径并存。
> 每一步的验收命令必须全部通过后才能进入下一步。

***

## 0. 给执行 AI 的硬性约束

1. **零行为变更**：不修改任何业务逻辑、SQL 语句、HTTP 响应。只做代码移动、路径修正、依赖注入改造。唯一例外是第 4 节明确记录的裁定项。
2. **每批一提交**：Phase 2/3 的每个批次 = 移动代码 + 同批修正所有消费方 use 路径 + 同批迁移对应测试 + `cargo test --workspace` 全绿 + git commit。禁止出现"先移走、下批再修路径"的中间态提交。
3. **禁止兼容层**：任何 crate 不得为了"让旧路径继续可用"而添加 `pub use` 转发（emby-proto 的既有 re-export 除外，那是它的正常 API）。路径改错就改到对，不许糊。
4. **函数级注释**：新增或改动的函数必须带中文函数级注释（项目约定）。移动的代码保持原注释。
5. **遇到本方案未覆盖的决策点**：遵循"最小改动 + 与既有风格一致"原则，并在提交信息中说明；不得引入新的抽象层。
6. 开发环境为 Windows（PowerShell），生产为 Linux。所有命令给 PowerShell 形式。

***

## 1. 现状盘点（2026-09-03 核对）

工作区：`emby-proto`、`emrs-core`、`emrs-server`（v0.1.7，edition 2024）。

emrs-core 共 43 个源文件、约 11k 行，其中 **24 个文件含 IO 依赖**（sqlx / redis / reqwest / notify / ffmpeg\_sidecar / rust-embed）。关键事实：

- `stores/mod.rs`（539 行）：聚合门面 `ItemsStore` / `PlaybackStore` + 行类型。**所有行类型（`ItemRow`** **/** **`MediaSourceRow`** **/** **`UserItemData`** **/** **`LibraryView`）都派生** **`sqlx::FromRow`**。

- `auth/`：`context.rs`（纯类型）/ `password.rs`（bcrypt 纯逻辑）/ `token.rs`（纯逻辑）+ `store.rs`（sqlx 查询）。

- `cache/`：`mod.rs` 定义 `Cache` trait + `CacheBackend`/`CacheConfig` 枚举；`memory.rs` / `redis.rs` / `facade.rs` 是实现（moka / redis）。

- `cloud/`：`mod.rs` 定义 `CloudDriver` trait + `DriverRegistry`；`http_driver.rs` 是 302 直链实现。

- `playback/`：`mod.rs`（122 行，`PlaybackBackend` trait + Redirect/Proxy/Ticket 三后端，Proxy 用 reqwest）；`ticket.rs`（jwt 纯逻辑）/ `block_cache.rs`（基于 Cache trait 的纯逻辑）/ `redirect.rs`（2 行 re-export）。

- `importer/`：`mod.rs` 定义 `Importer` 门面（持有 `Arc<Db>` + tmdb key + `Outbound`）；`tmdb.rs`（972 行，HTTP 客户端）与 `probe.rs`（834 行，ffprobe 执行）是纯 IO；`pipeline.rs` / `stages/` / `scanner/` / `filename.rs` / `nfo.rs` / `strm.rs` 是编排逻辑。

- `emby.rs`（27 行）：emby-proto 门面 re-export + `UserItemData → ViewsUserData` 的 From 转换。

- `config.rs`（372 行）：**使用 rust-embed 内嵌默认 emrs.yml**。

- `job.rs`（277 行）：JobManager（DashMap + tokio::spawn 协作取消）。

- `watcher.rs`（270 行）：notify 文件监听。

- `http_client.rs`（418 行）：reqwest 出网构造（`Outbound` / `HttpClient`）。

emrs-server：`main.rs` 装配 + `app.rs`/`state.rs` + `middleware.rs` + `routes/`（10 个路由模块）+ `emby/`（Emby 兼容 DTO 层）+ 8 个集成测试。

***

## 2. 目标架构

```
emby-proto     纯协议（不动）          serde / chrono / serde_json
emrs-core      纯领域：trait + 纯逻辑   无 sqlx / redis / reqwest / notify / ffmpeg-sidecar
emrs-infra      纯实现：全部 IO 具体    无 axum
emrs-service   纯编排：业务流程         无 axum；不直写 SQL（经 infra store）
emrs-server    纯 HTTP：路由 + 装配     不变（收窄 import 路径）
```

依赖方向严格单向：`proto ← core ← infra ← service ← server`。

**Facade 约定**：emrs-infra / emrs-service 不 re-export 下层 crate 的符号。消费方（service / server）显式同时 import 两层。这样依赖关系在代码里可见，不靠转发隐藏。

***

## 3. 关键裁定记录（执行 AI 不得重新决策）

| #   | 裁定                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        | 理由                                                                                                                                                       |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| C1  | **rust-embed 留在 emrs-core**，加入 core 允许依赖清单                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | config.rs 的内嵌默认配置是领域知识，拆分 config 得不偿失                                                                                                                    |
| C2  | **`sqlx::FromRow`** **派生的行类型（`ItemRow`/`MediaSourceRow`/`UserItemData`/`LibraryView`/`ResumeEntry`/`StreamInfo`/`PersonRow`/`ItemTaxonomy`** **等全部行类型）一律归属 emrs-infra**，随 stores 移动。emrs-core **不建 models 目录**                                                                                                                                                                                                                                                                                                                                            | 派生 sqlx::FromRow 即依赖 sqlx，类型进 core 必然破坏"core 无 sqlx"。core 的纯类型（AuthContext/DeviceInfo/UserRow/LoginEvent 等）留在原模块。**此裁定取代早前"行类型拆到 core/models"的说法**       |
| C3  | `core/emby.rs` 的 `UserItemData → ViewsUserData` From 实现**移入 emrs-server/src/emby/**（DTO 映射属地），core/emby.rs 只保留 emby-proto re-export                                                                                                                                                                                                                                                                                                                                                                                                                       | UserItemData 随 C2 进 infra，core 不再可见该类型                                                                                                                   |
| C4  | **保留 anyhow，不引入统一 AppError**。所有函数签名维持 `anyhow::Result`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | 错误体系重写是行为级变更，违反零行为变更。AppError 单独立项                                                                                                                       |
| C5  | playback：`ticket.rs`/`block_cache.rs`/`redirect.rs` 留 core；`playback/mod.rs` 中 **`PlaybackBackend`** **trait 定义 + Redirect/Ticket 后端留 core，Proxy 后端（reqwest 实现）移 emrs-infra**。trait 方法返回类型如涉及字节流，统一用 `tokio_util::io::ReaderStream<...>` 或自定义轻量包装，**禁止泄漏 reqwest/axum 类型进 core**                                                                                                                                                                                                                                                                          | Proxy 是纯 IO；trait 是三后端的公共契约                                                                                                                              |
| C6  | importer：`tmdb.rs`/`probe.rs` → infra；`pipeline.rs`/`stages/*`/`scanner/*`/`filename.rs`/`nfo.rs`/`strm.rs`/`mod.rs`(Importer 门面) → service。**Scanner 不再自己构造** **`TmdbScraper`**：构造上移到 `Importer`（service），`Scanner` 增加 `tmdb: Arc<TmdbScraper>` 字段注入（见 §5 Phase 3 签名规格）                                                                                                                                                                                                                                                                                  | 消除 service→infra 构造依赖的隐藏耦合                                                                                                                               |
| C7  | `job.rs` → emrs-service；`watcher.rs` → emrs-infra（notify 是纯 IO，JobManager 是编排）                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | tokio::spawn 属编排                                                                                                                                         |
| C8  | 简单 CRUD（settings 读写等）保留 server 直调 infra store，**不造透传 service**                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | 避免贫血透传层                                                                                                                                                  |
| C9  | 跨 store 事务一律在 service 层 `db.begin()`，事务对象作为 `Executor` 参数传给 infra store 函数。本次重构如 store 函数签名仅收 `&Db` 且无跨 store 事务调用点，**不强制改造**（现有调用方逐个核对后维持原样）                                                                                                                                                                                                                                                                                                                                                                                                             | 事务属主原则；不为假想需求改签名                                                                                                                                         |
| C10 | **SQL 查询返回规则**：store 函数返回一律 `#[derive(sqlx::FromRow)]` 命名结构体。元组仅限 ≤2 元（计数、id 对等）；≥3 元必须命名结构体。现有违规处（`claim_pending_scrape` 六元组及约 3 处 3-5 元组返回，均在 importer 消费的 store 函数）在批次 B1 顺手命名化——纯加 struct，零行为变更                                                                                                                                                                                                                                                                                                                                                       | 元组按位置解构，加列即破坏全部调用点且无编译期列名校验；FromRow 派生提供列名编译期校验                                                                                                          |
| C11 | **接口返回双轨制**（A3，否决全类型化 A2）：① 静态形状——字段集固定、所有 item 类型一致（`SystemInfoDto`/`ItemsResponse` 壳/`ViewsUserData`/`NameIdDto` 等）用类型 DTO 归 emby-proto；② 多态/动态形状（`ItemDto`/`MediaSourceDto` 系列）保留现有半类型化形态：struct 定义留在 server/emby/dto.rs，构造逻辑（类型分支/图片 tag 回退/ProviderIds 动态键）手写，**不上移 proto**。判定标准：字段集随 `item.type` 变化或含动态键 → 多态轨；否则 → 静态轨。dto.rs 中不依赖 server 上下文的纯 DTO 定义（`PersonDetailDto`/`PersonItemDto`/`ExternalUrlDto`/`RequiredHttpHeaders`，约 200 行）上移 emby-proto。**禁止**：infra store 返回 `serde_json::Value`/DTO；`Value` 不进 core/service（领域数据 JSON 如 ticket 除外） | Emby 协议的多态性（Movie/Series/Season/Episode 字段集不同、图片 tag 继承回退、flatten×skip\_serializing\_if 的 serde 坑）使全类型化退化为 Option 大杂烩，编译期校验形同虚设。dto.rs 现状已是半个 A2，卡点均为结构性 |

***

## 4. 依赖矩阵（Phase 5 终态，Phase 1 先全量后裁剪）

### emrs-core/Cargo.toml（终态）

```toml
[dependencies]
emby-proto.workspace = true
serde.workspace = true
serde_json.workspace = true
serde_yaml.workspace = true
thiserror.workspace = true
anyhow.workspace = true
async-trait.workspace = true
chrono.workspace = true
bcrypt.workspace = true
rand.workspace = true
sha2.workspace = true
uuid.workspace = true
dashmap.workspace = true
jsonwebtoken.workspace = true
regex.workspace = true
quick-xml.workspace = true       # nfo 解析若随 importer 移走则删除
rust-embed.workspace = true     # 裁定 C1
tokio = { workspace = true, features = ["sync", "time", "fs"] }  # 按 cargo check 提示裁剪
```

**禁止出现**：sqlx、redis、moka、reqwest、notify、ffmpeg-sidecar、filetime。

### emrs-infra/Cargo.toml（终态）

```toml
[dependencies]
emby-proto.workspace = true
emrs-core.workspace = true
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "sync", "fs", "io-util", "time", "process"] }
serde.workspace = true
serde_json.workspace = true
tracing.workspace = true
anyhow.workspace = true
async-trait.workspace = true
sqlx.workspace = true
redis.workspace = true
moka.workspace = true
reqwest.workspace = true
notify.workspace = true
ffmpeg-sidecar.workspace = true
rust-embed.workspace = true     # 若迁移 SQL 内嵌文件跟随 db.rs
chrono.workspace = true
uuid.workspace = true
rand.workspace = true
filetime.workspace = true
quick-xml.workspace = true
regex.workspace = true
```

### emrs-service/Cargo.toml（终态）

```toml
[dependencies]
emby-proto.workspace = true
emrs-core.workspace = true
emrs-infra = { path = "../emrs-infra" }   # 建成后在 workspace.dependencies 统一注册
tokio.workspace = true
anyhow.workspace = true
tracing.workspace = true
serde_json.workspace = true
dashmap.workspace = true
uuid.workspace = true
```

### emby-proto / emrs-server：不变（server 的依赖列表只增不减地维持，仅改 use 路径与 AppState 内容）。emby-proto 按裁定 C11 承接上移的纯 DTO 定义（`PersonDetailDto`/`PersonItemDto`/`ExternalUrlDto`/`RequiredHttpHeaders` 等），仍只需 serde 系依赖。

> 各 crate 的 `[dev-dependencies]`（tempfile / temp-env / tower util 等）随测试文件迁移。

***

## 5. 执行步骤

### Phase 0 — 基线锁定

```powershell
git switch -c refactor/layered-arch
cargo test --workspace 2>&1 | Tee-Object baseline-test.log   # 全绿才继续；失败先修复，不得带病重构
cargo clippy --workspace 2>&1 | Tee-Object baseline-clippy.log
```

### Phase 1 — 骨架搭建（一次提交）

1. 根 `Cargo.toml` 的 `members` 增加 `"crates/emrs-infra"`、`"crates/emrs-service"`；`[workspace.dependencies]` 增加 `emrs-infra = { path = "crates/emrs-infra" }`、`emrs-service = { path = "crates/emrs-service" }`。
2. 两个新 crate 建目录 + 上表 Cargo.toml（**全量依赖先给足**，Phase 5 裁剪）+ 仅含一行中文文档注释的 `lib.rs`。
3. 验收：`cargo check --workspace` 通过 → 提交 `feat(workspace): add emrs-infra and emrs-service skeletons`。

### Phase 2 — emrs-core 拆分（6 批，工作量主体）

**批次 B1 — 数据层根（含行类型裁定 C2 与 SQL 返回规则 C10，最重的一批）**

| 源（crates/emrs-core/src/）                                                          | 目标                                                                                       |
| --------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------- |
| `db.rs`                                                                           | emrs-infra/src/db.rs                                                                     |
| `stores/mod.rs`（门面 + 全部行类型）                                                       | emrs-infra/src/stores/mod.rs（整体，含 `ItemsStore`/`PlaybackStore`）                          |
| `stores/{item,library,media,image,taxonomy,user_data,scan_job,settings}_store.rs` | emrs-infra/src/stores/（同名）                                                               |
| `auth/store.rs`（AuthStore sqlx 查询）                                                | emrs-infra/src/auth/store.rs                                                             |
| `auth/context.rs` 的 `UserRow` 若派生 sqlx::FromRow → 留 infra；否则随 context 留 core      | 按派生逐字段核对                                                                                 |
| `core/emby.rs`                                                                    | core 保留 proto re-export；`UserItemData→ViewsUserData` From 移 emrs-server/src/emby/（裁定 C3） |

**同批 C10 顺手改造**：`item_store.rs` 的 `claim_pending_scrape`（六元组 → `PendingScrapeRow` 命名结构体）及其余 ≥3 元元组返回的 store 函数（用 `rg "query_as::<_, \(" crates/emrs-core/src/stores` 找全，约 4 处）命名化；调用方（importer stages/scanner）同步解构更新——纯加 struct，零行为变更。

同批必须完成：

- emrs-server 全部 `use emrs_core::{db, stores, ...}` → `use emrs_infra::...`（用 `rg "emrs_core::(db|stores|auth::store)"` 找全）。

- `emrs-core/tests/db_migrate.rs` → `emrs-infra/tests/`；`emrs-core/tests/{item_list_assembly,taxonomy_list}.rs` → `emrs-infra/tests/`。

- core 剩余代码中引用 `Db`/store 的模块（importer、job、playback、watcher、cloud、cache）——本批**临时**直接 `use emrs_infra::...`？**不行**（core 禁止依赖 infra）。正确做法：这些模块引用 `Db`/store 的文件**本批先留在 core 但把整文件一起移走**会造成乱序。因此本批策略调整为：**B1 只移 db.rs + auth/store.rs + 全部 stores，core 中所有 import 它们的模块（importer/job/playback/watcher/cloud 剩余部分）在同批先改声明为报错状态不可行——故本批将这些依赖方的编译断点交由 B2-B6 依次消除，期间** **`cargo check -p emrs-core`** **允许失败，但** **`cargo test -p emrs-server`** **必须绿**。
  **修正后的批内闸门**：B1 起以"server 全链路可编译可测试"为闸门（server 经 infra 访问 stores），core 内部断点随 B2-B6 消除，B6 完成时 `cargo check --workspace` 全绿。

> 执行提示：B1 闸门是本方案对"每批全绿"原则的唯一放宽，原因见上。若执行 AI 发现更优切法（例如把 importer 整体先行移入 service 再移 stores），可自行调整批次顺序，但**每批结束时 server 测试必须全绿**。

**批次 B2 — 缓存**：`cache/mod.rs` 的 trait + `CacheBackend`/`CacheConfig` 留 core（删掉其中 sqlx/redis 相关实现代码，若有）；`cache/{memory,redis,facade}.rs` → emrs-infra/src/cache/。core 内引用 facade 的模块（playback/block\_cache 等）本批改为……不引用实现，只引 trait（本来就是）。闸门：`cargo test -p emrs-server` 绿。

**批次 B3 — 外部驱动**：`http_client.rs` → infra；`cloud/http_driver.rs` → infra/src/cloud/（`cloud/mod.rs` 的 trait + `DriverRegistry` + `ResolvedSource` 留 core）；`importer/tmdb.rs` → infra/src/tmdb/；`importer/probe.rs` → infra/src/probe/；`watcher.rs` → infra。core 内消费方（importer scanner/stages、playback）的 `use crate::http_client::Outbound` 断点：`Outbound` 是 reqwest 配置类型 → **`Outbound`/`HttpClient`** **整体归 infra**，core 消费方在 B4/B6 移出 core 时消除引用。闸门：server 测试绿。

**批次 B4 — playback 定向拆解（裁定 C5）**：`ticket.rs`/`block_cache.rs`/`redirect.rs` 留 core；`mod.rs` 拆分——`PlaybackBackend` trait + Redirect/Ticket 实现留 core，Proxy 实现移 infra/src/playback\_proxy.rs。闸门：server 测试绿。

**批次 B5 — importer 逻辑整体上提（裁定 C6）**：`importer/{mod,pipeline,stages/*,scanner/*,filename,nfo,strm}.rs` → emrs-service/src/importer/。同时按 C6 改造 `Scanner` 构造签名（见 Phase 3 规格）。闸门：`cargo test --workspace`（本批起 core 断点应已基本消除）。

**批次 B6 — job + 收尾**：`job.rs` → service；重写 core `lib.rs` 模块树 + 中文文档注释；`rg "crate::(db|stores|http_client|watcher)" crates/emrs-core/src` 必须无输出。闸门：`cargo test --workspace` 全绿。

### Phase 3 — service 层成形（与 B5/B6 部分合并，此处给签名规格）

**Scanner 注入签名（C6）**：

```rust
// emrs-service/src/importer/scanner/mod.rs
/// 目录扫描器：注入 TMDB 刮削器与出网配置，不再自行构造。
pub struct Scanner {
    db: Arc<Db>,
    tmdb: Arc<TmdbScraper>,   // emrs_infra::tmdb::TmdbScraper
    // ...其余字段维持现状
}

/// 构造扫描器（替代原 with_outbound：TmdbScraper 由 Importer 门面构造后注入）。
pub fn with_tmdb(db: Arc<Db>, tmdb: Arc<TmdbScraper>) -> Scanner { ... }
```

`Importer` 门面（service）持 `Arc<Db>` + `tmdb_api_key` + `Arc<Outbound>`，在 `Importer::scan()` 内构造 `TmdbScraper` 传给 `Scanner`——外部行为不变。

**playback\_service**：把现散在 server `routes/playback.rs` / `routes/items/playback_info.rs` 的后端选择逻辑收拢为纯函数（输入 `ResolvedSource`/配置，输出 `PlaybackStrategy` 枚举），server 调用之。仅当该逻辑确实可抽时执行；若与 axum 类型深度纠缠，记录后跳过（不强行抽取）。

**auth\_service**：登录编排（验密→签发 token→写 `auth_login_event`）从 `routes/admin/login.rs` 抽为 service 函数，签名：`pub async fn login(db: &Db, username: &str, password: &str, device: &DeviceInfo) -> anyhow::Result<LoginOutcome>`。仅当现有逻辑可直接平移时执行，否则记录跳过。

### Phase 4 — server 收窄（一次提交）

1. 全局路径修正：模型类 → `emrs_core::`；store/驱动 → `emrs_infra::`；编排 → `emrs_service::`。
2. **C11 DTO 上移**：`emby/dto.rs` 中不依赖 server 上下文（不引用 `Db`/`ItemRow`/`AppState`）的纯 DTO 定义——`PersonDetailDto`、`PersonItemDto`、`ExternalUrlDto`、`RequiredHttpHeaders`——移入 emby-proto（新增 `person.rs` 或并入既有 `base.rs`），serde 属性原样保留；`ItemDto`/`MediaSourceDto`/`MediaStreamDto` 定义与全部构造逻辑**留在 server/emby/dto.rs 原地不动**（构造依赖 `Db` 预取批次数据，上移会把 infra 类型带进 proto，违反 C11 多态轨属地）。移动后 server 侧改 use 路径，输出 JSON 逐字节不变。
3. `AppState` 装配：`Arc<Db>`（infra）+ `Arc<dyn Cache>`（core trait，infra 实现）+ `DriverRegistry` + `JobManager`（service）。
4. `main.rs` 装配顺序：Config → Db(迁移) → Cache → DriverRegistry → Importer/Pipeline → JobManager → axum。
5. `middleware.rs` 认证改调 service（若 Phase 3 已抽 auth\_service）。
6. 闸门：`cargo test --workspace` 全绿。

### Phase 5 — 裁剪与最终验收（一次提交）

1. 按第 4 节终态矩阵裁剪三个 crate 依赖（以 `cargo check` / `cargo test` 驱动，逐个删除再验证）。
2. `emrs-core/tests/scan_job_stage.rs` 若未随 B5 迁移 → `emrs-service/tests/`。
3. 最终验收命令（**全部必须通过**）：

```powershell
cargo tree -p emrs-core | rg "sqlx|redis|reqwest|notify|ffmpeg|moka"   # 必须无输出
cargo tree -p emrs-infra | rg "axum"                                    # 必须无输出
cargo tree -p emrs-service | rg "axum"                                  # 必须无输出
cargo test --workspace                                                 # 与 baseline-test.log 对比：测试数量不减、全绿
cargo clippy --workspace -- -D warnings
```

1. 比对 `baseline-test.log` 与最终输出：**测试用例总数不得减少**，全部 PASS。

***

## 6. 验收清单（Definition of Done）

- [ ] workspace 恰好 5 个 crate，依赖方向无回指（`cargo tree` 验证）

- [ ] emrs-core 源码中 `rg "sqlx|redis::|reqwest|notify|ffmpeg_sidecar"` 无输出（rust-embed 除外，裁定 C1）

- [ ] C10：`rg "query_as::<_, \(" crates/emrs-infra/src` 命中处全部为 ≤2 元元组，无 ≥3 元元组返回

- [ ] C11：emby-proto 中无引用 `Db`/`ItemRow` 的 DTO；`ItemDto`/`MediaSourceDto` 构造逻辑仍在 server/emby/dto.rs；infra store 无 `serde_json::Value` 返回

- [ ] 全部现有测试迁移或保留并通过，数量不减

- [ ] `cargo clippy --workspace -- -D warnings` 通过

- [ ] emrs-server 二进制行为不变（8 个集成测试即验收基准）

- [ ] 提交历史按批次切分，每个提交信息说明该批内容与裁定编号

## 7. 风险与回退

| 风险                      | 缓解                                                                                     |
| ----------------------- | -------------------------------------------------------------------------------------- |
| B1 移 stores 后 core 内部断点 | 每批以 server 测试为闸门；断点在 B2-B6 收敛；B6 兜底 `rg` 检查                                            |
| scanner 改构造签名引入行为差      | `with_tmdb` 仅改注入方式，扫描逻辑零改动；`scan_job_stage` 测试把关                                       |
| Proxy 后端 trait 化设计不当    | 允许执行 AI 将 Proxy 整体留 infra 并让 trait 定义跟去 infra **仅当** core 无其他消费者时，并在提交信息记录（对裁定的唯一放宽通道） |
| 依赖裁剪引发连锁缺失              | 逐个删除 + `cargo check` 循环，不批量删                                                           |
| 任一批卡死超过 3 次尝试           | 停止，回退到上一绿色提交，记录卡点后请求人工裁定                                                               |

