//! Trace module for WAL
//!
//! Waveform trace interfaces and implementations.

mod trace;
mod container;
mod name_store;
mod vcd;
mod fst;
pub mod fsdb;

/// 去掉名字尾部的 range/位选(`data[3:0]` / `data [3:0]` → `data`)。
///
/// **BUG-004**: VCD 的规范名已统一为**引用名**(不带 range, 与 FST 一致);
/// 这个函数用于让"旧写法"(带 range 的查询)继续可用。转义标识符(`\a[0]`)的
/// 方括号属于名字 → 不剥。
pub fn strip_range_suffix(name: &str) -> Option<&str> {
    if name.starts_with('\\') {
        return None;
    }
    let t = name.trim_end();
    if !t.ends_with(']') {
        return None;
    }
    let open = t.rfind('[')?;
    let inner = &t[open + 1..t.len() - 1];
    let looks_like_range = !inner.is_empty()
        && inner.bytes().all(|b| b.is_ascii_digit() || b == b':')
        && inner.bytes().any(|b| b.is_ascii_digit());
    if !looks_like_range {
        return None;
    }
    let base = t[..open].trim_end();
    if base.is_empty() {
        None
    } else {
        Some(base)
    }
}

/// 名字解析兼容层:`resolve_name` 失败时去掉尾部 range 再试一次(见 [`strip_range_suffix`])。
pub trait ResolveCompat {
    fn resolve_name_compat(&self, name: &str) -> Option<String>;
    fn resolve_name_strict_compat(&self, name: &str) -> Result<String, String>;
}

impl<T: Trace + ?Sized> ResolveCompat for T {
    fn resolve_name_compat(&self, name: &str) -> Option<String> {
        self.resolve_name(name).or_else(|| {
            let base = strip_range_suffix(name)?;
            if base == name { None } else { self.resolve_name(base) }
        })
    }
    fn resolve_name_strict_compat(&self, name: &str) -> Result<String, String> {
        self.resolve_name_strict(name).or_else(|e| match strip_range_suffix(name) {
            Some(base) if base != name => self.resolve_name_strict(base).map_err(|_| e),
            _ => Err(e),
        })
    }
}

pub use trace::{Trace, TraceId, ScalarValue, FindCondition, BatchEntry};
pub use container::{TraceContainer, SharedTraceContainer, new_shared};
pub use vcd::VcdTrace;
pub use fst::FstTrace;
pub use fsdb::FsdbTrace;

/// 缓存写失败的**一次性**提示。
///
/// 缓存只是加速手段: 磁盘满(`ENOSPC`)、配额、只读目录、`ulimit -f`(`EFBIG`)
/// 都不该影响查询结果, 也不该影响退出码。这里只提示一次, 免得大循环里刷屏。
pub(crate) fn warn_cache_write(path: &std::path::Path, err: &std::io::Error) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    eprintln!(
        "wal-rust: 缓存写入失败, 本次会话不再尝试写缓存(不影响查询结果): {}: {}",
        path.display(),
        err
    );
}
/// 测试可见的 FSDB 内部编解码(仅供单测; 产品 API 不暴露)。
pub mod fsdb_test_api {
    use super::fsdb::{
        decode_timeline, decode_tree as raw_decode_tree, encode_timeline,
        encode_tree as raw_encode_tree,
    };

    /// 编码时间线(测试用)
    pub fn encode(times: &[u64], first_change: Option<u64>) -> Vec<u8> {
        encode_timeline(times, first_change)
    }

    /// 解码时间线(测试用; 指纹不符/损坏 → None)
    pub fn decode(buf: &[u8], expect_fp: u64) -> Option<(Vec<u64>, Option<u64>)> {
        decode_timeline(buf, expect_fp)
    }

    /// 轮转分片(测试用): 并行构建时间线时每个 worker 负责的信号下标
    pub fn round_robin_slice(n: usize, offset: usize, stride: usize) -> Vec<usize> {
        super::fsdb::round_robin_slice(n, offset, stride)
    }

    /// 写时间线分片文件(map 阶段产物格式; 测试用)
    pub fn write_times_file(path: &std::path::Path, times: &[u64]) -> std::io::Result<()> {
        super::fsdb::write_times(path, times)
    }

    /// 归并分片 → 安装 `.ftl`(reduce 阶段入口; 测试用) → (时间点数, 缓存文件)
    pub fn merge_timeline_parts(
        file: &std::path::Path,
        parts: &[std::path::PathBuf],
        cache_root: Option<&std::path::Path>,
    ) -> Result<(usize, std::path::PathBuf), String> {
        super::fsdb::merge_timeline_parts(file, parts, cache_root)
    }

    /// `WAL_FSDB_TL_JOBS` 解析(含 `auto`; 测试用)
    pub fn timeline_jobs() -> usize {
        super::fsdb::timeline_jobs()
    }

    /// 纯解析(测试用): raw 值 × 核数 → 分片数
    pub fn parse_timeline_jobs(raw: &str, cores: usize) -> usize {
        super::fsdb::parse_timeline_jobs(raw, cores)
    }

    /// 一个波形的并行度决策(测试用) → (worker 数, 是否自动)
    pub fn timeline_jobs_decision(file_bytes: u64, sigs: usize) -> (usize, bool) {
        super::fsdb::timeline_jobs_decision(file_bytes, sigs)
    }

    /// 纯决策函数(测试用): 设置值 × 额度 × 文件大小 → (worker 数, 是否自动)
    pub fn decide_jobs(
        setting: Option<&str>,
        budget: usize,
        file_bytes: u64,
        sigs: usize,
        min_bytes: u64,
    ) -> (usize, bool) {
        super::fsdb::decide_jobs(setting, budget, file_bytes, sigs, min_bytes)
    }

    /// 波形指纹(测试用): 旁挂缓存的失效判据
    pub fn wave_fingerprint(path: &std::path::Path) -> u64 {
        super::vcd::wave_fingerprint(path)
    }

    /// 编码名字树(测试用)
    pub fn encode_tree(names: &[String], widths: &[usize], scopes: &[String]) -> Vec<u8> {
        raw_encode_tree(&super::fsdb::TreeSnapshot {
            names: names.to_vec(),
            widths: widths.to_vec(),
            scopes: scopes.to_vec(),
        })
    }

    /// 解码名字树(测试用) → (names, widths, scopes)
    pub fn decode_tree(buf: &[u8], expect_fp: u64) -> Option<(Vec<String>, Vec<usize>, Vec<String>)> {
        raw_decode_tree(buf, expect_fp).map(|t| (t.names, t.widths, t.scopes))
    }
}
