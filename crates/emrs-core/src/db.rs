//! 数据库层：sqlx `Any` 统一池 + 三方言迁移。
//!
//! 关键约定：
//! - **任何池创建前必须调用 `sqlx::any::install_default_drivers()`**（[`Db::connect`] 内已做）
//! - 迁移按方言分仓：`migrations/{sqlite|mysql|postgres}/`，版本文件成对
//!   `<版本>_<名称>.up.sql` / `.down.sql`（6 个迁移按功能分类：auth_user /
//!   library / item / media / user_data / system，覆盖 20 张业务表；即清理后的最终形态，
//!   旧库不兼容，需重建）。迁移 SQL 由 `sqlx::migrate!` **编译期内嵌**进二进制，
//!   运行时不依赖源码目录（见 [`Dialect::migrator`]）
//! - JSON 类列在 Any 驱动下一律按 TEXT 读写，serde 反序列化在应用层完成
//! - PG/MySQL 分区表迁移后自动创建当月/下月分区（[`Db::ensure_partitions`]）

use anyhow::{Context, Result, bail};
use sqlx::AnyPool;
use sqlx::any::AnyPoolOptions;
use sqlx::migrate::Migrator;

use crate::config::StorageConfig;

/// 三方言迁移在编译期内嵌进二进制（`sqlx::migrate!` 读取 `migrations/<dialect>/`），
/// 运行时不再依赖源码目录，发布物（Docker / 独立二进制）自包含。
/// 与旧 `Migrator::new(dir)` 读盘校验和算法一致，已应用迁移不会因换方式而失配。
static SQLITE_MIGRATOR: Migrator = sqlx::migrate!("migrations/sqlite");
static MYSQL_MIGRATOR: Migrator = sqlx::migrate!("migrations/mysql");
static POSTGRES_MIGRATOR: Migrator = sqlx::migrate!("migrations/postgres");

impl Dialect {
    /// 返回本方言编译期内嵌的迁移器。
    pub fn migrator(self) -> &'static Migrator {
        match self {
            Dialect::Sqlite => &SQLITE_MIGRATOR,
            Dialect::Mysql => &MYSQL_MIGRATOR,
            Dialect::Postgres => &POSTGRES_MIGRATOR,
        }
    }
}

/// 当前数据库方言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    Mysql,
    Postgres,
}

impl Dialect {
    /// 从 DSN 前缀识别方言。
    pub fn from_dsn(dsn: &str) -> Result<Self> {
        if dsn.starts_with("sqlite:") {
            Ok(Dialect::Sqlite)
        } else if dsn.starts_with("mysql:") || dsn.starts_with("mariadb:") {
            Ok(Dialect::Mysql)
        } else if dsn.starts_with("postgres:") || dsn.starts_with("postgresql:") {
            Ok(Dialect::Postgres)
        } else {
            bail!("无法识别的存储 DSN（支持 sqlite:/mysql:/postgres:）: {dsn}")
        }
    }

    /// 统计当前库中用户表数量的 SQL（迁移冒烟用）。
    pub fn tables_count_sql(self) -> &'static str {
        match self {
            Dialect::Sqlite => {
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'"
            }
            Dialect::Mysql => {
                "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE() AND table_type = 'BASE TABLE'"
            }
            Dialect::Postgres => {
                "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = 'public' AND table_type = 'BASE TABLE'"
            }
        }
    }
}

/// 统一数据库入口：Any 池 + 方言标记。
pub struct Db {
    pool: AnyPool,
    dialect: Dialect,
}

impl Db {
    /// 建池。sqlite 文件库会自动创建父目录。
    pub async fn connect(cfg: &StorageConfig) -> Result<Self> {
        let dialect = Dialect::from_dsn(&cfg.dsn)?;
        if dialect == Dialect::Sqlite {
            ensure_sqlite_parent_dir(&cfg.dsn)?;
        }
        // 必须先于任何 Any 池创建
        sqlx::any::install_default_drivers();
        let pool = AnyPoolOptions::new()
            .max_connections(cfg.max_connections)
            .connect(&cfg.dsn)
            .await
            .with_context(|| format!("连接数据库失败: {}", cfg.dsn))?;
        Ok(Self { pool, dialect })
    }

    pub fn dialect(&self) -> Dialect {
        self.dialect
    }

    pub fn pool(&self) -> &AnyPool {
        &self.pool
    }

