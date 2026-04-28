//! List builtin operators
//!
//! list, first, second, last, rest, in, map, zip, max, min, fold, length, average

use crate::wal::ast::{Value, WList, Operator};
use crate::wal::eval::{Environment, Dispatcher, Evaluator};

fn op_list(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    Ok(Value::List(WList::from_vec(args.to_vec())))
}

fn op_first(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    match &args[0] {
        Value::List(lst) => lst.first().cloned().ok_or("empty list".to_string()),
        _ => Err("first expects list".to_string()),
    }
}

fn op_second(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    match &args[0] {
        Value::List(lst) if lst.len() >= 2 => Ok(lst[1].clone()),
        _ => Err("list has no second element".to_string()),
    }
}

fn op_third(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    match &args[0] {
        Value::List(lst) if lst.len() >= 3 => Ok(lst[2].clone()),
        _ => Err("list has no third element".to_string()),
    }
}

fn op_last(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    match &args[0] {
        Value::List(lst) => lst.last().cloned().ok_or("empty list".to_string()),
        _ => Err("last expects list".to_string()),
    }
}

fn op_rest(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    match &args[0] {
        Value::List(lst) => Ok(Value::List(WList::from_vec(lst.rest()))),
        _ => Err("rest expects list".to_string()),
    }
}

fn op_in(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let evaluated: Result<Vec<Value>, String> = args.iter().map(|v| eval.eval_value_public(v.clone())).collect();
    let evaluated = evaluated?;

    let container = &evaluated[evaluated.len() - 1];
    let mut result = true;

    if let Value::List(lst) = container {
        for check in &evaluated[..evaluated.len() - 1] {
            if !lst.0.contains(check) {
                result = false;
                break;
            }
        }
    } else if let Value::String(s) = container {
        for check in &evaluated[..evaluated.len() - 1] {
            if let Value::String(sub) = check {
                if !s.contains(sub.as_str()) {
                    result = false;
                    break;
                }
            } else {
                return Err("in: all arguments must be strings if the last argument is string".to_string());
            }
        }
    } else {
        return Err("in: expects list or string as last argument".to_string());
    }

    Ok(Value::Bool(result))
}

fn op_map(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 2)?;
    let list = match &args[1] {
        Value::List(lst) => &lst.0,
        _ => return Err("map: second argument must be a list".to_string()),
    };

    let mut result = Vec::new();
    for item in list {
        let applied = match &args[0] {
            Value::Symbol(s) if Operator::from_str(&s.name).is_some() => {
                let op = Operator::from_str(&s.name).unwrap();
                let quoted = Value::List(WList::from_vec(vec![
                    Value::Symbol(crate::wal::ast::Symbol::new("quote")),
                    item.clone()
                ]));
                let expr = Value::List(WList::from_vec(vec![Value::Symbol(crate::wal::ast::Symbol::new(op.as_str())), quoted]));
                eval.eval_value_public(expr)?
            }
            Value::List(_) | Value::Symbol(_) => {
                let quoted = Value::List(WList::from_vec(vec![
                    Value::Symbol(crate::wal::ast::Symbol::new("quote")),
                    item.clone()
                ]));
                let func = eval.eval_value_public(args[0].clone())?;
                match func {
                    Value::Closure(c) => {
                        eval.eval_closure(c, &[quoted])?
                    }
                    _ => return Err("map: first argument must be a function".to_string()),
                }
            }
            _ => return Err("map: first argument must be a function or operator".to_string()),
        };
        result.push(applied);
    }
    Ok(Value::List(WList::from_vec(result)))
}

fn op_zip(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 2)?;
    let a = extract_list(&args[0])?;
    let b = extract_list(&args[1])?;

    let len = a.len().min(b.len());
    let mut result = Vec::new();
    for i in 0..len {
        result.push(Value::List(WList::from_vec(vec![a[i].clone(), b[i].clone()])));
    }
    Ok(Value::List(WList::from_vec(result)))
}

fn op_max(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    match &args[0] {
        Value::List(lst) if !lst.is_empty() => {
            let mut max_val = &lst.0[0];
            for v in &lst.0[1..] {
                if is_greater(v, max_val) {
                    max_val = v;
                }
            }
            Ok(max_val.clone())
        }
        _ => Err("max expects non-empty list".to_string()),
    }
}

fn op_min(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    match &args[0] {
        Value::List(lst) if !lst.is_empty() => {
            let mut min_val = &lst.0[0];
            for v in &lst.0[1..] {
                if is_less(v, min_val) {
                    min_val = v;
                }
            }
            Ok(min_val.clone())
        }
        _ => Err("min expects non-empty list".to_string()),
    }
}

