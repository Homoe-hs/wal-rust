//! Trace module for WAL
//!
//! Waveform trace interfaces and implementations.

mod trace;
mod container;
mod vcd;
mod fst;

pub use trace::{Trace, TraceId, ScalarValue, FindCondition};
pub use container::{TraceContainer, SharedTraceContainer, new_shared};
pub use vcd::VcdTrace;
pub use fst::FstTrace;