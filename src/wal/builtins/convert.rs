//! Convert builtin operators
//!
//! convert - VCD to FST conversion

use crate::wal::ast::{Value, Operator};
use crate::wal::eval::{Environment, Dispatcher, Evaluator};

fn op_convert(_args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    Err("convert: not yet implemented".to_string())
}

fn ensure_arity_atleast(args: &[Value], min: usize) -> Result<(), String> {
    if args.len() < min {
        return Err(format!("Expected at least {} arguments, got {}", min, args.len()));
    }
    Ok(())
}

fn extract_string(v: &Value) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        _ => Err("Expected string".to_string()),
    }
}

pub fn register_convert(disp: &mut Dispatcher) {
    disp.register(Operator::Convert, op_convert);
}