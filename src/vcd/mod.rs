//! VCD (Value Change Dump) file format parser
//!
//! Provides streaming parser for VCD files, the standard waveform format
//! defined in IEEE 1364.
//!
//! # VCD Format
//!
//! VCD is a text-based format that consists of two sections:
//! 1. Header: Contains timescale, scope/variable declarations
//! 2. Dump: Contains timestamp and value changes
//!
//! # Example
//!
//! ```ignore
//! use walconv::vcd::{VcdParser, VcdEvent};
//!
//! let file = std::fs::File::open("wave.vcd")?;
//! let parser = VcdParser::new(file);
//!
//! for event in parser {
//!     match event? {
//!         VcdEvent::Timestamp(t) => println!("Time: {}", t),
//!         VcdEvent::ValueChange { id, value } => {
//!             println!("Signal {} changed to {:?}", id, value);
//!         }
//!         _ => {}
//!     }
//! }
//! ```

pub mod convert;
pub mod parallel_convert;
pub mod parser;
pub mod reader;
pub mod types;

#[allow(unused_imports)]

/// 把 VCD `$var` 的名字字段规范成"**引用名**"(去掉尾部 range/位选)。
///
/// VCD 头里 `$var wire 4 ! data [3:0] $end` 的引用是 `data`,`[3:0]` 是范围;
/// 转义标识符(`\foo[0] `)的方括号**属于名字**,必须原样保留。
/// 统一成引用名后,VCD 与 FST/FSDB 的 `(SIGNALS)`/名字解析口径一致(BUG-004/005)。
pub(crate) fn canonical_var_name(name: &str) -> &str {
    let bytes = canonical_var_name_bytes(name.as_bytes());
    // bytes 一定是 name 的子切片 → 安全
    let off = bytes.as_ptr() as usize - name.as_ptr() as usize;
    &name[off..off + bytes.len()]
}

pub(crate) fn canonical_var_name_bytes(mut s: &[u8]) -> &[u8] {
    while matches!(s.last(), Some(b' ') | Some(b'\t')) {
        s = &s[..s.len() - 1];
    }
    if s.first() == Some(&b'\\') {
        return s; // 转义标识符: 原样
    }
    if s.last() == Some(&b']') {
        if let Some(open) = s.iter().rposition(|&b| b == b'[') {
            let inner = &s[open + 1..s.len() - 1];
            let looks_like_range = !inner.is_empty()
                && inner.iter().all(|&b| b.is_ascii_digit() || b == b':')
                && inner.iter().any(|b| b.is_ascii_digit());
            if looks_like_range {
                let mut base = &s[..open];
                while matches!(base.last(), Some(b' ') | Some(b'\t')) {
                    base = &base[..base.len() - 1];
                }
                if !base.is_empty() {
                    return base;
                }
            }
        }
    }
    s
}

pub use types::VcdError;
#[allow(unused_imports)]
pub use types::VcdEvent;
