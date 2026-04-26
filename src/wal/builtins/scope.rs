//! Scope builtin operators
//!
//! scoped, all-scopes, resolve-scope, set-scope, unset-scope, groups, in-group, in-groups, resolve-group

use crate::wal::ast::{Value, Operator};
use crate::wal::eval::{Environment, Dispatcher, Evaluator};

fn op_scoped(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let scope_name = extract_symbol(&args[0])?;
    let mut new_env = env.child();
    new_env.set_scope(&scope_name);
    let mut result = Value::Nil;
    for arg in &args[1..] {
        result = arg.clone();
    }
    Ok(result)
}

fn op_allscopes(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    // all-scopes expr - evaluate expr in all scopes
    Ok(Value::List(crate::wal::ast::WList::new()))
}

fn op_resolve_scope(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let name = extract_symbol(&args[0])?;
    let scoped_name = format!("{}{}", env.get_scope(), name);
    env.lookup(&scoped_name).ok_or_else(|| format!("Unresolved scope: {}", scoped_name))
}

fn op_set_scope(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let scope = extract_symbol(&args[0])?;
    env.set_scope(&scope);
    Ok(Value::Nil)
}

fn op_unset_scope(_args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    env.set_scope("");
    Ok(Value::Nil)
}

fn op_groups(_args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    // groups pattern... - find groups matching patterns
    Ok(Value::List(crate::wal::ast::WList::new()))
}

fn op_in_group(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let group_name = extract_symbol(&args[0])?;
    let mut new_env = env.child();
    new_env.set_group(&group_name);
    let mut result = Value::Nil;
    for arg in &args[1..] {
        result = arg.clone();
    }
    Ok(result)
}

fn op_in_groups(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    // in-groups (groups...) expr
    Ok(Value::List(crate::wal::ast::WList::new()))
}

fn op_resolve_group(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let name = match &args[0] {
        Value::Symbol(s) => s.name.clone(),
        Value::List(lst) if !lst.is_empty() => {
            // Handle (quote signal) form from parser -> extract the quoted symbol
            match &lst.0[0] {
                Value::Symbol(s) => s.name.clone(),
                _ => return Err("resolve-group: expected symbol".to_string()),
            }
        }
        _ => return Err("resolve-group: expected symbol".to_string()),
    };
    let grouped_name = format!("#{}", name);
    env.lookup(&grouped_name).ok_or_else(|| format!("Unresolved group: {}", grouped_name))
}

fn op_in_scope(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 1)?;
    let scope = extract_symbol(&args[0])?;
    let mut new_env = env.child();
    new_env.set_scope(&scope);
    let mut result = Value::Nil;
    for arg in &args[1..] {
        result = eval.eval_value_public(arg.clone())?;
    }
    Ok(result)
}

fn op_in_scopes(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let scope_names = match &args[0] {
        Value::List(lst) => {
            lst.0.iter().map(|v| extract_symbol(v)).collect::<Result<Vec<_>, _>>()?
        }
        _ => return Err("in-scopes: first argument must be a list of scope names".to_string()),
    };
    let mut result = Value::Nil;
    for scope in scope_names {
        let mut scoped_env = env.child();
        scoped_env.set_scope(&scope);
        for arg in &args[1..] {
            let saved_env = std::mem::replace(eval.env_mut(), scoped_env);
            result = eval.eval_value_public(arg.clone())?;
            scoped_env = std::mem::replace(eval.env_mut(), saved_env);
        }
    }
    Ok(result)
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

pub fn register_scope(disp: &mut Dispatcher) {
    disp.register(Operator::Allscopes, op_allscopes);
    disp.register(Operator::Scoped, op_scoped);
    disp.register(Operator::ResolveScope, op_resolve_scope);
    disp.register(Operator::Setscope, op_set_scope);
    disp.register(Operator::UnsetScope, op_unset_scope);
    disp.register(Operator::Groups, op_groups);
    disp.register(Operator::InGroup, op_in_group);
    disp.register(Operator::InGroups, op_in_groups);
    disp.register(Operator::InScope, op_in_scope);
    disp.register(Operator::InScopes, op_in_scopes);
    disp.register(Operator::ResolveGroup, op_resolve_group);
}