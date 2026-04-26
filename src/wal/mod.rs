//! WAL - Waveform Analysis Language
//!
//! A high-performance WAL parser, evaluator, and REPL.

pub mod lexer;
pub mod parser;
pub mod ast;
pub mod eval;
pub mod builtins;
pub mod repl;

pub use ast::{Symbol, Value, WList, Operator, Closure};
pub use eval::{Environment, Evaluator};
pub use parser::{WalParser, parse_to_value};

pub fn language() -> tree_sitter::Language {
    tree_sitter_wal::language()
}