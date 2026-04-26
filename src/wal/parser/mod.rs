//! Parser module for WAL
//!
//! Tree-sitter based parser.

pub mod parse;
pub use parse::{WalParser, parse_to_value};