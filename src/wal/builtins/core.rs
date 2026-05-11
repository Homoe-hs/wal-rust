//! Core builtin operators
//!
//! Control flow: not, =, !=, if, do, while, let, set, define, print, printf, exit, etc.

use crate::wal::ast::{Value, Operator, Symbol};
use crate::wal::eval::{Environment, Dispatcher, Evaluator};

fn op_not(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 1)?;
    let any_truthy = args.iter().any(|a| a.is_truthy());
    Ok(Value::Bool(!any_truthy))
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(i), Value::Float(f)) => (*i as f64) == *f,
        (Value::Float(f), Value::Int(i)) => *f == (*i as f64),
        (Value::List(la), Value::List(lb)) => {
            la.0.len() == lb.0.len() && la.0.iter().zip(lb.0.iter()).all(|(x, y)| values_equal(x, y))
        }
        _ => a == b,
    }
}

fn op_eq(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let first = &args[0];
    Ok(Value::Bool(args[1..].iter().all(|a| values_equal(a, first))))
}

fn op_neq(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let first = &args[0];
    Ok(Value::Bool(args[1..].iter().any(|a| !values_equal(a, first))))
}

fn op_if(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 3)?;
    let cond = args[0].is_truthy();
    if cond {
        eval.eval_value_public(args[1].clone())
    } else {
        eval.eval_value_public(args[2].clone())
    }
}

fn op_define(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 2)?;
    let name = extract_symbol(&args[0])?;
    let value = args[1].clone();
    env.define(name, value.clone());
    Ok(value)
}

fn op_set(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 2)?;
    let name = extract_symbol(&args[0])?;
    let value = args[1].clone();
    env.set(&name, value.clone())?;
    Ok(value)
}

fn op_let(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 1)?;
    let mut new_env = env.child();

    let bindings = match &args[0] {
        Value::List(list) => list.0.clone(),
        _ => return Err("let expects list of bindings".to_string()),
    };

    for binding in bindings.chunks(2) {
        if binding.len() != 2 {
            return Err("let binding must be (name value)".to_string());
        }
        let name = extract_symbol(&binding[0])?;
        let value = eval.eval_value_public(binding[1].clone())?;
        new_env.define(name, value);
    }

    let mut result = Value::Nil;
    for arg in &args[1..] {
        result = eval.eval_value_public(arg.clone())?;
    }
    Ok(result)
}

fn op_do(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    let mut result = Value::Nil;
    for arg in args {
        result = eval.eval_value_public(arg.clone())?;
    }
    Ok(result)
}

fn op_while(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 2)?;
    let mut result = Value::Nil;
    while eval.eval_value_public(args[0].clone())?.is_truthy() {
        result = eval.eval_value_public(args[1].clone())?;
    }
    Ok(result)
}

fn op_print(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    for arg in args {
        match arg {
            Value::String(s) => print!("{}", s),
            Value::Symbol(s) => print!("{}", s.name),
            _ => print!("{}", arg),
        }
    }
    println!();
    Ok(Value::Nil)
}

