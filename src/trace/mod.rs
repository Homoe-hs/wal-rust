//! Trace module for WAL
//!
//! Waveform trace interfaces and implementations.

mod trace;
mod container;
mod vcd;
mod fst;
mod fsdb;

pub use trace::{Trace, TraceId, ScalarValue, FindCondition, BatchEntry};
pub use container::{TraceContainer, SharedTraceContainer, new_shared};
pub use vcd::VcdTrace;
pub use fst::FstTrace;
pub use fsdb::FsdbTrace;