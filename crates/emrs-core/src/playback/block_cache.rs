//! 分块缓存（BlockCacheBackend，GDrive 迁移）：
//! 磁盘块缓存热点区间，miss 块回源拉取后落盘复用。
//!
//! 用途：网盘驱动代理播放的热点加速、ffprobe 头部探测复用、字幕小文件整缓存。
//! key = 媒体标识（如 uuid），块文件 = `{dir}/{key}-{idx}.bin`，
//! 超过 `max_blocks` 时按 mtime 淘汰最旧块。

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// 块缓存配置。
#[derive(Debug, Clone)]
pub struct BlockCacheConfig {
    /// 缓存目录（如 `data/block-cache`）。
    pub dir: PathBuf,
    /// 块大小（默认 4 MiB）。
    pub block_size: u64,
    /// 块数上限（默认 1024 块 = 4 GiB）。
    pub max_blocks: usize,
    /// 单次回源拉取超时。
    pub fetch_timeout: Duration,
}

impl Default for BlockCacheConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("data/block-cache"),
            block_size: 4 * 1024 * 1024,
            max_blocks: 1024,
            fetch_timeout: Duration::from_secs(30),
        }
    }
}

/// 磁盘分块缓存。
pub struct BlockCache {
    cfg: BlockCacheConfig,
    /// 写块互斥（同 key 同块并发回源合并；跨 key 仍并行）。
    write_locks: dashmap::DashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>,
}

