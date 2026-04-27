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

fn op_groups(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    // (groups posts*) — find all signal name prefixes matching all post suffixes
    ensure_arity_atleast(args, 1)?;

    if let Some(traces) = env.get_traces() {
        let traces = traces.read().unwrap_or_else(|e| e.into_inner());
        let all_sigs: Vec<String> = traces.all_signals();
        let posts: Vec<String> = args.iter()
            .map(|v| match v {
                Value::Symbol(s) => s.name.clone(),
                Value::String(s) => s.clone(),
                _ => format!("{}", v),
            })
            .collect();

        // For each post suffix, find signals that end with it, extract prefix
        let mut prefix_sets: Vec<Vec<String>> = Vec::new();
        for post in &posts {
            let mut prefixes = Vec::new();
            for sig in &all_sigs {
                if sig.ends_with(post.as_str()) && sig.len() > post.len() {
                    let prefix = &sig[..sig.len() - post.len()];
                    if !prefixes.contains(&prefix.to_string()) {
                        prefixes.push(prefix.to_string());
                    }
                }
            }
            prefix_sets.push(prefixes);
        }

        // Find prefixes common to all post suffixes
        if prefix_sets.is_empty() {
            return Ok(Value::List(crate::wal::ast::WList::new()));
        }
        let mut common = prefix_sets[0].clone();
        for ps in &prefix_sets[1..] {
            common.retain(|p| ps.contains(p));
        }
        let result: Vec<Value> = common.into_iter().map(Value::String).collect();
        return Ok(Value::List(crate::wal::ast::WList::from_vec(result)));
    }
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

fn op_in_groups(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    // (in-groups groups body+) — eval body in each group context
    ensure_arity_atleast(args, 2)?;
    let groups = match &args[0] {
        Value::List(lst) => lst.0.clone(),
        _ => return Err("in-groups: first argument must be a list of group names".to_string()),
    };

    let mut result = Value::Nil;
    for g in &groups {
        let group_name = match g {
            Value::String(s) => s.clone(),
            _ => format!("{}", g),
        };
        let mut group_env = env.child();
        group_env.set_group(&group_name);
        let saved_env = std::mem::replace(&mut *env, group_env);
        for arg in &args[1..] {
            result = eval.eval_value_public(arg.clone())?;
        }
        let _ = std::mem::replace(&mut *env, saved_env);
    }
    Ok(result)
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