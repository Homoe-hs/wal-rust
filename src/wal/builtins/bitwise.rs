//! Bitwise builtin operators
//!
//! bor, band, bxor

use crate::wal::ast::{Value, Operator};
use crate::wal::eval::{Environment, Dispatcher, Evaluator};

fn op_bor(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let mut result = extract_int(&args[0])?;
    for arg in &args[1..] {
        result |= extract_int(arg)?;
    }
    Ok(Value::Int(result))
}

fn op_band(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let mut result = extract_int(&args[0])?;
    for arg in &args[1..] {
        result &= extract_int(arg)?;
    }
    Ok(Value::Int(result))
}

fn op_bxor(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let mut result = extract_int(&args[0])?;
    for arg in &args[1..] {
        result ^= extract_int(arg)?;
    }
    Ok(Value::Int(result))
}

fn extract_int(v: &Value) -> Result<i64, String> {
    match v {
        Value::Int(i) => Ok(*i),
        _ => Err("Expected integer".to_string()),
    }
}

fn ensure_arity_atleast(args: &[Value], min: usize) -> Result<(), String> {
    if args.len() < min {
        return Err(format!("Expected at least {} arguments, got {}", min, args.len()));
    }
    Ok(())
}

pub fn register_bitwise(disp: &mut Dispatcher) {
    disp.register(Operator::Bor, op_bor);
    disp.register(Operator::Band, op_band);
    disp.register(Operator::Bxor, op_bxor);
}