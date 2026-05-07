//! Type builtin operators
//!
//! defined?, atom?, symbol?, string?, int?, list?, type conversions

use crate::wal::ast::{Value, Operator, Symbol};
use crate::wal::eval::{Environment, Dispatcher, Evaluator};

fn op_defined_p(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let name = extract_symbol(&args[0])?;
    Ok(Value::Bool(env.lookup(&name).is_some()))
}

fn op_atom_p(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let is_atom = match &args[0] {
        Value::Nil | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::String(_) => true,
        Value::Symbol(_) | Value::Closure(_) | Value::Macro(_) => true,
        Value::List(lst) => lst.is_empty(),
        Value::Unquote(_) | Value::UnquoteSplice(_) => true,
    };
    Ok(Value::Bool(is_atom))
}

fn op_symbol_p(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    Ok(Value::Bool(matches!(&args[0], Value::Symbol(_))))
}

fn op_string_p(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    Ok(Value::Bool(matches!(&args[0], Value::String(_))))
}

fn op_int_p(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    Ok(Value::Bool(matches!(&args[0], Value::Int(_))))
}

fn op_list_p(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    Ok(Value::Bool(matches!(&args[0], Value::List(_))))
}

fn op_convert_binary(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if args.len() < 1 || args.len() > 2 {
        return Err(format!("convert/bin expects 1 or 2 arguments, got {}", args.len()));
    }
    let num = match &args[0] {
        Value::Int(i) => *i,
        Value::String(s) => s.parse::<i64>().map_err(|_| "Cannot convert to binary".to_string())?,
        _ => return Err("convert/bin expects number".to_string()),
    };
    let bin_str = format!("{:b}", num);
    if args.len() == 2 {
        let width = match &args[1] {
            Value::Int(i) => *i as usize,
            _ => return Err("convert/bin: second argument must be integer width".to_string()),
        };
        if bin_str.len() < width {
            Ok(Value::String(format!("{:0>width$}", bin_str)))
        } else {
            Ok(Value::String(bin_str))
        }
    } else {
        Ok(Value::String(bin_str))
    }
}

fn op_string_to_int(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    match &args[0] {
        Value::String(s) => {
            s.parse::<i64>().map(Value::Int).map_err(|_| "Cannot parse int".to_string())
        }
        Value::Int(i) => Ok(Value::Int(*i)),
        _ => Err("string->int expects string".to_string()),
    }
}

fn op_bits_to_sint(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let evaluated = eval.eval_value_public(args[0].clone())?;
    match evaluated {
        Value::String(s) => {
            if s.is_empty() {
                return Err("bits->sint: empty string".to_string());
            }
            if s.chars().all(|c| c == '0' || c == '1') {
                if s.starts_with('1') {
                    let inverted: String = s.chars().map(|c| if c == '0' { '1' } else { '0' }).collect();
                    let u_res = isize::from_str_radix(&inverted, 2).map_err(|_| "invalid bits")? + 1;
                    Ok(Value::Int(-u_res as i64))
                } else {
                    let v = isize::from_str_radix(&s, 2).map_err(|_| "invalid bits")?;
                    Ok(Value::Int(v as i64))
                }
            } else {
                Err("bits->sint: string must contain only 0 and 1".to_string())
            }
        }
        _ => Err("bits->sint: argument must evaluate to string".to_string()),
    }
}

fn op_symbol_to_string(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    match &args[0] {
        Value::Symbol(s) => Ok(Value::String(s.name.clone())),
        Value::String(s) => Ok(Value::String(s.clone())),
        _ => Err("symbol->string expects symbol".to_string()),
    }
}

fn op_string_to_symbol(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    match &args[0] {
        Value::String(s) => Ok(Value::Symbol(Symbol::new(s))),
        Value::Symbol(s) => Ok(Value::Symbol(s.clone())),
        _ => Err("string->symbol expects string".to_string()),
    }
}

fn op_int_to_string(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    match &args[0] {
        Value::Int(i) => Ok(Value::String(i.to_string())),
        Value::Float(f) => Ok(Value::String(format!("{}", f))),
        _ => Err("int->string expects number".to_string()),
    }
}

fn op_string_append(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let mut result = String::new();
    for arg in args {
        match arg {
            Value::String(s) => result.push_str(s),
            other => result.push_str(&format!("{}", other)),
        }
    }
    Ok(Value::String(result))
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

pub fn register_types(disp: &mut Dispatcher) {
    disp.register(Operator::IsDefined, op_defined_p);
    disp.register(Operator::IsAtom, op_atom_p);
    disp.register(Operator::IsSymbol, op_symbol_p);
    disp.register(Operator::IsString, op_string_p);
    disp.register(Operator::IsInt, op_int_p);
    disp.register(Operator::IsList, op_list_p);
    disp.register(Operator::ConvertBinary, op_convert_binary);
    disp.register(Operator::StringToInt, op_string_to_int);
    disp.register(Operator::BitsToSint, op_bits_to_sint);
    disp.register(Operator::StringToSymbol, op_symbol_to_string);
    disp.register(Operator::SymbolToString, op_string_to_symbol);
    disp.register(Operator::IntToString, op_int_to_string);
    disp.register(Operator::StringAppend, op_string_append);
}