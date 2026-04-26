//! wal-rust - WAL: Waveform Analysis Language
//!
//! # Architecture
//!
//! - [`vcd`] - VCD file parser with streaming state machine
//! - [`fst`] - FST file reader/writer
//! - [`wal`] - WAL language parser, evaluator, and REPL

pub mod cli;
pub mod fst;
pub mod vcd;
pub mod wal;
pub mod trace;

pub use cli::{Args, LogLevel};
pub use vcd::{VcdError, VcdEvent, VcdParser};
pub use fst::{FstWriter, FstOptions, Compression};