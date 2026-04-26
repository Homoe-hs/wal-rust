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

pub mod parser;
pub mod reader;
pub mod types;

#[allow(unused_imports)]
pub use parser::{VcdParser, MmapVcdParser};
#[allow(unused_imports)]
pub use reader::FileInfo;
#[allow(unused_imports)]
pub use types::{VcdError, VcdEvent};
