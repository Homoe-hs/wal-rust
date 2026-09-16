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
