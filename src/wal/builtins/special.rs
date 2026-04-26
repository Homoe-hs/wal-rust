//! Special builtin operators
//!
//! quote, quasiquote, unquote, eval, parse, fn, defmacro, macroexpand, gensym, rel_eval(@), slice, repl, exit

use crate::wal::ast::{Value, Operator, Symbol, WList, Closure, Macro};
use crate::wal::eval::{Environment, Dispatcher, Evaluator};
use std::cell::RefCell;
use std::rc::Rc;

fn op_quote(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    Ok(args[0].clone())
}

fn op_quasiquote(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    quasiquote_eval(&args[0], eval)
}

pub fn quasiquote_eval(value: &Value, eval: &mut Evaluator) -> Result<Value, String> {
    match value {
        Value::List(lst) => {
            let mut result = Vec::new();
            for item in &lst.0 {
                match item {
                    Value::Unquote(inner) => {
                        result.push(eval.eval_value_public(*inner.clone())?);
                    }
                    Value::UnquoteSplice(inner) => {
                        let evaluated = eval.eval_value_public(*inner.clone())?;
                        if let Value::List(splice_lst) = evaluated {
                            result.extend_from_slice(&splice_lst.0);
                        } else {
                            return Err("unquote-splice: expected list".to_string());
                        }
                    }
                    _ => {
                        result.push(quasiquote_eval(item, eval)?);
                    }
                }
            }
            Ok(Value::List(WList::from_vec(result)))
        }
        Value::Unquote(_) => Err("unquote outside quasiquote".to_string()),
        Value::UnquoteSplice(_) => Err("unquote-splice outside quasiquote".to_string()),
        Value::Symbol(_) | Value::Closure(_) | Value::Macro(_) => Ok(value.clone()),
        _ => Ok(value.clone()),
    }
}

fn op_unquote(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    // Should not be evaluated directly
    Err("unquote outside quasiquote".to_string())
}

fn op_eval(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    eval.eval_value_public(args[0].clone())
}

fn op_parse(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let source = extract_string(&args[0])?;
    let mut parser = crate::wal::WalParser::new()
        .map_err(|e| format!("Failed to create parser: {}", e))?;
    parser.parse_expr(&source)
}

fn op_fn(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let args_list = match &args[0] {
        Value::List(lst) => lst.0.iter().filter_map(|v| {
            if let Value::Symbol(s) = v {
                Some(s.clone())
            } else {
                None
            }
        }).collect(),
        Value::Symbol(s) => vec![s.clone()],
        _ => return Err("fn expects argument list".to_string()),
    };

    let mut body = args[1].clone();
    for arg in &args[2..] {
        body = Value::List(WList::from_vec(vec![body, arg.clone()]));
    }

    let closure = Closure::new(Rc::new(RefCell::new(env.clone())), args_list, body);
    Ok(Value::Closure(closure))
}

fn op_defmacro(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 3)?;
    let name = extract_symbol(&args[0])?;
    let args_list = match &args[1] {
        Value::List(lst) => lst.0.iter().filter_map(|v| {
            if let Value::Symbol(s) = v {
                Some(s.clone())
            } else {
                None
            }
        }).collect(),
        Value::Symbol(s) => vec![s.clone()],
        _ => return Err("defmacro expects argument list".to_string()),
    };

    let mut body = args[2].clone();
    for arg in &args[3..] {
        body = Value::List(WList::from_vec(vec![body, arg.clone()]));
    }

    let macro_obj = Macro::new(Rc::new(RefCell::new(env.clone())), args_list, body).with_name(&name);
    let value = Value::Macro(macro_obj);
    env.define(name, value.clone());
    Ok(value)
}

fn op_macroexpand(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let expr = &args[0];

    let (macro_name, macro_args) = match expr {
        Value::List(lst) if !lst.is_empty() => {
            match &lst[0] {
                Value::Symbol(s) => (s.name.clone(), lst.rest()),
                _ => return Err("macroexpand: first element must be a symbol".to_string()),
            }
        }
        _ => return Err("macroexpand: argument must be a list".to_string()),
    };

    let macro_val = env.lookup(&macro_name).ok_or_else(|| format!("Undefined macro: {}", macro_name))?;

    let macro_obj = match macro_val {
        Value::Macro(m) => m,
        _ => return Err(format!("{} is not a macro", macro_name)),
    };

    let mut local_env = eval.env.child();

    if macro_obj.variadic {
        if let Some(first_arg) = macro_obj.args.first() {
            local_env.define(first_arg.name.clone(), Value::List(WList::from_vec(macro_args.to_vec())));
        }
    } else {
        for (i, arg_name) in macro_obj.args.iter().enumerate() {
            let value = macro_args.get(i).cloned().unwrap_or(Value::Nil);
            local_env.define(arg_name.name.clone(), value);
        }
    }

    let saved_env = std::mem::replace(&mut eval.env, local_env);
    let eval_result = eval.eval_value(*macro_obj.body.clone());
    eval.env = saved_env;
    eval_result
}

