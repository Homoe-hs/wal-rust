//! Evaluation module for WAL

pub mod environment;
pub mod dispatch;
pub mod evaluator;
pub mod semantic;

pub use environment::Environment;
pub use dispatch::{Dispatcher, BuiltinFn};
pub use evaluator::Evaluator;
pub use semantic::{SemanticChecker, SemanticError};