fn is_greater(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(i), Value::Int(j)) => i > j,
        (Value::Float(f), Value::Float(g)) => f > g,
        (Value::Int(i), Value::Float(f)) => (*i as f64) > *f,
        (Value::Float(f), Value::Int(i)) => *f > (*i as f64),
        _ => false,
    }
}

fn is_less(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(i), Value::Int(j)) => i < j,
        (Value::Float(f), Value::Float(g)) => f < g,
        (Value::Int(i), Value::Float(f)) => (*i as f64) < *f,
        (Value::Float(f), Value::Int(i)) => *f < (*i as f64),
        _ => false,
    }
}

fn op_fold(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 3)?;
    // args are already evaluated by the dispatcher — don't re-evaluate
    let acc_init = args[1].clone();
    let list = match &args[2] {
        Value::List(lst) => lst.0.clone(),
        _ => return Err("fold: last argument must be a list".to_string()),
    };

    let mut acc = acc_init;
    for item in &list {
        match &args[0] {
            Value::Symbol(s) if Operator::from_str(&s.name).is_some() => {
                let op = Operator::from_str(&s.name).unwrap();
                let quoted_acc = Value::List(WList::from_vec(vec![
                    Value::Symbol(crate::wal::ast::Symbol::new("quote")),
                    acc.clone()
                ]));
                let quoted_item = Value::List(WList::from_vec(vec![
                    Value::Symbol(crate::wal::ast::Symbol::new("quote")),
                    item.clone()
                ]));
                let expr = Value::List(WList::from_vec(vec![
                    Value::Symbol(crate::wal::ast::Symbol::new(op.as_str())),
                    quoted_acc,
                    quoted_item
                ]));
                acc = eval.eval_value_public(expr)?;
            }
            Value::List(_) | Value::Symbol(_) => {
                let func = eval.eval_value_public(args[0].clone())?;
                let quoted_acc = Value::List(WList::from_vec(vec![
                    Value::Symbol(crate::wal::ast::Symbol::new("quote")),
                    acc.clone()
                ]));
                let quoted_item = Value::List(WList::from_vec(vec![
                    Value::Symbol(crate::wal::ast::Symbol::new("quote")),
                    item.clone()
                ]));
                match func {
                    Value::Closure(c) => {
                        acc = eval.eval_closure(c, &[quoted_acc, quoted_item])?;
                    }
                    _ => return Err("fold: first argument must be a function".to_string()),
                }
            }
            _ => return Err("fold: first argument must be a function or operator".to_string()),
        }
    }
    Ok(acc)
}

fn op_length(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    match &args[0] {
        Value::List(lst) => Ok(Value::Int(lst.len() as i64)),
        Value::String(s) => Ok(Value::Int(s.len() as i64)),
        _ => Err("length expects list or string".to_string()),
    }
}

fn op_average(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let list = extract_list(&args[0])?;
    if list.is_empty() {
        return Err("average of empty list".to_string());
    }

    let mut sum: f64 = 0.0;
    for v in list {
        sum += extract_float(v)?;
    }
    Ok(Value::Float(sum / list.len() as f64))
}

fn extract_list(v: &Value) -> Result<&Vec<Value>, String> {
    match v {
        Value::List(lst) => Ok(&lst.0),
        _ => Err("Expected list".to_string()),
    }
}

fn extract_float(v: &Value) -> Result<f64, String> {
    match v {
        Value::Int(i) => Ok(*i as f64),
        Value::Float(f) => Ok(*f),
        _ => Err("Expected number".to_string()),
    }
}

fn op_is_null(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    match &args[0] {
        Value::Nil => Ok(Value::Bool(true)),
        Value::List(lst) => Ok(Value::Bool(lst.is_empty())),
        _ => Ok(Value::Bool(false)),
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

pub fn register_list(disp: &mut Dispatcher) {
    disp.register(Operator::List, op_list);
    disp.register(Operator::First, op_first);
    disp.register(Operator::Second, op_second);
    disp.register(Operator::Last, op_last);
    disp.register(Operator::Rest, op_rest);
    disp.register(Operator::In, op_in);
    disp.register(Operator::Map, op_map);
    disp.register(Operator::Zip, op_zip);
    disp.register(Operator::Max, op_max);
    disp.register(Operator::Min, op_min);
    disp.register(Operator::Fold, op_fold);
    disp.register(Operator::Length, op_length);
    disp.register(Operator::Average, op_average);
    disp.register(Operator::Third, op_third);
    disp.register(Operator::IsNull, op_is_null);
}