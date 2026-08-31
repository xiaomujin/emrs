//! 三方言迁移集成测试。
//!
//! - sqlite：始终运行（临时文件库）
//! - mysql / postgres：设置 `EMRS_TEST_MYSQL_URL` / `EMRS_TEST_PG_URL` 时运行
//!   （docker 速起：
//!   `docker run -d -p 3306:3306 -e MYSQL_DATABASE=emrs_test -e MYSQL_ROOT_PASSWORD=root mysql:8`
//!   `docker run -d -p 5432:5432 -e POSTGRES_DB=emrs_test -e POSTGRES_PASSWORD=root postgres:16`）
//!
//! 注意：mysql/pg 测试库必须为空库或可重复迁移库（Migrator 依赖 `_sqlx_migrations` 幂等）。

use emrs_core::config::StorageConfig;
use emrs_core::db::{Db, Dialect};

/// 按功能分类的迁移名（与 `migrations/{dialect}/` 文件名一一对应）。
const MIGRATIONS: &[&str] = &[
    "0001_auth_user",
    "0002_library",
    "0003_item",
    "0004_media",
    "0005_user_data",
    "0006_system",
];

/// 建表总数（20 张业务表 + `_sqlx_migrations` 元数据表）。
const EXPECTED_TABLES: i64 = 21;

fn tmp_sqlite_dsn(tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("emrs-migrate-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("test.db");
    // Windows 绝对路径用 `sqlite:C:/...` 形式（`sqlite://` 会把盘符解析成 host）
    format!(
        "sqlite:{}?mode=rwc",
        db.to_string_lossy().replace('\\', "/")
    )
}

async fn run_acceptance(dsn: &str, dialect: Dialect) {
    let cfg = StorageConfig {
        dsn: dsn.to_string(),
        max_connections: 4,
    };
    let db = Db::connect(&cfg).await.unwrap();
    assert_eq!(db.dialect(), dialect);

    // 空库迁移
    db.migrate().await.unwrap();
    db.ping().await.unwrap();

    // 表数量：三方言均含 _sqlx_migrations（sqlite 的内部表已排除）
    let count = db.tables_count().await.unwrap();
    assert_eq!(count, EXPECTED_TABLES, "{dialect:?} 建表数量不符");

    // 幂等：重复迁移不报错、不重复建表
    db.migrate().await.unwrap();
    assert_eq!(db.tables_count().await.unwrap(), EXPECTED_TABLES);

    // schema 冒烟：关键列存在（scrape_attempts / runtime / path_type）
    let attempts: Option<i64> = sqlx::query_scalar("SELECT scrape_attempts FROM item WHERE 1 = 0")
        .fetch_optional(db.pool())
        .await
        .unwrap();
    assert_eq!(attempts, None);
    sqlx::query("SELECT runtime FROM item WHERE 1 = 0")
        .fetch_optional(db.pool())
        .await
        .unwrap();
    sqlx::query("SELECT path_type FROM item_image WHERE 1 = 0")
        .fetch_optional(db.pool())
        .await
        .unwrap();

    // 冒烟：任意业务 SQL 走 Any 池（? 占位符）
    sqlx::query("INSERT INTO \"user\" (username, password_hash) VALUES (?, ?)")
        .bind("alice")
        .bind("x")
        .execute(db.pool())
        .await
        .unwrap();
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM \"user\"")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn sqlite_migrate_and_smoke() {
    run_acceptance(&tmp_sqlite_dsn("sqlite"), Dialect::Sqlite).await;
}

#[tokio::test]
async fn mysql_migrate_and_smoke() {
    let Ok(dsn) = std::env::var("EMRS_TEST_MYSQL_URL") else {
        eprintln!("跳过：未设置 EMRS_TEST_MYSQL_URL");
        return;
    };
    run_acceptance(&dsn, Dialect::Mysql).await;
}

#[tokio::test]
async fn postgres_migrate_and_smoke() {
    let Ok(dsn) = std::env::var("EMRS_TEST_PG_URL") else {
        eprintln!("跳过：未设置 EMRS_TEST_PG_URL");
        return;
    };
    run_acceptance(&dsn, Dialect::Postgres).await;
}

/// 三方言 DDL 静态一致性：表名集合必须完全一致，down 必须覆盖全部业务表。
/// （防"改一份漏两份"漂移；真库迁移由上面的 env-gated 测试覆盖）
#[test]
fn dialect_ddl_tables_consistent() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    // Parse all migration up files per dialect and collect CREATE TABLE names.
    let parse_all = |dialect: &str| -> Vec<String> {
        let mut tables = Vec::new();
        for prefix in MIGRATIONS {
            let sql = std::fs::read_to_string(root.join(dialect).join(format!("{prefix}.up.sql")))
                .unwrap();
            for line in sql.lines() {
                let trimmed = line.trim_start();
                if trimmed.to_uppercase().starts_with("CREATE TABLE ")
                    && !trimmed.to_uppercase().contains(" PARTITION OF ")
                {
                    let name = trimmed
                        .trim_start_matches("CREATE TABLE ")
                        .split('(')
                        .next()
                        .unwrap()
                        .trim()
                        .trim_matches(|c| c == '`' || c == '"')
                        .to_string();
                    tables.push(name);
                }
            }
        }
        tables
    };

    let sqlite = parse_all("sqlite");
    let mysql = parse_all("mysql");
    let pg = parse_all("postgres");
    // 20 business tables (excluding _sqlx_migrations)
    assert_eq!(
        sqlite.len(),
        20,
        "sqlite 建表数应 20，实际 {}",
        sqlite.len()
    );
    assert_eq!(mysql, sqlite, "mysql 表名集合与 sqlite 不一致");
    assert_eq!(pg, sqlite, "postgres 表名集合与 sqlite 不一致");

    // down 覆盖所有业务表（collect from all down files）
    let mut all_down = String::new();
    for prefix in MIGRATIONS {
        all_down.push_str(
            &std::fs::read_to_string(root.join("sqlite").join(format!("{prefix}.down.sql")))
                .unwrap(),
        );
    }
    for t in &sqlite {
        let table_name = t.trim_matches('"');
        assert!(all_down.contains(table_name), "down 迁移缺少表 {t}");
    }
}