    /// 按方言执行编译期内嵌的迁移；已应用的版本自动跳过（幂等）。
    pub async fn migrate(&self) -> Result<()> {
        self.dialect
            .migrator()
            .run(&self.pool)
            .await
            .context("执行迁移失败")?;
        // PG/MySQL: 自动创建当月/下月分区
        if self.dialect != Dialect::Sqlite {
            self.ensure_partitions().await;
        }
        Ok(())
    }

    /// 连通性检查。
    pub async fn ping(&self) -> Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .context("ping 失败")?;
        Ok(())
    }

    /// 当前用户表数量（不含迁移元数据表，含则加 1——两版兼容这里返回原始计数）。
    /// 注意：`_sqlx_migrations` 在 sqlite 被排除，mysql/pg 会计入，测试断言用 `>=`。
    pub async fn tables_count(&self) -> Result<i64> {
        let (n,): (i64,) = sqlx::query_as(self.dialect.tables_count_sql())
            .fetch_one(&self.pool)
            .await
            .context("统计表数量失败")?;
        Ok(n)
    }
}

/// 分区表名列表（PG/MySQL 共用）。
/// （cloud_request_log / playback_activity / security_audit_event 已随迁移 0008 删除）
const PARTITION_TABLES: &[&str] = &["auth_login_event"];

impl Db {
    /// 为 PG/MySQL 分区表创建当月和下月分区。
    /// 失败不阻断迁移（降级：旧分区仍可用，下次启动补建）。
    async fn ensure_partitions(&self) {
        let now = chrono::Utc::now();
        let months = [
            now.format("%Y-%m").to_string(),
            (now + chrono::Duration::days(31))
                .format("%Y-%m")
                .to_string(),
        ];
        for table in PARTITION_TABLES {
            for m in &months {
                let part_name = format!("{}_p_{}", table, m.replace('-', "_"));
                let start = format!("{}-01", m);
                let end = {
                    let ym = chrono::NaiveDate::parse_from_str(&format!("{}-01", m), "%Y-%m-%d")
                        .unwrap_or(now.date_naive());
                    (ym + chrono::Months::new(1)).format("%Y-%m-%d").to_string()
                };
                let sql = match self.dialect {
                    Dialect::Postgres => format!(
                        "CREATE TABLE IF NOT EXISTS {} PARTITION OF {} FOR VALUES FROM ('{}') TO ('{}')",
                        part_name, table, start, end
                    ),
                    Dialect::Mysql => format!(
                        "ALTER TABLE {} ADD PARTITION IF NOT EXISTS (PARTITION {} VALUES LESS THAN (TO_DAYS('{}')))",
                        table, part_name, end
                    ),
                    Dialect::Sqlite => unreachable!(),
                };
                let _ = sqlx::query(&sql).execute(&self.pool).await;
            }
        }
    }
}

/// sqlite 文件库自动建父目录；内存库跳过。
fn ensure_sqlite_parent_dir(dsn: &str) -> Result<()> {
    let path = dsn
        .trim_start_matches("sqlite://")
        .trim_start_matches("sqlite:")
        .split('?')
        .next()
        .unwrap_or_default();
    if path.is_empty() || path == ":memory:" {
        return Ok(());
    }
    let p = std::path::Path::new(path);
    if let Some(parent) = p.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建 sqlite 目录失败: {}", parent.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect_from_dsn() {
        assert_eq!(Dialect::from_dsn("sqlite://a.db").unwrap(), Dialect::Sqlite);
        assert_eq!(
            Dialect::from_dsn("mysql://u:p@h/db").unwrap(),
            Dialect::Mysql
        );
        assert_eq!(
            Dialect::from_dsn("postgres://u:p@h/db").unwrap(),
            Dialect::Postgres
        );
        assert!(Dialect::from_dsn("redis://x").is_err());
    }

    #[test]
    fn migrations_are_embedded() {
        for d in [Dialect::Sqlite, Dialect::Mysql, Dialect::Postgres] {
            let m = d.migrator();
            assert!(
                m.migrations.len() >= 6,
                "{:?} 内嵌迁移数异常: {}",
                d,
                m.migrations.len()
            );
            assert!(
                m.migrations.iter().any(|mig| mig.version == 1
                    && mig.description.contains("auth")
                    && !mig.sql.is_empty()),
                "{:?} 缺少已内嵌的 0001_auth_user: {:?}",
                d,
                m.migrations
                    .iter()
                    .map(|x| (x.version, &x.description))
                    .collect::<Vec<_>>()
            );
        }
    }
}
