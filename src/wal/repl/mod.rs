//! REPL module for WAL
//!
//! Interactive read-eval-print loop implementation.

mod shell;

pub use shell::{Repl, run_repl};