fn op_printf(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 1)?;
    let fmt = extract_string(&args[0])?;
    let mut evaluated = Vec::new();
    for v in &args[1..] {
        evaluated.push(format!("{}", eval.eval_value_public(v.clone())?));
    }
    // Handle escape sequences in format string
    let fmt = fmt.replace("\\n", "\n").replace("\\t", "\t").replace("\\\"", "\"");
        let result = if fmt.contains('%') {
        // printf-style: Python format spec: %[flags][width][.precision]spec
        let mut pos = 0usize;
        let mut output = String::new();
        let mut arg_idx = 0usize;
        let chars: Vec<char> = fmt.chars().collect();
        while pos < chars.len() {
            if chars[pos] == '%' && pos + 1 < chars.len() {
                let start = pos;
                pos += 1; // skip %
                // Parse flags
                let mut flags = String::new();
                while pos < chars.len() && matches!(chars[pos], '-' | '+' | ' ' | '0' | '#') {
                    flags.push(chars[pos]);
                    pos += 1;
                }
                // Parse width
                let mut width_str = String::new();
                while pos < chars.len() && chars[pos].is_ascii_digit() {
                    width_str.push(chars[pos]);
                    pos += 1;
                }
                let width: usize = width_str.parse().unwrap_or(0);
                // Parse precision
                let mut precision: Option<usize> = None;
                if pos < chars.len() && chars[pos] == '.' {
                    pos += 1;
                    let mut prec_str = String::new();
                    while pos < chars.len() && chars[pos].is_ascii_digit() {
                        prec_str.push(chars[pos]);
                        pos += 1;
                    }
                    precision = prec_str.parse().ok();
                }
                // Read specifier
                let spec = if pos < chars.len() { chars[pos] } else { '%' };
                pos += 1;
                let val = evaluated.get(arg_idx).map(|s| s.as_str()).unwrap_or("");
                let formatted = match spec {
                    'd' | 'i' => {
                        let n: i64 = val.parse().unwrap_or(0);
                        if let Some(prec) = precision {
                            format!("{:0>width$}", n.to_string(), width = prec)
                        } else if flags.contains('0') && width > 0 {
                            format!("{:0>width$}", n, width = width)
                        } else if width > 0 {
                            format!("{:>width$}", n, width = width)
                        } else {
                            n.to_string()
                        }
                    }
                    's' => {
                        if width > 0 && flags.contains('-') {
                            format!("{:width$}", val, width = width)
                        } else if width > 0 {
                            format!("{:>width$}", val, width = width)
                        } else {
                            val.to_string()
                        }
                    }
                    'f' => {
                        let n: f64 = val.parse().unwrap_or(0.0);
                        match precision {
                            Some(p) => format!("{:.prec$}", n, prec = p),
                            None => n.to_string(),
                        }
                    }
                    'x' => {
                        let n: i64 = val.parse().unwrap_or(0);
                        let s = format!("{:x}", n);
                        if width > 0 && flags.contains('0') {
                            format!("{:0>width$}", s, width = width)
                        } else if width > 0 {
                            format!("{:>width$}", s, width = width)
                        } else {
                            s
                        }
                    }
                    '%' => "%".to_string(),
                    _ => {
                        format!("%{}", spec)
                    }
                };
                output.push_str(&formatted);
                arg_idx += 1;
            } else {
                output.push(chars[pos]);
                pos += 1;
            }
        }
        output
    } else {
        interpolate(&fmt, &evaluated)
    };
    print!("{}", result);
    Ok(Value::Nil)
}

fn op_exit(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    let code = if args.is_empty() {
        0
    } else {
        match &args[0] {
            Value::Int(i) => *i as i32,
            _ => 0,
        }
    };
    Err(format!("exit:{}", code))
}

fn op_type(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let type_str = match args[0].type_name() {
        "int" => "<class 'int'>",
        "float" => "<class 'float'>",
        "string" => "<class 'str'>",
        "bool" => "<class 'bool'>",
        "symbol" => "<class 'symbol'>",
        "list" => "<class 'list'>",
        "nil" => "<class 'NoneType'>",
        other => other,
    };
    Ok(Value::String(type_str.to_string()))
}

