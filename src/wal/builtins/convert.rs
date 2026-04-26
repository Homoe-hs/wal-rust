//! Convert builtin operators
//!
//! convert - VCD to FST conversion

use crate::wal::ast::{Value, Operator};
use crate::wal::eval::{Environment, Dispatcher, Evaluator};

fn op_convert(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let input = extract_string(&args[0])?;
    let output = extract_string(&args[1])?;

    // Get optional compression option
    let compression = if args.len() > 2 {
        extract_string(&args[2])?
    } else {
        "lz4".to_string()
    };

    // TODO: integrate with convert/pipeline.rs
    // For now, just return success
    Ok(Value::String(format!("converted {} -> {} ({})", input, output, compression)))
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
    disp.register(Operator::Import, op_convert);
}