/// key 派生稳定文件名前缀（防路径穿越/非法字符）。
fn key_hash(key: &str) -> String {
    let mut h = Sha256::new();
    h.update(key.as_bytes());
    let d = h.finalize();
    d.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

impl BlockCache {
    pub fn new(cfg: BlockCacheConfig) -> Self {
        Self {
            cfg,
            write_locks: dashmap::DashMap::new(),
        }
    }

    pub fn config(&self) -> &BlockCacheConfig {
        &self.cfg
    }

    fn block_path(&self, hashed: &str, idx: u64) -> PathBuf {
        self.cfg.dir.join(format!("{hashed}-{idx:08x}.bin"))
    }

    fn lock_for(&self, hashed: &str, idx: u64) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        self.write_locks
            .entry(format!("{hashed}:{idx}"))
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// 读单块：本地命中直接读；miss 时调 `fetch(offset)` 回源并落盘。
    /// `fetch` 返回的字节数可能小于块大小（文件尾部）。
    pub async fn read_block<F, Fut>(&self, key: &str, idx: u64, fetch: F) -> Result<Vec<u8>>
    where
        F: FnOnce(u64) -> Fut,
        Fut: std::future::Future<Output = Result<Vec<u8>>>,
    {
        let hashed = key_hash(key);
        let path = self.block_path(&hashed, idx);
        if let Ok(meta) = tokio::fs::metadata(&path).await
            && meta.len() > 0
        {
            let data = tokio::fs::read(&path).await.context("读缓存块失败")?;
            if data.len() as u64 == meta.len() {
                // touch mtime（LRU 语义）
                let _ = filetime::set_file_mtime(&path, filetime::FileTime::now());
                return Ok(data);
            }
        }

        // miss：互斥回源（并发同块合并为一次）
        let lock = self.lock_for(&hashed, idx);
        let _guard = lock.lock().await;
        // double-check
        if let Ok(meta) = tokio::fs::metadata(&path).await
            && meta.len() > 0
        {
            return tokio::fs::read(&path).await.context("读缓存块失败");
        }

        let offset = idx * self.cfg.block_size;
        let data = tokio::time::timeout(self.cfg.fetch_timeout, fetch(offset))
            .await
            .context("回源拉块超时")??;

        if !data.is_empty()
            && let Err(e) = self.write_block(&path, &data).await
        {
            tracing::warn!(error = %e, "缓存块写盘失败（不影响本次读取）");
        }
        Ok(data)
    }

    async fn write_block(&self, path: &Path, data: &[u8]) -> Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("创建缓存目录失败")?;
        }
        // 临时文件 + rename 原子落盘（避免半块被并发读到）
        let tmp = path.with_extension("tmp");
        tokio::fs::write(&tmp, data).await.context("写缓存块失败")?;
        tokio::fs::rename(&tmp, path)
            .await
            .context("缓存块落盘失败")?;
        self.evict_if_needed().await;
        Ok(())
    }

    /// 读区间 `[start, end)`（end=None 表示读到文件尾）：
    /// 按块拼接返回。`fetch(offset)` 需返回从 offset 起的块大小数据
    /// （或到文件尾的剩余数据）。
    pub async fn read_range<F, Fut>(
        &self,
        key: &str,
        start: u64,
        end: Option<u64>,
        fetch: F,
    ) -> Result<Vec<u8>>
    where
        F: Fn(u64) -> Fut + Clone,
        Fut: std::future::Future<Output = Result<Vec<u8>>>,
    {
        let bs = self.cfg.block_size;
        let first_idx = start / bs;
        let in_block_off = (start % bs) as usize;
        let mut out: Vec<u8> = Vec::new();
        let mut idx = first_idx;
        loop {
            let block = {
                let f = fetch.clone();
                self.read_block(key, idx, f).await?
            };
            if block.is_empty() {
                break;
            }
            let take_from = if idx == first_idx { in_block_off } else { 0 };
            let slice = &block[take_from.min(block.len())..];
            out.extend_from_slice(slice);
            if let Some(e) = end
                && out.len() as u64 + start >= e
            {
                out.truncate((e - start) as usize);
                break;
            }
            if (block.len() as u64) < bs {
                break; // 尾块
            }
            idx += 1;
        }
        Ok(out)
    }

    /// 缓存目录内块数超限时，按 mtime 淘汰最旧块直至达标。
    async fn evict_if_needed(&self) {
        let mut entries: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        let mut rd = match tokio::fs::read_dir(&self.cfg.dir).await {
            Ok(r) => r,
            Err(_) => return,
        };
        while let Ok(Some(e)) = rd.next_entry().await {
            if e.path().extension().map(|x| x == "bin").unwrap_or(false)
                && let Ok(meta) = e.metadata().await
            {
                let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                entries.push((e.path(), mtime));
            }
        }
        if entries.len() <= self.cfg.max_blocks {
            return;
        }
        entries.sort_by_key(|(_, t)| *t);
        let to_remove = entries.len() - self.cfg.max_blocks;
        for (p, _) in entries.into_iter().take(to_remove) {
            let _ = tokio::fs::remove_file(p).await;
        }
    }

    /// 清空某 key 的全部块（媒体删除时调用）。
    pub async fn invalidate(&self, key: &str) {
        let hashed = key_hash(key);
        let mut rd = match tokio::fs::read_dir(&self.cfg.dir).await {
            Ok(r) => r,
            Err(_) => return,
        };
        while let Ok(Some(e)) = rd.next_entry().await {
            let name = e.file_name();
            if name.to_string_lossy().starts_with(&hashed) {
                let _ = tokio::fs::remove_file(e.path()).await;
            }
        }
    }

    /// 统计（块数, 总字节）。
    pub async fn stats(&self) -> (usize, u64) {
        let mut n = 0usize;
        let mut bytes = 0u64;
        let Ok(mut rd) = tokio::fs::read_dir(&self.cfg.dir).await else {
            return (0, 0);
        };
        while let Ok(Some(e)) = rd.next_entry().await {
            if e.path().extension().map(|x| x == "bin").unwrap_or(false)
                && let Ok(meta) = e.metadata().await
            {
                n += 1;
                bytes += meta.len();
            }
        }
        (n, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// mock 回源：64 字节源数据（16 个块 × 4 字节，用小块方便测试）。
    fn src() -> Vec<u8> {
        (0..64u8).collect()
    }

    fn small_cfg(dir: &Path) -> BlockCacheConfig {
        BlockCacheConfig {
            dir: dir.to_path_buf(),
            block_size: 16,
            max_blocks: 4,
            fetch_timeout: Duration::from_secs(5),
        }
    }

    #[tokio::test]
    async fn read_block_caches_after_first_fetch() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = BlockCache::new(small_cfg(tmp.path()));
        let data = src();

        let mut calls = 0usize;
        let v1 = cache
            .read_block("k1", 0, |off| {
                calls += 1;
                let d = data.clone();
                async move { Ok(d[off as usize..off as usize + 16].to_vec()) }
            })
            .await
            .unwrap();
        assert_eq!(v1, data[0..16].to_vec());
        assert_eq!(calls, 1);

        // 第二次读同一块：不再回源
        let v2 = cache
            .read_block("k1", 0, |off| {
                calls += 1;
                let d = data.clone();
                async move { Ok(d[off as usize..off as usize + 16].to_vec()) }
            })
            .await
            .unwrap();
        assert_eq!(v2, v1);
        assert_eq!(calls, 1, "命中缓存不应回源");

        let (n, bytes) = cache.stats().await;
        assert_eq!(n, 1);
        assert_eq!(bytes, 16);
    }

    #[tokio::test]
    async fn read_range_spans_blocks() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = BlockCache::new(small_cfg(tmp.path()));
        let data = src();

        // [5, 40) 跨 3 块
        let v = cache
            .read_range("k2", 5, Some(40), |off| {
                let d = data.clone();
                async move {
                    let end = (off as usize + 16).min(d.len());
                    Ok(d[off as usize..end].to_vec())
                }
            })
            .await
            .unwrap();
        assert_eq!(v, data[5..40].to_vec());
        let (n, _) = cache.stats().await;
        assert_eq!(n, 3, "应缓存 3 个块");
    }

    #[tokio::test]
    async fn read_range_to_eof() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = BlockCache::new(small_cfg(tmp.path()));
        let data = src();

        let v = cache
            .read_range("k3", 48, None, |off| {
                let d = data.clone();
                async move {
                    let end = (off as usize + 16).min(d.len());
                    Ok(d[off as usize..end].to_vec())
                }
            })
            .await
            .unwrap();
        assert_eq!(v, data[48..].to_vec());
    }

    #[tokio::test]
    async fn evict_keeps_max_blocks() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = BlockCache::new(small_cfg(tmp.path()));
        let data = src();

        // 读 6 个块（上限 4）
        for idx in 0..6u64 {
            cache
                .read_block("k4", idx, |off| {
                    let d = data.clone();
                    async move {
                        let end = (off as usize + 16).min(d.len());
                        if off as usize >= d.len() {
                            return Ok(vec![]);
                        }
                        Ok(d[off as usize..end].to_vec())
                    }
                })
                .await
                .unwrap();
        }
        let (n, _) = cache.stats().await;
        assert!(n <= 4, "块数应被限制在上限内，实际 {n}");
    }

    #[tokio::test]
    async fn invalidate_removes_key() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = BlockCache::new(small_cfg(tmp.path()));
        let data = src();
        cache
            .read_block("k5", 0, |off| {
                let d = data.clone();
                async move { Ok(d[off as usize..off as usize + 16].to_vec()) }
            })
            .await
            .unwrap();
        assert_eq!(cache.stats().await.0, 1);
        cache.invalidate("k5").await;
        assert_eq!(cache.stats().await.0, 0);
    }

    #[test]
    fn key_hash_stable_and_safe() {
        assert_eq!(key_hash("abc"), key_hash("abc"));
        assert_ne!(key_hash("abc"), key_hash("abd"));
        assert!(!key_hash("../evil").contains(".."));
    }
}
