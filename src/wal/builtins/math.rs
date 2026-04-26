//! Math builtin operators
//!
//! +, -, *, /, **, floor, ceil, round, mod, sum

use crate::wal::ast::{Value, WList};
use crate::wal::eval::{Environment, Dispatcher, Evaluator};

fn op_add(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    if args.is_empty() {
        return Ok(Value::Int(0));
    }

    let evaluated: Result<Vec<Value>, String> = args.iter().map(|v| eval.eval_value_public(v.clone())).collect();
    let evaluated = evaluated?;

    if evaluated.iter().any(|v| matches!(v, Value::List(_))) {
        let mut res = Vec::new();
        for item in &evaluated {
            match item {
                Value::List(lst) => res.extend_from_slice(&lst.0),
                _ => res.push(item.clone()),
            }
        }
        return Ok(Value::List(WList::from_vec(res)));
    }

    if evaluated.iter().any(|v| matches!(v, Value::String(_))) {
        let res: String = evaluated.iter().map(|v| v.to_string()).collect();
        return Ok(Value::String(res));
    }

    let mut result: i64 = 0;
    let mut is_float = false;
    let mut float_result: f64 = 0.0;

    for arg in &evaluated {
        match arg {
            Value::Int(i) => {
                if is_float {
                    float_result += *i as f64;
                } else {
                    result += i;
                }
            }
            Value::Float(f) => {
                if !is_float {
                    float_result = result as f64;
                    is_float = true;
                }
                float_result += f;
            }
            _ => return Err("Invalid type for addition".to_string()),
        }
    }

    if is_float {
        Ok(Value::Float(float_result))
    } else {
        Ok(Value::Int(result))
    }
}

fn op_sub(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 1)?;
    match &args[0] {
        Value::Int(_) => {
            let mut result: i64 = if args.len() == 1 {
                -extract_int(&args[0])?
            } else {
                extract_int(&args[0])?
            };
            for arg in &args[1..] {
                result -= extract_int(arg)?;
            }
            Ok(Value::Int(result))
        }
        Value::Float(_) => {
            let mut result: f64 = if args.len() == 1 {
                -extract_float(&args[0])?
            } else {
                extract_float(&args[0])?
            };
            for arg in &args[1..] {
                result -= extract_float(arg)?;
            }
            Ok(Value::Float(result))
        }
        _ => Err("Invalid type for subtraction".to_string()),
    }
}

fn op_mul(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if args.is_empty() {
        return Ok(Value::Int(1));
    }

    let mut result: i64 = 1;
    let mut is_float = false;
    let mut float_result: f64 = 1.0;

    for arg in args {
        match arg {
            Value::Int(i) => {
                if is_float {
                    float_result *= *i as f64;
                } else {
                    result *= i;
                }
            }
            Value::Float(f) => {
                if !is_float {
                    float_result = result as f64;
                    is_float = true;
                }
                float_result *= f;
            }
            _ => return Err("Invalid type for multiplication".to_string()),
        }
    }

    if is_float {
        Ok(Value::Float(float_result))
    } else {
        Ok(Value::Int(result))
    }
}

fn op_div(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 2)?;
    let a = extract_float(&args[0])?;
    let b = extract_float(&args[1])?;
    if b == 0.0 {
        return Err("Division by zero".to_string());
    }
    Ok(Value::Float(a / b))
}

fn op_exp(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 2)?;
    let base = extract_float(&args[0])?;
    let exp = extract_float(&args[1])?;
    Ok(Value::Float(base.powf(exp)))
}

fn op_floor(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    match &args[0] {
        Value::Float(f) => Ok(Value::Int(f.floor() as i64)),
        Value::Int(i) => Ok(Value::Int(*i)),
        _ => Err("floor expects number".to_string()),
    }
}

fn op_ceil(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    match &args[0] {
        Value::Float(f) => Ok(Value::Int(f.ceil() as i64)),
        Value::Int(i) => Ok(Value::Int(*i)),
        _ => Err("ceil expects number".to_string()),
    }
}

fn op_round(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    match &args[0] {
        Value::Float(f) => Ok(Value::Int(f.round() as i64)),
        Value::Int(i) => Ok(Value::Int(*i)),
        _ => Err("round expects number".to_string()),
    }
}

fn op_mod(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 2)?;
    let a = extract_int(&args[0])?;
    let b = extract_int(&args[1])?;
    if b == 0 {
        return Err("Modulo by zero".to_string());
    }
    Ok(Value::Int(a % b))
}

fn op_sum(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let list = match eval.eval_value_public(args[0].clone())? {
        Value::List(lst) => lst.0.clone(),
        _ => return Err("sum expects a list".to_string()),
    };
    if list.is_empty() {
        return Ok(Value::Int(0));
    }
    let mut int_sum: i64 = 0;
    let mut float_sum: f64 = 0.0;
    let mut is_float = false;
    for v in list {
        match v {
            Value::Int(i) => {
                if is_float {
                    float_sum += i as f64;
                } else {
                    int_sum += i;
                }
            }
            Value::Float(f) => {
                if !is_float {
                    float_sum = int_sum as f64;
                    is_float = true;
                }
                float_sum += f;
            }
            _ => return Err("sum: list must contain numbers".to_string()),
        }
    }
    if is_float {
        Ok(Value::Float(float_sum))
    } else {
        Ok(Value::Int(int_sum))
    }
}

fn extract_int(v: &Value) -> Result<i64, String> {
    match v {
        Value::Int(i) => Ok(*i),
        Value::Float(f) => Ok(*f as i64),
        _ => Err("Expected number".to_string()),
    }
}

fn extract_float(v: &Value) -> Result<f64, String> {
    match v {
        Value::Int(i) => Ok(*i as f64),
        Value::Float(f) => Ok(*f),
        _ => Err("Expected number".to_string()),
    }
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

pub fn register_math(disp: &mut Dispatcher) {
    disp.register(crate::wal::ast::Operator::Add, op_add);
    disp.register(crate::wal::ast::Operator::Sub, op_sub);
    disp.register(crate::wal::ast::Operator::Mul, op_mul);
    disp.register(crate::wal::ast::Operator::Div, op_div);
    disp.register(crate::wal::ast::Operator::Exp, op_exp);
    disp.register(crate::wal::ast::Operator::Floor, op_floor);
    disp.register(crate::wal::ast::Operator::Ceil, op_ceil);
    disp.register(crate::wal::ast::Operator::Round, op_round);
    disp.register(crate::wal::ast::Operator::Mod, op_mod);
    disp.register(crate::wal::ast::Operator::Sum, op_sum);
}