fn op_gensym(_args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(Value::Symbol(Symbol::new(format!("GENSYM_{}", n))))
}

fn op_case(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let key = eval_value(&args[0], env)?;

    for chunk in args[1..].chunks(2) {
        if chunk.len() != 2 {
            return Err("case expects (key result) pairs".to_string());
        }
        if chunk[0] == key {
            return eval_value(&chunk[1], env);
        }
    }
    Ok(Value::Nil)
}

fn op_slice(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_range(args)?;
    let val = eval.eval_value_public(args[0].clone())?;
    let idx1 = extract_int(&eval.eval_value_public(args[1].clone())?)?;

    if args.len() == 2 {
        match val {
            Value::String(s) => {
                let idx = idx1 as usize;
                if idx >= s.len() {
                    return Ok(Value::String(String::new()));
                }
                Ok(Value::String(s.chars().nth(idx).map(|c| c.to_string()).unwrap_or_default()))
            }
            Value::List(lst) => {
                let idx = idx1 as usize;
                if idx >= lst.len() {
                    return Ok(Value::Nil);
                }
                Ok(lst.get(idx).cloned().unwrap_or(Value::Nil))
            }
            _ => Err("slice: first argument must be a string or list".to_string()),
        }
    } else {
        let idx2 = extract_int(&eval.eval_value_public(args[2].clone())?)?;
        match val {
            Value::String(s) => {
                let start = idx1.max(0) as usize;
                let end = idx2.max(0) as usize;
                if start >= s.len() {
                    return Ok(Value::String(String::new()));
                }
                let end = end.min(s.len());
                Ok(Value::String(s.chars().skip(start).take(end - start).collect()))
            }
            Value::List(lst) => {
                let start = idx1.max(0) as usize;
                let end = idx2.max(0) as usize;
                if start >= lst.len() {
                    return Ok(Value::List(WList::new()));
                }
                let end = end.min(lst.len());
                Ok(Value::List(WList::from_vec(lst.0[start..end].to_vec())))
            }
            _ => Err("slice: first argument must be a string or list".to_string()),
        }
    }
}

fn op_repl(_args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    Err("repl: interactive REPL not available in this context".to_string())
}

fn op_import(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let path = extract_string(&args[0])?;
    let source = std::fs::read_to_string(&path)
        .map_err(|e| format!("import: cannot read '{}': {}", path, e))?;
    eval.eval(&source)
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

fn ensure_arity_range(args: &[Value]) -> Result<(), String> {
    if args.len() < 2 || args.len() > 3 {
        return Err("slice expects 2-3 arguments".to_string());
    }
    Ok(())
}

fn extract_int(v: &Value) -> Result<i64, String> {
    match v {
        Value::Int(i) => Ok(*i),
        Value::Float(f) => Ok(*f as i64),
        _ => Err("Expected number".to_string()),
    }
}

fn extract_symbol(v: &Value) -> Result<String, String> {
    match v {
        Value::Symbol(s) => Ok(s.name.clone()),
        _ => Err("Expected symbol".to_string()),
    }
}

fn extract_string(v: &Value) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        _ => Err("Expected string".to_string()),
    }
}

fn eval_value(v: &Value, _env: &mut Environment) -> Result<Value, String> {
    Ok(v.clone())
}

pub fn register_special(disp: &mut Dispatcher) {
    disp.register(Operator::Quote, op_quote);
    disp.register(Operator::Quasiquote, op_quasiquote);
    disp.register(Operator::Unquote, op_unquote);
    disp.register(Operator::Eval, op_eval);
    disp.register(Operator::Parse, op_parse);
    disp.register(Operator::Fn, op_fn);
    disp.register(Operator::Defmacro, op_defmacro);
    disp.register(Operator::Macroexpand, op_macroexpand);
    disp.register(Operator::Gensym, op_gensym);
    disp.register(Operator::Slice, op_slice);
    disp.register(Operator::Repl, op_repl);
    disp.register(Operator::Import, op_import);
    disp.register(Operator::Case, op_case);
}