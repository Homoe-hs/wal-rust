//! Trace module for WAL
//!
//! Waveform trace interfaces and implementations.

mod trace;
mod container;
mod vcd;
mod fst;
pub mod fsdb;

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