fn op_alias(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 2)?;
    let alias_name = extract_symbol(&args[0])?;
    // Evaluate target expression (golden behavior)
    let target_val = eval.eval_value_public(args[1].clone())?;
    let target_name = match &target_val {
        Value::Symbol(s) => s.name.clone(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    env.add_alias(&alias_name, &target_name);
    Ok(Value::Nil)
}

fn op_unalias(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let name = extract_symbol(&args[0])?;
    if env.remove_alias(&name) {
        Ok(Value::Nil)
    } else {
        Err(format!("Alias '{}' not found", name))
    }
}

fn op_when(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let cond = eval.eval_value_public(args[0].clone())?;
    if cond.is_truthy() {
        let mut result = Value::Nil;
        for arg in &args[1..] {
            result = eval.eval_value_public(arg.clone())?;
        }
        Ok(result)
    } else {
        Ok(Value::Nil)
    }
}

fn op_unless(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let cond = eval.eval_value_public(args[0].clone())?;
    if !cond.is_truthy() {
        let mut result = Value::Nil;
        for arg in &args[1..] {
            result = eval.eval_value_public(arg.clone())?;
        }
        Ok(result)
    } else {
        Ok(Value::Nil)
    }
}

fn op_cond(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    for clause in args {
        match clause {
            Value::List(lst) if !lst.is_empty() => {
                let test = lst.first().ok_or("cond clause cannot be empty")?;
                if test == &Value::Symbol(Symbol::new("else")) || eval.eval_value_public(test.clone())?.is_truthy() {
                    let mut result = Value::Nil;
                    for arg in lst.rest() {
                        result = eval.eval_value_public(arg)?;
                    }
                    return Ok(result);
                }
            }
            _ => return Err("cond expects list clauses".to_string()),
        }
    }
    Ok(Value::Nil)
}

fn op_larger(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let mut prev = extract_number(&args[0])?;
    for arg in &args[1..] {
        let curr = extract_number(arg)?;
        if curr >= prev {
            return Ok(Value::Bool(false));
        }
        prev = curr;
    }
    Ok(Value::Bool(true))
}

fn op_smaller(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let mut prev = extract_number(&args[0])?;
    for arg in &args[1..] {
        let curr = extract_number(arg)?;
        if curr <= prev {
            return Ok(Value::Bool(false));
        }
        prev = curr;
    }
    Ok(Value::Bool(true))
}

fn op_larger_equal(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let mut prev = extract_number(&args[0])?;
    for arg in &args[1..] {
        let curr = extract_number(arg)?;
        if curr > prev {
            return Ok(Value::Bool(false));
        }
        prev = curr;
    }
    Ok(Value::Bool(true))
}

fn op_smaller_equal(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let mut prev = extract_number(&args[0])?;
    for arg in &args[1..] {
        let curr = extract_number(arg)?;
        if curr < prev {
            return Ok(Value::Bool(false));
        }
        prev = curr;
    }
    Ok(Value::Bool(true))
}

fn op_and(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 1)?;
    for arg in args {
        let result = eval.eval_value_public(arg.clone())?;
        if !result.is_truthy() {
            return Ok(Value::Int(0));
        }
    }
    Ok(Value::Int(1))
}

fn op_or(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 1)?;
    for arg in args {
        let result = eval.eval_value_public(arg.clone())?;
        if result.is_truthy() {
            return Ok(Value::Int(1));
        }
    }
    Ok(Value::Int(0))
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

fn extract_string(v: &Value) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        _ => Err("Expected string".to_string()),
    }
}

fn extract_number(v: &Value) -> Result<f64, String> {
    match v {
        Value::Int(i) => Ok(*i as f64),
        Value::Float(f) => Ok(*f),
        _ => Err("Expected number".to_string()),
    }
}

fn interpolate(fmt: &str, args: &[String]) -> String {
    let mut result = fmt.to_string();
    for (i, arg) in args.iter().enumerate() {
        result = result.replace(&format!("{{{}}}", i), arg);
    }
    result
}

pub fn register_core(disp: &mut Dispatcher) {
    disp.register(Operator::Not, op_not);
    disp.register(Operator::Eq, op_eq);
    disp.register(Operator::Neq, op_neq);
    disp.register(Operator::Larger, op_larger);
    disp.register(Operator::Smaller, op_smaller);
    disp.register(Operator::LargerEqual, op_larger_equal);
    disp.register(Operator::SmallerEqual, op_smaller_equal);
    disp.register(Operator::And, op_and);
    disp.register(Operator::Or, op_or);
    disp.register(Operator::If, op_if);
    disp.register(Operator::Define, op_define);
    disp.register(Operator::Set, op_set);
    disp.register(Operator::Let, op_let);
    disp.register(Operator::Do, op_do);
    disp.register(Operator::While, op_while);
    disp.register(Operator::When, op_when);
    disp.register(Operator::Unless, op_unless);
    disp.register(Operator::Cond, op_cond);
    disp.register(Operator::Print, op_print);
    disp.register(Operator::Printf, op_printf);
    disp.register(Operator::Exit, op_exit);
    disp.register(Operator::Type, op_type);
    disp.register(Operator::Alias, op_alias);
    disp.register(Operator::Unalias, op_unalias);
}