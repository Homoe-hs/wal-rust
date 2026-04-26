//! FST file format support
//!
//! This module provides support for reading and writing FST (Fast Signal Trace) files,
//! a binary waveform format used by GTKWave and other EDA tools.
//!
//! # FST Format Overview
//!
//! FST is a binary format with block-based structure:
//! - Header block (metadata)
//! - Geometry block (signal definitions)
//! - Hierarchy block (scope/variable tree)
//! - VCDATA blocks (value changes, compressed)
//!
//! # Example
//!
//! ```ignore
//! use walconv::fst::{FstWriter, FstOptions, VarType, ScopeType};
//!
//! let mut writer = FstWriter::create("output.fst", FstOptions::default()).unwrap();
//! writer.push_scope("top", ScopeType::VcdModule);
//! let handle = writer.create_var("clk", 1, VarType::VcdWire);
//! writer.emit_value_change(handle, &[b'1']);
//! writer.close().unwrap();
//! ```

pub mod blocks;
pub mod compress;
pub mod reader;
pub mod types;
pub mod varint;
pub mod writer;

#[allow(unused_imports)]
pub use reader::{FstFile, FstReader};
#[allow(unused_imports)]
pub use types::{Compression, ScopeType, VarType};
#[allow(unused_imports)]
pub use writer::{FstOptions, FstWriter};
