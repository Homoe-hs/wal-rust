//! Virtual signal builtin operators
//!
//! defsig, new-trace, dump-trace

use crate::wal::ast::{Value, Operator};
use crate::wal::eval::{Environment, Dispatcher, Evaluator};

fn op_defsig(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let name = extract_symbol(&args[0])?;
    let _expr = &args[1];
    // TODO: implement virtual signal
    env.define(&name, Value::Int(0));
    Ok(Value::Nil)
}

fn op_new_trace(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    // new-trace name - create new virtual trace
    Ok(Value::Nil)
}

fn op_dump_trace(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    // dump-trace path - export trace to VCD
    Ok(Value::Nil)
}

fn ensure_arity(args: &[Value], expected: usize) -> Result<(), String> {
    if args.len() != expected {
        return Err(format!("Expected {} arguments, got {}", expected, args.len()));
    }
    Ok(())
}

fn ensure_arity_atleast(args: &[Value], min: usize) -> Result<(), String> {
    if args.len() < min {
        return Err(format!("Expected at least {} arguments, got {}", min, args.len()));
    }
    Ok(())
}

fn extract_symbol(v: &Value) -> Result<String, String> {
    match v {
        Value::Symbol(s) => Ok(s.name.clone()),
        _ => Err("Expected symbol".to_string()),
    }
}

pub fn register_virtual(disp: &mut Dispatcher) {
    disp.register(Operator::Defsig, op_defsig);
    disp.register(Operator::NewTrace, op_new_trace);
    disp.register(Operator::DumpTrace, op_dump_trace);
}