//! Array builtin operators
//!
//! array, seta, geta, geta/default, dela, mapa

use crate::wal::ast::{Value, WList, Operator};
use crate::wal::eval::{Environment, Dispatcher, Evaluator};

fn op_array(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    // array creates a flat key-value pair list: [k1 v1 k2 v2 ...]
    let mut result = Vec::new();
    for arg in args {
        result.push(arg.clone());
    }
    Ok(Value::List(WList::from_vec(result)))
}

fn op_seta(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 3)?;
    // seta arr key val — returns new array with key set to val
    let arr = extract_list(&args[0])?;
    let key = &args[1];
    let val = args[2].clone();
    let mut result = Vec::new();
    let mut found = false;
    for i in (0..arr.len()).step_by(2) {
        if i + 1 < arr.len() && &arr[i] == key {
            result.push(key.clone());
            result.push(val.clone());
            found = true;
        } else if i < arr.len() {
            result.push(arr[i].clone());
            if i + 1 < arr.len() {
                result.push(arr[i + 1].clone());
            }
        }
    }
    if !found {
        result.push(key.clone());
        result.push(val);
    }
    Ok(Value::List(WList::from_vec(result)))
}

fn op_geta(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 2)?;
    let arr = extract_list(&args[0])?;
    let key = &args[1];
    for i in (0..arr.len()).step_by(2) {
        if i + 1 < arr.len() && &arr[i] == key {
            return Ok(arr[i + 1].clone());
        }
    }
    Err("geta: key not found".to_string())
}

fn op_geta_default(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 3)?;
    let arr = extract_list(&args[0])?;
    let key = &args[1];
    let default = args[2].clone();
    for i in (0..arr.len()).step_by(2) {
        if i + 1 < arr.len() && &arr[i] == key {
            return Ok(arr[i + 1].clone());
        }
    }
    Ok(default)
}

fn op_dela(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 2)?;
    let arr = extract_list(&args[0])?;
    let key = &args[1];
    let mut result = Vec::new();
    for i in (0..arr.len()).step_by(2) {
        if i + 1 < arr.len() && &arr[i] == key {
            continue; // skip this key-value pair
        }
        result.push(arr[i].clone());
        if i + 1 < arr.len() {
            result.push(arr[i + 1].clone());
        }
    }
    Ok(Value::List(WList::from_vec(result)))
}

fn op_mapa(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 2)?;
    let arr = extract_list(&args[0])?;
    let func = &args[1];
    let mut result = Vec::new();
    for i in (0..arr.len()).step_by(2) {
        if i + 1 < arr.len() {
            let key = arr[i].clone();
            let val = arr[i + 1].clone();
            let mapped_val = match func {
                Value::Closure(c) => {
                    eval.eval_closure(c.clone(), &[val])?
                }
                _ => eval.eval_value_public(Value::List(WList::from_vec(vec![
                    func.clone(),
                    val,
                ])))?,
            };
            result.push(key);
            result.push(mapped_val);
        }
    }
    Ok(Value::List(WList::from_vec(result)))
}

fn extract_list(v: &Value) -> Result<Vec<Value>, String> {
    match v {
        Value::List(lst) => Ok(lst.0.clone()),
        _ => Err("Expected list".to_string()),
    }
}

fn ensure_arity(args: &[Value], expected: usize) -> Result<(), String> {
    if args.len() != expected {
        return Err(format!("Expected {} arguments, got {}", expected, args.len()));
    }
    Ok(())
}

pub fn register_array(disp: &mut Dispatcher) {
    disp.register(Operator::Array, op_array);
    disp.register(Operator::Seta, op_seta);
    disp.register(Operator::Geta, op_geta);
    disp.register(Operator::GetaDefault, op_geta_default);
    disp.register(Operator::Dela, op_dela);
    disp.register(Operator::Mapa, op_mapa);
}
