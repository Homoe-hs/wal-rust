//! Basic Evaluator for WAL

use crate::wal::ast::{Value, Symbol, WList, Closure, Operator};
use crate::wal::eval::{Environment, Dispatcher, SemanticChecker};
use crate::wal::builtins;
use crate::wal::builtins::special::quasiquote_eval;
use crate::trace::{FindCondition, BatchEntry, TraceContainer, SharedTraceContainer, ScalarValue};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, RwLock};

#[derive(Debug)]
pub struct Evaluator {
    pub env: Environment,
    pub disp: Dispatcher,
    pub traces: SharedTraceContainer,
}

impl Evaluator {
    pub fn new() -> Self {
        let traces = Arc::new(RwLock::new(TraceContainer::new()));
        let traces_disp = traces.clone();
        let traces_env = traces.clone();

        let mut disp = Dispatcher::new();
        disp.set_traces(traces_disp);
        builtins::register_all(&mut disp);

        let mut env = Environment::new();
        env.set_traces(traces_env);

        Evaluator {
            env,
            disp,
            traces,
        }
    }

    pub fn eval(&mut self, source: &str) -> Result<Value, String> {
        let mut parser = crate::wal::WalParser::new()?;
        let value = parser.parse_expr(source)?;
        self.eval_value(value)
    }
}

/// Collect all top-level expression nodes from the parse tree
#[allow(dead_code)]
fn collect_top_level_sexprs(node: tree_sitter::Node) -> Vec<tree_sitter::Node> {
    let mut result = Vec::new();
    let kind = node.kind();

    match kind {
        "sexpr" | "atom" | "list" => {
            result.push(node);
        }
        "sexpr_list" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                result.extend(collect_top_level_sexprs(child));
            }
        }
        "program" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                result.extend(collect_top_level_sexprs(child));
            }
        }
        _ => {}
    }

    result
}

impl Evaluator {

    pub fn eval_value(&mut self, value: Value) -> Result<Value, String> {
        match value {
            Value::Symbol(s) => self.eval_symbol(s),
            Value::List(lst) => self.eval_list(lst),
            _ => Ok(value),
        }
    }

    fn eval_symbol(&mut self, sym: Symbol) -> Result<Value, String> {
        // Resolve alias chain
        let name = if let Some(target) = self.env.resolve_alias(&sym.name) {
            target.to_string()
        } else {
            sym.name.clone()
        };

        if let Some(v) = self.env.lookup(&name) {
            // Virtual signals: re-evaluate at each access (dynamic computation)
            if self.env.is_virtual_signal(&name) {
                return self.eval_value(v);
            }
            return Ok(v);
        }

        if let Some(op) = Operator::from_str(&name) {
            return Ok(Value::Symbol(Symbol::new(op.as_str())));
        }

        // Special variables (require trace access)
        match name.as_str() {
            "INDEX" => {
                if let Some(traces) = self.env.get_traces() {
                    let traces = traces.read().unwrap_or_else(|e| e.into_inner());
                    if let Some(t) = traces.first_trace() {
                        return Ok(Value::Int(t.index() as i64));
                    }
                }
                return Ok(Value::Int(0));
            }
            "MAX-INDEX" => {
                if let Some(traces) = self.env.get_traces() {
                    let traces = traces.read().unwrap_or_else(|e| e.into_inner());
                    if let Some(t) = traces.first_trace() {
                        return Ok(Value::Int(t.max_index() as i64));
                    }
                }
                return Ok(Value::Int(0));
            }
            "SIGNALS" => {
                if let Some(traces) = self.env.get_traces() {
                    let traces = traces.read().unwrap_or_else(|e| e.into_inner());
                    let sigs: Vec<Value> = traces.all_signals().into_iter()
                        .map(Value::String).collect();
                    return Ok(Value::List(WList::from_vec(sigs)));
                }
                return Ok(Value::List(WList::new()));
            }
            "CG" => {
                return Ok(Value::String(self.env.get_group().to_string()));
            }
            "CS" => {
                return Ok(Value::String(self.env.get_scope().to_string()));
            }
            "TS" => {
                if let Some(traces) = self.env.get_traces() {
                    let traces = traces.read().unwrap_or_else(|e| e.into_inner());
                    if let Some(t) = traces.first_trace() {
                        return Ok(Value::Int(t.index() as i64));
                    }
                }
                return Ok(Value::Int(0));
            }
            "TRACE-NAME" => {
                if let Some(traces) = self.env.get_traces() {
                    let traces = traces.read().unwrap_or_else(|e| e.into_inner());
                    if let Some(t) = traces.first_trace() {
                        return Ok(Value::String(t.id().to_string()));
                    }
                }
                return Ok(Value::String("".to_string()));
            }
            "TRACE-FILE" => {
                if let Some(traces) = self.env.get_traces() {
                    let traces = traces.read().unwrap_or_else(|e| e.into_inner());
                    if let Some(t) = traces.first_trace() {
                        return Ok(Value::String(t.filename().to_string()));
                    }
                }
                return Ok(Value::String("".to_string()));
            }
            "SIGNALS-NO-ALIAS" => {
                if let Some(traces) = self.env.get_traces() {
                    let traces = traces.read().unwrap_or_else(|e| e.into_inner());
                    let sigs: Vec<Value> = traces.all_signals().into_iter()
                        .map(Value::String).collect();
                    return Ok(Value::List(WList::from_vec(sigs)));
                }
                return Ok(Value::List(WList::new()));
            }
            "SCOPES" => {
                if let Some(traces) = self.env.get_traces() {
                    let traces = traces.read().unwrap_or_else(|e| e.into_inner());
                    let mut all_scopes = Vec::new();
                    for trace in traces.traces_iter() {
                        all_scopes.extend(trace.scopes());
                    }
                    all_scopes.sort();
                    all_scopes.dedup();
                    return Ok(Value::List(WList::from_vec(
                        all_scopes.into_iter().map(Value::String).collect()
                    )));
                }
                return Ok(Value::List(WList::new()));
            }
            "LOCAL-SIGNALS" => {
                let cs = self.env.get_scope();
                if let Some(traces) = self.env.get_traces() {
                    let traces = traces.read().unwrap_or_else(|e| e.into_inner());
                    let sigs: Vec<Value> = traces.all_signals().into_iter()
                        .filter(|s| s.starts_with(&cs))
                        .map(Value::String).collect();
                    return Ok(Value::List(WList::from_vec(sigs)));
                }
                return Ok(Value::List(WList::new()));
            }
            "LOCAL-SCOPES" => {
                let cs = self.env.get_scope();
                if let Some(traces) = self.env.get_traces() {
                    let traces = traces.read().unwrap_or_else(|e| e.into_inner());
                    let mut local = Vec::new();
                    for trace in traces.traces_iter() {
                        for s in trace.scopes() {
                            if s.starts_with(&cs) && s.len() > cs.len() {
                                let rest = &s[cs.len()..];
                                if let Some(dot) = rest.find('.') {
                                    local.push(format!("{}{}", cs, &rest[..dot+1]));
                                } else {
                                    local.push(s.clone());
                                }
                            }
                        }
                    }
                    local.sort();
                    local.dedup();
                    return Ok(Value::List(WList::from_vec(
                        local.into_iter().map(Value::String).collect()
                    )));
                }
                return Ok(Value::List(WList::new()));
            }
            "VIRTUAL-SIGNALS" => {
                let names = self.env.virtual_signal_names();
                return Ok(Value::List(WList::from_vec(
                    names.into_iter().map(|s| {
                        if s.starts_with('"') && s.ends_with('"') {
                            Value::String(s[1..s.len()-1].to_string())
                        } else {
                            Value::String(s)
                        }
                    }).collect()
                )));
            }
            _ => {}
        }

        // Try signal name auto-lookup from loaded traces
        // WAL spec: bare signal names return their waveform value at current INDEX
        // Try name as-is, then prepend scope, then prepend group
        if let Some(traces) = self.env.get_traces() {
            let traces = traces.read().unwrap_or_else(|e| e.into_inner());
            let candidates = [
                name.clone(),
                format!("{}{}", self.env.get_scope(), name),
                format!("{}{}", self.env.get_group(), name),
            ];
            for candidate in &candidates {
                for id in traces.trace_ids() {
                    if let Some(sigs) = traces.signals(&id) {
                        if sigs.contains(candidate) {
                            let get_expr = Value::List(WList::from_vec(vec![
                                Value::Symbol(Symbol::new("get")),
                                Value::String(candidate.clone()),
                            ]));
                            return self.eval_value(get_expr);
                        }
                    }
                }
            }
            // Fuzzy fallback: try suffix / substring match
            for id in traces.trace_ids() {
                if let Some(sigs) = traces.signals(&id) {
                    let (matched, candidates) = fuzzy_match_signal(&name, &sigs);
                    if candidates.len() > 1 {
                        log::warn!("signal '{}' is ambiguous: matches {:?}, using '{}'",
                            name, &candidates[..candidates.len().min(5)],
                            matched.map(|s| s.as_str()).unwrap_or("?"));
                    }
                    if let Some(matched) = matched {
                        let get_expr = Value::List(WList::from_vec(vec![
                            Value::Symbol(Symbol::new("get")),
                            Value::String(matched.clone()),
                        ]));
                        return self.eval_value(get_expr);
                    }
                }
            }
        }

        Err(format!("Undefined symbol: {}", name))
    }

    fn eval_list(&mut self, lst: WList) -> Result<Value, String> {
        if lst.is_empty() {
            return Ok(Value::List(lst));
        }

        let first = lst.first().ok_or("Empty list")?;
        let rest = lst.rest();

        match first {
            Value::Symbol(s) => {
                // Handle defun macro: (defun name (args...) body...) -> (define name (fn (args...) body...))
                if s.name == "defun" {
                    return self.eval_defun_macro(&rest);
                }
                // Handle defunm macro: (defunm name (args...) body...) -> (defmacro name (args...) body...)
                if s.name == "defunm" {
                    return self.eval_defunm_macro(&rest);
                }
                // Handle set! macro: (set! x val) -> (set x val)
                if s.name == "set!" {
                    return self.eval_set_macro(&rest);
                }
                // Handle for/list macro: (for/list (x lst) body...) -> for expanded form
                if s.name == "for/list" {
                    return self.eval_for_list_macro(&rest);
                }
                // Handle timeframe special form (body not pre-evaluated)
                if s.name == "timeframe" {
                    return self.eval_timeframe(&rest);
                }

                // Handle alias special form — first arg is literal symbol, not evaluated
                if s.name == "alias" {
                    return self.eval_alias(&rest);
                }
                // Handle unalias special form — first arg is literal symbol
                if s.name == "unalias" {
                    return self.eval_unalias(&rest);
                }

                if let Some(op) = Operator::from_str(&s.name) {
                    if op == Operator::Defsig {
                        return self.eval_defsig(&rest);
                    } else if op == Operator::Define {
                        return self.eval_define(&rest);
                    } else if op == Operator::If {
                        return self.eval_if(&rest);
                    } else if op == Operator::Case {
                        return self.eval_case(&rest);
                    } else if op == Operator::Scoped {
                        return self.eval_scoped(&rest);
                    } else if op == Operator::InGroup {
                        return self.eval_in_group(&rest);
                    } else if op == Operator::InScope {
                        return self.eval_in_scope(&rest);
                    } else if op == Operator::InScopes {
                        return self.eval_in_scopes(&rest);
                    } else if op == Operator::InGroups {
                        return self.eval_in_groups(&rest);
                    } else if op == Operator::Let {
                        return self.eval_let(&rest);
                    } else if op == Operator::Quasiquote {
                        return self.eval_quasiquote(&rest);
                    } else if op == Operator::Quote {
                        return self.eval_quote(&rest);
                    } else if op == Operator::RelEval {
                        return self.eval_releval(&rest);
                    } else if op == Operator::Find {
                        return self.eval_find(&rest);
                    } else if op == Operator::FindG {
                        return self.eval_find_g(&rest);
                    } else if op == Operator::Count {
                        return self.eval_count(&rest);
                    } else if op == Operator::Whenever {
                        return self.eval_whenever(&rest);
                    } else if op == Operator::Fn {
                        // fn is a special form — create closure, then call with remaining args
                        if rest.len() <= 2 {
                            // Just create closure (no extra call args)
                            return self.eval_fn_special(&rest);
                        }
                        // Create closure and call with args[2..]
                        let closure_val = self.eval_fn_special(&rest)?;
                        match closure_val {
                            Value::Closure(c) => {
                                let call_args: Result<Vec<Value>, String> = rest[2..].iter()
                                    .map(|a| self.eval_value(a.clone()))
                                    .collect();
                                self.eval_closure(c, &call_args?)
                            }
                            Value::Macro(m) => {
                                self.eval_macro(m, &rest[2..].to_vec())
                            }
                            _ => Ok(closure_val),
                        }
                    } else {
                        let mut evaluated_args = Vec::new();
                        for arg in &rest {
                            evaluated_args.push(self.eval_value(arg.clone())?);
                        }
                        self.eval_dispatch(op, &evaluated_args)
                    }
                } else if let Some(v) = self.env.lookup(&s.name) {
                    match v {
                        Value::Closure(c) => {
                            let mut evaluated_args = Vec::new();
                            for arg in &rest {
                                evaluated_args.push(self.eval_value(arg.clone())?);
                            }
                            self.eval_closure(c, &evaluated_args)
                        }
                        Value::Macro(m) => {
                            let unevaluated_args: Vec<Value> = rest.into();
                            self.eval_macro(m, &unevaluated_args)
                        }
                        _ => Ok(v),
                    }
                } else if let Some(v) = self.env.lookup_global(&s.name) {
                    match v {
                        Value::Closure(c) => {
                            let mut evaluated_args = Vec::new();
                            for arg in &rest {
                                evaluated_args.push(self.eval_value(arg.clone())?);
                            }
                            self.eval_closure(c, &evaluated_args)
                        }
                        Value::Macro(m) => {
                            let unevaluated_args: Vec<Value> = rest.into();
                            self.eval_macro(m, &unevaluated_args)
                        }
                        _ => Ok(v),
                    }
                } else {
                    Err(format!("Unknown operator or function: {}", s.name))
                }
            }
            Value::List(ref inner) => {
                // Evaluate the inner list as a whole expression (handles fn/closure creation)
                let first_val = self.eval_value(Value::List(inner.clone()))?;
                match first_val {
                    Value::Closure(c) => {
                        let mut args = Vec::new();
                        for v in lst.0.iter().skip(1) {
                            args.push(self.eval_value(v.clone())?);
                        }
                        return self.eval_closure(c, &args);
                    }
                    Value::Macro(m) => {
                        let args: Vec<Value> = lst.0[1..].to_vec();
                        return self.eval_macro(m, &args);
                    }
                    _ => {
                        let mut evaluated = Vec::new();
                        evaluated.push(first_val);
                        for v in lst.0.iter().skip(1) {
                            evaluated.push(self.eval_value(v.clone())?);
                        }
                        self.eval_list(WList::from_vec(evaluated))
                    }
                }
            }
                _ => {
                    // First element is not a symbol: evaluate all elements
                    // and return as a list (not a function application)
                    let mut all_vals = Vec::with_capacity(lst.len());
                    for v in lst.0.iter() {
                        all_vals.push(self.eval_value(v.clone())?);
                    }
                    Ok(Value::List(WList::from_vec(all_vals)))
                }
        }
    }

pub fn eval_closure(&mut self, closure: Closure, args: &[Value]) -> Result<Value, String> {
        let closure_env = closure.env.clone();
        let closure_name = closure.name().map(|s| s.to_string());

        let mut closure_env_mut = closure_env.borrow().clone();
        closure_env_mut.set_parent(Some(Rc::new(RefCell::new(self.env.clone()))));
        let closure_rc = Rc::new(RefCell::new(closure_env_mut));

        let mut local_env = Environment::with_parent(closure_rc);

        if closure.variadic {
            if let Some(first_arg) = closure.args.first() {
                local_env.define(first_arg.name.clone(), Value::List(WList::from_vec(args.to_vec())));
            }
        } else {
            if let Some(err) = SemanticChecker::validate_closure_args(&closure.args, args) {
                return Err(err.message());
            }
            for (i, arg) in closure.args.iter().enumerate() {
                let value = args.get(i).cloned().unwrap_or(Value::Nil);
                local_env.define(arg.name.clone(), value);
            }
        }

        if let Some(name) = closure_name {
            local_env.define(name, Value::Closure(closure.clone()));
        }

        let saved_env = std::mem::replace(&mut self.env, local_env);
        let result = self.eval_value(*closure.body);
        self.env = saved_env;
        result
    }

    pub(crate) fn eval_macro(&mut self, macro_obj: crate::wal::ast::Macro, args: &[Value]) -> Result<Value, String> {
        let mut local_env = self.env.child();

        if macro_obj.variadic {
            if let Some(first_arg) = macro_obj.args.first() {
                local_env.define(first_arg.name.clone(), Value::List(WList::from_vec(args.to_vec())));
            }
        } else {
            for (i, arg_name) in macro_obj.args.iter().enumerate() {
                let value = args.get(i).cloned().unwrap_or(Value::Nil);
                local_env.define(arg_name.name.clone(), value);
            }
        }

        let saved_env = std::mem::replace(&mut self.env, local_env);
        let expanded = self.eval_value(*macro_obj.body.clone());
        self.env = saved_env;

        let expanded = expanded?;
        self.eval_value(expanded)
    }

    fn eval_set(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.is_empty() {
            return Err("set expects at least 1 argument".to_string());
        }

        // Single mode: (set! name value)
        if matches!(&args[0], Value::Symbol(_))  {
            if args.len() != 2 {
                return Err(format!("set expects 2 arguments for single mode, got {}", args.len()));
            }
            let name = match &args[0] {
                Value::Symbol(s) => s.name.clone(),
                _ => unreachable!(),
            };
            let value = self.eval_value(args[1].clone())?;
            self.env.set(&name, value.clone())?;
            return Ok(value);
        }

        // Multi-pair mode: (set! (name1 val1) (name2 val2) ...)
        let mut result = Value::Nil;
        for arg in args {
            let pair = match arg {
                Value::List(lst) if lst.len() == 2 => lst,
                _ => return Err(format!("set: each argument must be a (name value) pair, got {:?}", arg)),
            };
            let name = match &pair.0[0] {
                Value::Symbol(s) => s.name.clone(),
                _ => return Err("set: pair first element must be a symbol".to_string()),
            };
            let value = self.eval_value(pair.0[1].clone())?;
            self.env.set(&name, value.clone())?;
            result = value;
        }
        Ok(result)
    }

    fn eval_defsig(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 2 {
            return Err(format!("defsig expects 2 arguments, got {}", args.len()));
        }
        let name = match &args[0] {
            Value::Symbol(s) => s.name.clone(),
            Value::String(s) => s.clone(),
            _ => return Err("defsig: first argument must be a symbol or string".to_string()),
        };
        // Store the UNEVALUATED expression
        self.env.define(name.clone(), args[1].clone());
        self.env.add_virtual_signal(&name);
        Ok(Value::Nil)
    }

    fn eval_define(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 2 {
            return Err(format!("define expects 2 arguments, got {}", args.len()));
        }

        match &args[0] {
            Value::Symbol(s) => {
                let name = s.name.clone();
                let (value, _is_closure) = match &args[1] {
                    Value::List(lst) if !lst.is_empty() => {
                        if let Some(Value::Symbol(fn_sym)) = lst.first() {
                            if fn_sym.name == "fn" {
                                let fn_list = lst.rest();
                                let fn_args_list = fn_list.first().ok_or("fn expects argument list")?;
                                let (closure_args, variadic) = match fn_args_list {
                                    Value::List(args_lst) => {
                                        let syms: Vec<Symbol> = args_lst.0.iter().filter_map(|v| {
                                            if let Value::Symbol(s) = v { Some(s.clone()) } else { None }
                                        }).collect();
                                        (syms, false)
                                    }
                                    Value::Symbol(s) => (vec![s.clone()], true),
                                    _ => return Err("fn expects argument list".to_string()),
                                };
                                let body = if fn_list.len() > 1 {
                                    fn_list[1].clone()
                                } else {
                                    Value::Nil
                                };
                                let mut closure = Closure::new(
                                    Rc::new(RefCell::new(self.env.clone())),
                                    closure_args,
                                    body,
                                );
                                closure.variadic = variadic;
                                (Value::Closure(closure), true)
                            } else {
                                (self.eval_value(args[1].clone())?, false)
                            }
                        } else {
                            (self.eval_value(args[1].clone())?, false)
                        }
                    }
                    _ => (self.eval_value(args[1].clone())?, false),
                };
                self.env.define(name, value.clone());
                return Ok(value);
            }
            Value::List(list) => {
                if list.is_empty() {
                    return Err("define expects function name or symbol".to_string());
                }
                let func_name = match &list[0] {
                    Value::Symbol(s) => s.name.clone(),
                    _ => return Err("define expects function name as first element".to_string()),
                };
                let closure_args: Vec<Symbol> = list[1..]
                    .iter()
                    .map(|v| match v {
                        Value::Symbol(s) => Ok(s.clone()),
                        _ => Err("Function argument must be a symbol".to_string()),
                    })
                    .collect::<Result<Vec<_>, _>>()?;

                let closure = Closure::new(
                    Rc::new(RefCell::new(self.env.clone())),
                    closure_args,
                    args[1].clone(),
                ).with_name(&func_name);

                let closure_val = Value::Closure(closure);
                self.env.define(func_name, closure_val.clone());
                Ok(closure_val)
            }
            _ => Err("define expects symbol or function definition".to_string()),
        }
    }

    fn eval_if(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() < 2 {
            return Err(format!("if expects at least 2 arguments, got {}", args.len()));
        }
        let cond = self.eval_value(args[0].clone())?;
        if cond.is_truthy() {
            self.eval_value(args[1].clone())
        } else if args.len() >= 3 {
            self.eval_value(args[2].clone())
        } else {
            Ok(Value::Nil)
        }
    }

    fn eval_case(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("case expects at least key and one clause".to_string());
        }
        let key = self.eval_value(args[0].clone())?;
        for clause in &args[1..] {
            match clause {
                Value::List(lst) if lst.len() >= 1 => {
                    let clause_key = &lst[0];
                    if matches!(clause_key, Value::Symbol(s) if s.name == "default") {
                        let mut result = Value::Nil;
                        for expr in lst.rest() {
                            result = self.eval_value(expr)?;
                        }
                        return Ok(result);
                    }
                    let val = self.eval_value(clause_key.clone())?;
                    if val == key {
                        let mut result = Value::Nil;
                        for expr in lst.rest() {
                            result = self.eval_value(expr)?;
                        }
                        return Ok(result);
                    }
                }
                _ => return Err("case clause must be a list (value expr...)".to_string()),
            }
        }
        Ok(Value::Nil)
    }

    fn eval_scoped(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("scoped expects at least a scope name and body".to_string());
        }
        let scope_name = scope_extract_name(&args[0])?;
        let mut new_env = self.env.child();
        new_env.set_scope(&scope_name);
        let saved_env = std::mem::replace(&mut self.env, new_env);
        let mut result = Value::Nil;
        for arg in &args[1..] {
            result = self.eval_value(arg.clone())?;
        }
        self.env = saved_env;
        Ok(result)
    }

    fn eval_in_group(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("in-group expects at least a group name and body".to_string());
        }
        let group_name = scope_extract_name(&args[0])?;
        let mut new_env = self.env.child();
        new_env.set_group(&group_name);
        let saved_env = std::mem::replace(&mut self.env, new_env);
        let mut result = Value::Nil;
        for arg in &args[1..] {
            result = self.eval_value(arg.clone())?;
        }
        self.env = saved_env;
        Ok(result)
    }

    fn eval_in_scope(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("in-scope expects at least a scope name and body".to_string());
        }
        let scope_name = scope_extract_name(&args[0])?;
        let mut new_env = self.env.child();
        new_env.set_scope(&scope_name);
        let saved_env = std::mem::replace(&mut self.env, new_env);
        let mut result = Value::Nil;
        for arg in &args[1..] {
            result = self.eval_value(arg.clone())?;
        }
        self.env = saved_env;
        Ok(result)
    }

    fn eval_in_scopes(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("in-scopes expects at least a scope list and body".to_string());
        }
        // Evaluate the first argument to get the list of scope names
        let scope_names: Vec<String> = match self.eval_value(args[0].clone())? {
            Value::List(lst) => lst.0.iter().map(scope_extract_name).collect::<Result<_, _>>()?,
            _ => return Err("in-scopes: first argument must evaluate to a list of scope names".to_string()),
        };
        let mut result = Value::Nil;
        for scope in &scope_names {
            let mut new_env = self.env.child();
            new_env.set_scope(scope);
            let saved_env = std::mem::replace(&mut self.env, new_env);
            for arg in &args[1..] {
                result = self.eval_value(arg.clone())?;
            }
            self.env = saved_env;
        }
        Ok(result)
    }

    fn eval_in_groups(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("in-groups expects at least a group list and body".to_string());
        }
        // Evaluate the first argument to get the list of group names
        let group_names: Vec<String> = match self.eval_value(args[0].clone())? {
            Value::List(lst) => lst.0.iter().map(|v| match v {
                Value::String(s) => Ok(s.clone()),
                Value::Symbol(s) => Ok(s.name.clone()),
                _ => Err("in-groups: group names must be strings or symbols".to_string()),
            }).collect::<Result<_, _>>()?,
            _ => return Err("in-groups: first argument must evaluate to a list of group names".to_string()),
        };
        let mut result = Value::Nil;
        for group in &group_names {
            let mut new_env = self.env.child();
            new_env.set_group(group);
            let saved_env = std::mem::replace(&mut self.env, new_env);
            for arg in &args[1..] {
                result = self.eval_value(arg.clone())?;
            }
            self.env = saved_env;
        }
        Ok(result)
    }

    fn eval_let(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() < 1 {
            return Err("let expects at least bindings".to_string());
        }
        let mut new_env = self.env.child();
        let bindings = match &args[0] {
            Value::List(list) => list.0.clone(),
            _ => return Err("let expects list of bindings".to_string()),
        };
        // Support both (let (x 1 y 2) body) and (let ([x 1] [y 2]) body) formats
        if bindings.len() >= 2 && bindings.iter().all(|b| matches!(b, Value::List(p) if p.len() == 2)) {
            for pair in &bindings {
                if let Value::List(pair_lst) = pair {
                    let name = match &pair_lst[0] {
                        Value::Symbol(s) => s.name.clone(),
                        _ => return Err("let binding name must be symbol".to_string()),
                    };
                    let value = self.eval_value(pair_lst[1].clone())?;
                    new_env.define(name, value);
                }
            }
        } else {
            for binding in bindings.chunks(2) {
                if binding.len() != 2 {
                    return Err("let binding must be (name value)".to_string());
                }
                let name = match &binding[0] {
                    Value::Symbol(s) => s.name.clone(),
                    _ => return Err("let binding name must be symbol".to_string()),
                };
                let value = self.eval_value(binding[1].clone())?;
                new_env.define(name, value);
            }
        }
        let saved_env = std::mem::replace(&mut self.env, new_env);
        let mut result = Value::Nil;
        for arg in &args[1..] {
            result = self.eval_value(arg.clone())?;
        }
        self.env = saved_env;
        Ok(result)
    }

    fn eval_quasiquote(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 1 {
            return Err(format!("quasiquote expects 1 argument, got {}", args.len()));
        }
        quasiquote_eval(&args[0], self)
    }

    fn eval_releval(&mut self, args: &[Value]) -> Result<Value, String> {
        // (reval expr offset) — evaluate expr at current_index + offset
        if args.len() != 2 {
            return Err(format!("reval expects 2 arguments, got {}", args.len()));
        }
        let offset = match self.eval_value(args[1].clone())? {
            Value::Int(i) => i,
            _ => return Err("reval: offset must be an integer".to_string()),
        };

        if let Some(traces) = self.traces.read().map(|g| g.trace_ids()).ok() {
            if traces.is_empty() {
                return Err("reval: no traces loaded".to_string());
            }
            // Save current indices
            let saved: Vec<(String, usize)> = {
                let t = self.traces.read().unwrap_or_else(|e| e.into_inner());
                traces.iter().filter_map(|tid| {
                    t.get(tid).map(|tr| (tid.clone(), tr.index()))
                }).collect()
            };

            // Check bounds and adjust indices
            {
                let mut t = self.traces.write().unwrap_or_else(|e| e.into_inner());
                for (tid, _) in &saved {
                    if let Some(tr) = t.get(tid) {
                        let new_idx = tr.index() as i64 + offset;
                        if new_idx < 0 || new_idx as usize > tr.max_index() {
                            _ = std::mem::drop(t);
                            // Restore all
                            let mut t2 = self.traces.write().unwrap_or_else(|e| e.into_inner());
                            for (tid, idx) in &saved {
                                let _ = t2.set_index(tid, *idx);
                            }
                            return Ok(Value::Bool(false));
                        }
                    }
                }
                for (tid, _) in &saved {
                    if let Some(tr) = t.get_mut(tid) {
                        let new_idx = (tr.index() as i64 + offset) as usize;
                        let _ = tr.set_index(new_idx);
                    }
                }
            }

            // Evaluate the expression
            let result = self.eval_value(args[0].clone());

            // Restore indices
            {
                let mut t = self.traces.write().unwrap_or_else(|e| e.into_inner());
                for (tid, idx) in &saved {
                    let _ = t.set_index(tid, *idx);
                }
            }

            result
        } else {
            Err("reval: no traces loaded".to_string())
        }
    }

    /// Try to parse a simple condition pattern like (= (get "signal") value)
    /// Returns (signal_name, target_value) if matched.
    /// Parse simple condition: (= (get "sig") val), (!= (get "sig") val), (not (= (get "sig") val))
    /// Returns (signal_name, target_value, is_not)
    fn parse_simple_condition(&self, expr: &Value) -> Option<(String, i64, bool)> {
        // Check for (not <expr>) wrapping
        // Also handles bare (get "sig") — a 2-element list starting with "get"
        let inner = if let Value::List(lst) = expr {
            if lst.len() == 2 {
                if let Value::Symbol(s) = &lst[0] {
                    if s.name == "not" || s.name == "!" {
                        Some((&lst[1], true))
                    } else if s.name == "get" {
                        // Bare (get "sig") — treat as (= (get "sig") 1)
                        let sig = match &lst[1] {
                            Value::String(s) => s.clone(),
                            Value::Symbol(s) => s.name.clone(),
                            _ => return None,
                        };
                        return Some((sig, 1, false));
                    } else {
                        None
                    }
                } else { None }
            } else if lst.len() == 3 {
                Some((expr, false))
            } else { None }
        } else { None };
        let (target_expr, is_not) = inner?;

        // Handle (= (get "sig") val), (!= (get "sig") val)
        let lst = match target_expr {
            Value::List(lst) if lst.len() == 3 => lst,
            _ => return None,
        };
        let op = match &lst[0] {
            Value::Symbol(s) => s.name.as_str(),
            _ => return None,
        };
        let negated_by_op = op == "!=";
        if op != "=" && op != "!=" { return None; }
        let effective_not = is_not ^ negated_by_op;

        for (a, b) in &[(0, 1), (1, 0), (1, 2)] {
            if let Value::List(inner) = &lst[*a] {
                if inner.len() == 2 {
                    if let Value::Symbol(fn_sym) = &inner[0] {
                        if fn_sym.name == "get" {
                            let sig = match &inner[1] {
                                Value::String(s) => s.clone(),
                                Value::Symbol(s) => s.name.clone(),
                                _ => continue,
                            };
                            let val = match &lst[*b] {
                                Value::Int(i) => *i,
                                _ => continue,
                            };
                            return Some((sig, val, effective_not));
                        }
                    }
                }
            }
        }
        None
    }

    /// Walk an expression and resolve variable references in (get <symbol>) calls.
    /// Returns the expression with symbols replaced by their string values.
    fn resolve_get_symbols(&self, expr: &Value) -> Value {
        match expr {
            Value::List(lst) => {
                if lst.len() == 2 && matches!(&lst[0], Value::Symbol(s) if s.name == "get") {
                    if let Value::Symbol(sym) = &lst[1] {
                        if let Some(val) = self.env.lookup(&sym.name) {
                            if matches!(val, Value::String(_)) {
                                let items = vec![
                                    Value::Symbol(Symbol::new("get".to_string())),
                                    val,
                                ];
                                return Value::List(WList::from_vec(items));
                            }
                        }
                    }
                }
                let resolved: Vec<Value> = lst.iter().map(|v| self.resolve_get_symbols(v)).collect();
                Value::List(WList::from_vec(resolved))
            }
            other => other.clone(),
        }
    }

    /// Build a FindCondition from target value and negation flag.
    fn build_cond(target: i64, is_not: bool) -> FindCondition {
        if is_not {
            if target <= 1 && target >= 0 { FindCondition::Neq(target as u8) }
            else { FindCondition::NeqI64(target) }
        } else {
            if target <= 1 && target >= 0 { FindCondition::Value(target as u8) }
            else { FindCondition::ValueI64(target) }
        }
    }

    /// Decompose (&& cond1 cond2 ...) into individual simple conditions.
    /// Returns None if it can't be decomposed.
    fn decompose_and_condition(&self, expr: &Value) -> Option<Vec<(String, FindCondition)>> {
        let lst = match expr {
            Value::List(lst) if lst.len() >= 3 => lst,
            _ => return None,
        };
        let op = match &lst[0] {
            Value::Symbol(s) => s.name.as_str(),
            _ => return None,
        };
        if op != "&&" && op != "and" { return None; }
        let mut subs = Vec::new();
        for sub in lst.iter().skip(1) {
            if let Some((sig, target, is_not)) = self.parse_simple_condition(sub) {
                subs.push((sig, Self::build_cond(target, is_not)));
            } else {
                return None; // Can't decompose non-simple sub-condition
            }
        }
        if subs.len() >= 2 { Some(subs) } else { None }
    }

    /// Try decomposing (&& ...) or (|| ...) for fast counting.
    /// Each sub-condition that matches (=/!= (get "sig") val) uses find_indices.
    /// For &&: verify remaining conditions on the smallest set of candidates.
    /// For ||: union all matching indices.
    fn decompose_and_count(&self, expr: &Value, trace_ids: &[String]) -> Result<Option<i64>, String> {
        let lst = match expr {
            Value::List(lst) if lst.len() == 3 => lst,
            _ => return Ok(None),
        };
        let op = match &lst[0] {
            Value::Symbol(s) => s.name.as_str(),
            _ => return Ok(None),
        };
        if op != "&&" && op != "||" { return Ok(None); }
        let is_and = op == "&&";

        // Extract sub-conditions
        let sub1 = &lst[1];
        let sub2 = &lst[2];

        // Try to find indices for each sub-condition
        let mut idx_sets: Vec<Vec<usize>> = Vec::new();

        for sub in [sub1, sub2] {
            if let Some((sig, target, is_not)) = self.parse_simple_condition(sub) {
                let cond: FindCondition = if is_not {
                    if target <= 1 && target >= 0 { FindCondition::Neq(target as u8) }
                    else { FindCondition::NeqI64(target) }
                } else {
                    if target <= 1 && target >= 0 { FindCondition::Value(target as u8) }
                    else { FindCondition::ValueI64(target) }
                };
                if let Ok(t) = self.traces.read() {
                    for tid in trace_ids {
                        if let Some(tr) = t.get(tid) {
                            let sigs = tr.signals();
                            let resolved = resolve_signal_name(&sig, &sigs)
                                .unwrap_or_else(|| sig.clone());
                            if let Ok(idxs) = tr.find_indices(&resolved, cond.clone()) {
                                idx_sets.push(idxs);
                            }
                        }
                    }
                }
            }
        }

        if idx_sets.is_empty() {
            return Ok(None); // can't decompose
        }

        Ok(Some(if is_and {
            idx_sets.sort_by_key(|s| s.len());
            let mut candidates = std::mem::take(&mut idx_sets[0]);
            if candidates.is_empty() { return Ok(Some(0)); }
            if idx_sets.len() >= 2 {
                let set2: std::collections::HashSet<usize> = idx_sets[1].iter().copied().collect();
                candidates.retain(|i| set2.contains(i));
            }
            candidates.len() as i64
        } else {
            let mut all: Vec<usize> = Vec::new();
            for s in &idx_sets { all.extend(s); }
            all.sort();
            all.dedup();
            all.len() as i64
        }))
    }

    /// Try decomposing (&& ...) or (|| ...) for fast find-indices.
    fn decompose_and_find_indices(&self, expr: &Value, trace_ids: &[String]) -> Result<Option<Vec<usize>>, String> {
        let lst = match expr {
            Value::List(lst) if lst.len() == 3 => lst,
            _ => return Ok(None),
        };
        let op = match &lst[0] {
            Value::Symbol(s) => s.name.as_str(),
            _ => return Ok(None),
        };
        if op != "&&" && op != "||" { return Ok(None); }
        let is_and = op == "&&";

        let sub1 = &lst[1];
        let sub2 = &lst[2];
        let mut idx_sets: Vec<Vec<usize>> = Vec::new();

        for sub in [sub1, sub2] {
            if let Some((sig, target, is_not)) = self.parse_simple_condition(sub) {
                let cond: FindCondition = if is_not {
                    if target <= 1 && target >= 0 { FindCondition::Neq(target as u8) }
                    else { FindCondition::NeqI64(target) }
                } else {
                    if target <= 1 && target >= 0 { FindCondition::Value(target as u8) }
                    else { FindCondition::ValueI64(target) }
                };
                if let Ok(t) = self.traces.read() {
                    for tid in trace_ids {
                        if let Some(tr) = t.get(tid) {
                            let sigs = tr.signals();
                            let resolved = resolve_signal_name(&sig, &sigs)
                                .unwrap_or_else(|| sig.clone());
                            if let Ok(idxs) = tr.find_indices(&resolved, cond.clone()) {
                                idx_sets.push(idxs);
                            }
                        }
                    }
                }
            }
        }

        if idx_sets.is_empty() {
            return Ok(None);
        }

        Ok(Some(if is_and {
            idx_sets.sort_by_key(|s| s.len());
            let mut candidates = std::mem::take(&mut idx_sets[0]);
            if idx_sets.len() >= 2 {
                let set2: std::collections::HashSet<usize> = idx_sets[1].iter().copied().collect();
                candidates.retain(|i| set2.contains(i));
            }
            candidates
        } else {
            let mut all: Vec<usize> = Vec::new();
            for s in &idx_sets { all.extend(s); }
            all.sort();
            all.dedup();
            all
        }))
    }

    /// Decompose (whenever (= 1 1) (do (if cond set!) ...)) into independent count/find calls.
    /// Returns true if the decomposition was applied.
    fn decompose_whenever_do(&mut self, cond: &Value, body_args: &[Value], trace_ids: &[String]) -> Result<bool, String> {
        let is_always_true = match cond {
            Value::List(lst) if lst.len() == 3 => {
                matches!(&lst[0], Value::Symbol(s) if s.name == "=")
                    && matches!(&lst[1], Value::Int(1))
                    && matches!(&lst[2], Value::Int(1))
            }
            _ => false,
        };
        if !is_always_true { return Ok(false); }

        let do_list = if body_args.len() == 1 {
            if let Value::List(lst) = &body_args[0] {
                if lst.len() >= 2 && matches!(&lst[0], Value::Symbol(s) if s.name == "do") {
                    &lst[1..]
                } else { return Ok(false); }
            } else { return Ok(false); }
        } else { return Ok(false); };

        let mut var_values: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        let mut any_decomposed = false;

        for expr in do_list {
            let if_list = match expr {
                Value::List(lst) if lst.len() >= 3 && matches!(&lst[0], Value::Symbol(s) if s.name == "if") => lst,
                _ => continue,
            };
            let if_cond = &if_list[1];
            let if_body = &if_list[2];

            let set_var = match if_body {
                Value::List(lst) if lst.len() == 3 && matches!(&lst[0], Value::Symbol(s) if s.name == "set!") => {
                    match &lst[1] {
                        Value::Symbol(s) => s.name.clone(),
                        _ => continue,
                    }
                }
                _ => continue,
            };

            // Try to get count for this condition — supports simple, &&, and ||
            let count = self.try_count_condition(if_cond, trace_ids)?;
            if let Some(c) = count {
                var_values.insert(set_var, c);
                any_decomposed = true;
            }
        }

        if !any_decomposed { return Ok(false); }

        for (var, val) in &var_values {
            let _ = self.env.set(var, Value::Int(*val));
        }
        Ok(true)
    }

    /// Try to count a condition using any available fast path.
    fn try_count_condition(&self, expr: &Value, trace_ids: &[String]) -> Result<Option<i64>, String> {
        // Try simple condition first
        if let Some((sig, target, is_not)) = self.parse_simple_condition(expr) {
            let cond: FindCondition = if is_not {
                if target <= 1 && target >= 0 { FindCondition::Neq(target as u8) }
                else { FindCondition::NeqI64(target) }
            } else {
                if target <= 1 && target >= 0 { FindCondition::Value(target as u8) }
                else { FindCondition::ValueI64(target) }
            };
            let mut total = 0usize;
            if let Ok(t) = self.traces.read() {
                for tid in trace_ids {
                    if let Some(tr) = t.get(tid) {
                        let sigs = tr.signals();
                        let resolved = resolve_signal_name(&sig, &sigs)
                            .unwrap_or_else(|| sig.clone());
                        if let Ok(idxs) = tr.find_indices(&resolved, cond.clone()) {
                            total += idxs.len();
                        }
                    }
                }
            }
            return Ok(Some(total as i64));
        }

        // Try &&/|| decomposition
        if let Some(result) = self.decompose_and_count(expr, trace_ids)? {
            return Ok(Some(result));
        }

        Ok(None)
    }

    /// Fast-path: evaluate a simple (= (get "sig") val) condition at given index
    #[allow(dead_code)]
    fn eval_simple_cond(&self, sig_name: &str, target: i64, idx: usize) -> bool {
        if let Ok(t) = self.traces.read() {
            for tid in t.trace_ids() {
                if let Some(tr) = t.get(&tid) {
                    if let Ok(sv) = tr.signal_value(sig_name, idx) {
                        let val = match sv {
                            crate::trace::ScalarValue::Bit(b) => {
                                if b == b'1' { 1i64 } else { 0i64 }
                            }
                            crate::trace::ScalarValue::Vector(v) => {
                                v.iter().fold(0i64, |acc, &b| (acc << 1) | if b == b'1' { 1 } else { 0 })
                            }
                            crate::trace::ScalarValue::Real(r) => r as i64,
                        };
                        return val == target;
                    }
                }
            }
        }
        false
    }

    fn eval_find(&mut self, args: &[Value]) -> Result<Value, String> {
        // (find cond) — find all indices where cond evaluates to true
        // Args are NOT pre-evaluated (special form)
        if args.len() < 1 {
            return Err("find expects at least 1 argument".to_string());
        }
        let max_results = if args.len() > 1 {
            match self.eval_value(args[1].clone())? {
                Value::Int(n) => n as usize,
                _ => return Err("find: second argument must be an integer limit".to_string()),
            }
        } else {
            usize::MAX
        };

        if let Some(traces) = self.traces.read().map(|g| g.trace_ids()).ok() {
            if traces.is_empty() {
                return Ok(Value::List(WList::new()));
            }

            let mut found: Vec<i64> = Vec::new();

            // Fast path: try simple condition (= (get "sig") val), (!= ...), (not (= ...))
            // Uses trace.find_indices() for a parallel scan.
            if let Some((sig_name, target, is_not)) = self.parse_simple_condition(&args[0]) {
                let cond: FindCondition = if is_not {
                    if target <= 1 && target >= 0 { FindCondition::Neq(target as u8) }
                    else { FindCondition::NeqI64(target) }
                } else {
                    if target <= 1 && target >= 0 { FindCondition::Value(target as u8) }
                    else { FindCondition::ValueI64(target) }
                };
                if let Ok(t) = self.traces.read() {
                    for tid in &traces {
                        if let Some(tr) = t.get(tid) {
                            // Resolve signal name via fuzzy matching (handles short names)
                            let resolved = resolve_signal_name(&sig_name, &tr.signals())
                                .unwrap_or_else(|| sig_name.clone());
                            if let Ok(idxs) = tr.find_indices(&resolved, cond.clone()) {
                                found.extend(idxs.into_iter().map(|i| i as i64));
                            }
                        }
                    }
                    found.sort();
                    found.dedup();
                    if found.len() > max_results { found.truncate(max_results); }
                    return Ok(Value::List(WList::from_vec(
                        found.into_iter().map(Value::Int).collect()
                    )));
                }
            }

            // Fast path 2: try decomposing (&& ...) or (|| ...)
            if let Some(indices) = self.decompose_and_find_indices(&args[0], &traces)? {
                found = indices.into_iter().map(|i| i as i64).collect();
                found.sort();
                found.dedup();
                if found.len() > max_results { found.truncate(max_results); }
                return Ok(Value::List(WList::from_vec(
                    found.into_iter().map(Value::Int).collect()
                )));
            }

            // Fallback: evaluate condition at each step
            let saved: Vec<(String, usize)> = {
                let t = self.traces.read().unwrap_or_else(|e| e.into_inner());
                traces.iter().filter_map(|tid| t.get(tid).map(|tr| (tid.clone(), tr.index()))).collect()
            };
            let mut ended = false;
            while !ended && found.len() < max_results {
                match self.eval_value(args[0].clone())? {
                    Value::Bool(true) => {
                        let t = self.traces.read().unwrap_or_else(|e| e.into_inner());
                        for tid in &traces {
                            if let Some(tr) = t.get(tid) {
                                found.push(tr.index() as i64);
                            }
                        }
                    }
                    _ => {}
                }
                let mut any_ended = true;
                if let Ok(mut t) = self.traces.write() {
                    for tid in &traces {
                        if let Some(tr) = t.get_mut(tid) {
                            if tr.step(1).is_ok() { any_ended = false; }
                        }
                    }
                }
                ended = any_ended;
            }

            // Restore
            if let Ok(mut t) = self.traces.write() {
                for (tid, idx) in &saved {
                    let _ = t.set_index(tid, *idx);
                }
            }

            found.sort();
            found.dedup();
            if found.len() > max_results { found.truncate(max_results); }
            return Ok(Value::List(WList::from_vec(found.into_iter().map(Value::Int).collect())));
        }
        Ok(Value::List(WList::new()))
    }

    fn eval_find_g(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() < 1 {
            return Err("find/g expects at least 1 argument".to_string());
        }
        let saved: Vec<(String, usize)>;
        let traces_ids: Vec<String>;
        if let Ok(t) = self.traces.read() {
            traces_ids = t.trace_ids();
            saved = traces_ids.iter().filter_map(|tid| t.get(tid).map(|tr| (tid.clone(), tr.index()))).collect();
        } else {
            return Ok(Value::List(WList::new()));
        }

        // Fast path: try simple condition (= (get "sig") val), (!= ...), (not (= ...))
        if let Some((sig_name, target, is_not)) = self.parse_simple_condition(&args[0]) {
            let cond: FindCondition = if is_not {
                if target <= 1 && target >= 0 { FindCondition::Neq(target as u8) }
                else { FindCondition::NeqI64(target) }
            } else {
                if target <= 1 && target >= 0 { FindCondition::Value(target as u8) }
                else { FindCondition::ValueI64(target) }
            };
            let mut found = Vec::new();
            if let Ok(t) = self.traces.read() {
                for tid in &traces_ids {
                    if let Some(tr) = t.get(tid) {
                        if let Ok(idxs) = tr.find_indices(&resolve_signal_name(&sig_name, &tr.signals()).unwrap_or_else(|| sig_name.clone()), cond.clone()) {
                            for &idx in &idxs {
                                found.push(Value::Int(idx as i64));
                            }
                        }
                    }
                }
            }
            found.sort_by_key(|v| if let Value::Int(i) = v { *i } else { 0 });
            found.dedup();
            if let Ok(mut t) = self.traces.write() {
                for (tid, idx) in &saved {
                    let _ = t.set_index(tid, *idx);
                }
            }
            return Ok(Value::List(WList::from_vec(found)));
        }

        // Slow path: step-by-step iteration
        let mut found = Vec::new();
        let mut ended = false;
        while !ended {
            match self.eval_value(args[0].clone())? {
                Value::Bool(true) => {
                    if let Ok(t) = self.traces.read() {
                        let indices: Vec<i64> = traces_ids.iter()
                            .filter_map(|tid| t.get(tid).map(|tr| tr.index() as i64))
                            .collect();
                        found.push(if indices.len() == 1 {
                            Value::Int(indices[0])
                        } else {
                            Value::List(WList::from_vec(indices.into_iter().map(Value::Int).collect()))
                        });
                    }
                }
                _ => {}
            }
            let mut any_ended = true;
            if let Ok(mut t) = self.traces.write() {
                for tid in &traces_ids {
                    if let Some(tr) = t.get_mut(tid) {
                        if tr.step(1).is_ok() { any_ended = false; }
                    }
                }
            }
            ended = any_ended;
        }

        if let Ok(mut t) = self.traces.write() {
            for (tid, idx) in &saved {
                let _ = t.set_index(tid, *idx);
            }
        }
        Ok(Value::List(WList::from_vec(found)))
    }

    fn eval_count(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() < 1 {
            return Err("count expects at least 1 argument".to_string());
        }
        let saved: Vec<(String, usize)>;
        let traces_ids: Vec<String>;
        if let Ok(t) = self.traces.read() {
            traces_ids = t.trace_ids();
            saved = traces_ids.iter().filter_map(|tid| t.get(tid).map(|tr| (tid.clone(), tr.index()))).collect();
        } else {
            return Ok(Value::Int(0));
        }

        // Batch mode: multiple conditions → single scan for all (including &&)
        if args.len() > 1 {
            let mut batch_entries: Vec<BatchEntry> = Vec::new();
            for arg in args {
                // Resolve variable references in (get <symbol>) → (get "resolved_string")
                let resolved_arg = self.resolve_get_symbols(arg);
                if let Some((sig, target, is_not)) = self.parse_simple_condition(&resolved_arg) {
                    batch_entries.push(BatchEntry::Simple(sig, Self::build_cond(target, is_not)));
                } else if let Some(subs) = self.decompose_and_condition(&resolved_arg) {
                    // (&& cond1 cond2 ...) → build And entry
                    if subs.len() >= 2 {
                        batch_entries.push(BatchEntry::And(subs));
                    }
                }
            }
            if batch_entries.len() >= 2 {
                let mut result_counts: Vec<Value> = Vec::new();
                {
                    let t = self.traces.read().unwrap_or_else(|e| e.into_inner());
                    for tid in &traces_ids {
                        if let Some(tr) = t.get(tid) {
                            let sigs = tr.signals();
                            // Resolve signal names in all entries
                            let resolved_entries: Vec<BatchEntry> = batch_entries.iter()
                                .map(|entry| match entry {
                                    BatchEntry::Simple(name, cond) => {
                                        let r = resolve_signal_name(name, &sigs).unwrap_or_else(|| name.clone());
                                        BatchEntry::Simple(r, cond.clone())
                                    }
                                    BatchEntry::And(subs) => {
                                        let resolved: Vec<(String, FindCondition)> = subs.iter()
                                            .map(|(name, cond)| {
                                                let r = resolve_signal_name(name, &sigs).unwrap_or_else(|| name.clone());
                                                (r, cond.clone())
                                            })
                                            .collect();
                                        BatchEntry::And(resolved)
                                    }
                                })
                                .collect();
                            if let Ok(results) = tr.find_indices_batch(&resolved_entries) {
                                for (_, indices) in results {
                                    result_counts.push(Value::Int(indices.len() as i64));
                                }
                                // Read guard dropped, restore indices
                                if let Ok(mut tc) = self.traces.write() {
                                    for (tid, idx) in &saved {
                                        let _ = tc.set_index(tid, *idx);
                                    }
                                }
                                return Ok(Value::List(WList::from_vec(result_counts)));
                            }
                        }
                    }
                }
            }
        }

        // Fast path: try simple condition (= (get "sig") val), (!= ...), (not (= ...))
        if let Some((sig_name, target, is_not)) = self.parse_simple_condition(&args[0]) {
            let cond: FindCondition = if is_not {
                if target <= 1 && target >= 0 { FindCondition::Neq(target as u8) }
                else { FindCondition::NeqI64(target) }
            } else {
                if target <= 1 && target >= 0 { FindCondition::Value(target as u8) }
                else { FindCondition::ValueI64(target) }
            };
            let total: usize = {
                let t = self.traces.read().unwrap_or_else(|e| e.into_inner());
                let mut sum = 0usize;
                for tid in &traces_ids {
                    if let Some(tr) = t.get(tid) {
                        let sigs = tr.signals();
                        let resolved = resolve_signal_name(&sig_name, &sigs)
                            .unwrap_or_else(|| sig_name.clone());
                        if let Ok(idxs) = tr.find_indices(&resolved, cond.clone()) {
                            sum += idxs.len();
                        }
                    }
                }
                sum
            };
            return Ok(Value::Int(total as i64));
        }

        // Fast path 2: try decomposing (&& cond1 cond2) or (|| cond1 cond2)
        if let Some(result) = self.decompose_and_count(&args[0], &traces_ids)? {
            return Ok(Value::Int(result));
        }

        // Fallback: evaluate condition at each step
        let mut count: i64 = 0;
        let mut ended = false;
        while !ended {
            if self.eval_value(args[0].clone())?.is_truthy() {
                count += 1;
            }
            let mut any_ended = true;
            if let Ok(mut t) = self.traces.write() {
                for tid in &traces_ids {
                    if let Some(tr) = t.get_mut(tid) {
                        if tr.step(1).is_ok() { any_ended = false; }
                    }
                }
            }
            ended = any_ended;
        }

        if let Ok(mut t) = self.traces.write() {
            for (tid, idx) in &saved {
                let _ = t.set_index(tid, *idx);
            }
        }
        Ok(Value::Int(count))
    }

    fn eval_whenever(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("whenever expects at least 2 arguments".to_string());
        }
        let body_args: Vec<Value> = args[1..].to_vec();

        let saved: Vec<(String, usize)>;
        let traces_ids: Vec<String>;
        if let Ok(t) = self.traces.read() {
            traces_ids = t.trace_ids();
            saved = traces_ids.iter().filter_map(|tid| t.get(tid).map(|tr| (tid.clone(), tr.index()))).collect();
        } else {
            return Ok(Value::Nil);
        }

        // Fast path: try simple condition (= (get "sig") val), (!= ...), (not (= ...))
        // Uses trace.find_indices() for a parallel scan.
        if let Some((sig_name, target, is_not)) = self.parse_simple_condition(&args[0]) {
            let cond: FindCondition = if is_not {
                if target <= 1 && target >= 0 { FindCondition::Neq(target as u8) }
                else { FindCondition::NeqI64(target) }
            } else {
                if target <= 1 && target >= 0 { FindCondition::Value(target as u8) }
                else { FindCondition::ValueI64(target) }
            };
            let all_indices: Vec<usize> = {
                let t = self.traces.read().unwrap_or_else(|e| e.into_inner());
                traces_ids.iter().filter_map(|tid| {
                    t.get(tid).and_then(|tr| {
                        let resolved = resolve_signal_name(&sig_name, &tr.signals())
                            .unwrap_or_else(|| sig_name.clone());
                        tr.find_indices(&resolved, cond.clone()).ok()
                    })
                }).flatten().collect()
            };
            let mut result = Value::Nil;
            for &idx in &all_indices {
                if let Ok(mut t) = self.traces.write() {
                    for tid in &traces_ids {
                        let _ = t.set_index(tid, idx);
                    }
                }
                for b in &body_args {
                    result = self.eval_value(b.clone())?;
                }
            }
            // Restore indices
            for (tid, idx) in &saved {
                if let Ok(mut t) = self.traces.write() {
                    let _ = t.set_index(tid, *idx);
                }
            }
            return Ok(result);
        }

        // Fast path 2: try decomposing (whenever (= 1 1) (do (if cond set!) ...))
        if self.decompose_whenever_do(&args[0], &body_args, &traces_ids)? {
            // Restore indices
            for (tid, idx) in &saved {
                if let Ok(mut t) = self.traces.write() {
                    let _ = t.set_index(tid, *idx);
                }
            }
            return Ok(Value::Nil);
        }

        // Reset all traces to start (fallback path)
        if let Ok(mut t) = self.traces.write() {
            for tid in &traces_ids {
                let _ = t.set_index(tid, 0);
            }
        }

        let mut result = Value::Nil;
        let mut ended = false;
        while !ended {
            let cond_true = self.eval_value(args[0].clone())?.is_truthy();
            if cond_true {
                for b in &body_args {
                    result = self.eval_value(b.clone())?;
                }
            }
            let mut any_ended = true;
            if let Ok(mut t) = self.traces.write() {
                for tid in &traces_ids {
                    if let Some(tr) = t.get_mut(tid) {
                        if tr.step(1).is_ok() { any_ended = false; }
                    }
                }
            }
            ended = any_ended;
        }

        if let Ok(mut t) = self.traces.write() {
            for (tid, idx) in &saved {
                let _ = t.set_index(tid, *idx);
            }
        }
        Ok(result)
    }

    fn eval_quote(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 1 {
            return Err(format!("quote expects 1 argument, got {}", args.len()));
        }
        Ok(args[0].clone())
    }

    // fn special form: (fn (args+) body+)
    // No pre-evaluation — argument list and body expressions are passed as-is
    fn eval_fn_special(&mut self, args: &[Value]) -> Result<Value, String> {
        self.eval_dispatch(Operator::Fn, args)
    }

    // defun macro: (defun name (args...) body...) → (define name (fn (args...) body...))
    //            or (defun name singe-symbol body...) → variadic
    fn eval_defun_macro(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() < 3 {
            return Err("defun expects at least name, params, and body".to_string());
        }
        let name = match &args[0] {
            Value::Symbol(s) => s.name.clone(),
            _ => return Err("defun: first argument must be a symbol".to_string()),
        };
        // args[1] is either a list of parameter symbols, or a single symbol (variadic)
        let body_expr = if args.len() > 3 {
            // Multiple body expressions → wrap in do
            let mut do_args = vec![Value::Symbol(Symbol::new("do"))];
            do_args.extend_from_slice(&args[2..]);
            Value::List(WList::from_vec(do_args))
        } else {
            args[2].clone()
        };
        let fn_expr = Value::List(WList::from_vec(vec![
            Value::Symbol(Symbol::new("fn")),
            args[1].clone(),
            body_expr,
        ]));
        let define_expr = Value::List(WList::from_vec(vec![
            Value::Symbol(Symbol::new("define")),
            Value::Symbol(Symbol::new(&name)),
            fn_expr,
        ]));
        self.eval_value(define_expr)
    }

    // defunm macro: (defunm name (args...) body...) → (defmacro name (args...) body...)
    fn eval_defunm_macro(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("defunm expects at least name and body".to_string());
        }
        let name = match &args[0] {
            Value::Symbol(s) => s.name.clone(),
            _ => return Err("defunm: first argument must be a symbol".to_string()),
        };
        let defmacro_expr = Value::List(WList::from_vec(vec![
            Value::Symbol(Symbol::new("defmacro")),
            Value::Symbol(Symbol::new(&name)),
            args[1].clone(),
            Value::List(WList::from_vec(args[2..].to_vec())),
        ]));
        self.eval_value(defmacro_expr)
    }

    // set! macro: (set! x val) -> (set x val)
    fn eval_set_macro(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("set! expects 2 arguments".to_string());
        }
        let set_expr = Value::List(WList::from_vec(vec![
            Value::Symbol(Symbol::new("set")),
            args[0].clone(),
            args[1].clone(),
        ]));
        self.eval_value(set_expr)
    }

    // for/list macro: (for/list (x lst) body...) -> map or similar iteration
    fn eval_for_list_macro(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("for/list expects at least binding and body".to_string());
        }
        // (for/list (x lst) body...) -> (map (fn (x) body...) lst)
        let binding = match &args[0] {
            Value::List(lst) if lst.len() == 2 => lst.clone(),
            _ => return Err("for/list: first argument must be (var list)".to_string()),
        };
        let var = binding.0[0].clone();
        let lst_expr = binding.0[1].clone();
        let body = Value::List(WList::from_vec(args[1..].to_vec()));
        let fn_expr = Value::List(WList::from_vec(vec![
            Value::Symbol(Symbol::new("fn")),
            Value::List(WList::from_vec(vec![var])),
            body,
        ]));
        let map_expr = Value::List(WList::from_vec(vec![
            Value::Symbol(Symbol::new("map")),
            fn_expr,
            lst_expr,
        ]));
        self.eval_value(map_expr)
    }

    // timeframe special form: (timeframe body...) — save/restore INDEX
    fn eval_timeframe(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.is_empty() {
            return Err("timeframe expects at least 1 argument".to_string());
        }

        let tids: Vec<_> = {
            let traces = self.traces.read().unwrap_or_else(|e| e.into_inner());
            traces.trace_ids()
        };
        let prev_idx_values: Vec<_> = {
            let traces = self.traces.read().unwrap_or_else(|e| e.into_inner());
            tids.iter()
                .map(|tid| traces.get(tid).map(|t| t.index()).unwrap_or(0))
                .collect()
        };

        let mut result = Value::Nil;
        for arg in args {
            result = self.eval_value(arg.clone())?;
        }

        {
            let mut traces = self.traces.write().unwrap_or_else(|e| e.into_inner());
            for (tid, &idx) in tids.iter().zip(prev_idx_values.iter()) {
                let _ = traces.set_index(tid, idx);
            }
        }

        Ok(result)
    }

    // alias special form: (alias name target) — first arg is literal symbol
    fn eval_alias(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("alias expects 2 arguments".to_string());
        }
        let alias_name = match &args[0] {
            Value::Symbol(s) => s.name.clone(),
            _ => return Err("alias: first argument must be a symbol".to_string()),
        };
        let target_name = match &args[1] {
            Value::Symbol(s) => s.name.clone(),
            _ => return Err("alias: second argument must be a symbol".to_string()),
        };
        self.env.add_alias(&alias_name, &target_name);
        Ok(Value::Nil)
    }

    // unalias special form
    fn eval_unalias(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("unalias expects 1 argument".to_string());
        }
        let name = match &args[0] {
            Value::Symbol(s) => s.name.clone(),
            _ => return Err("unalias: argument must be a symbol".to_string()),
        };
        if self.env.remove_alias(&name) {
            Ok(Value::Nil)
        } else {
            Err(format!("Alias '{}' not found", name))
        }
    }

    pub fn define(&mut self, name: &str, value: Value) {
        self.env.define(name, value);
    }

    pub fn load_trace(&mut self, path: &str, id: &str) -> Result<(), String> {
        use std::path::Path;
        self.traces.write().unwrap_or_else(|e| e.into_inner()).load(Path::new(path), id.to_string())?;
        self.env.define(id, Value::String(path.to_string()));
        Ok(())
    }

    pub fn env_mut(&mut self) -> &mut Environment {
        &mut self.env
    }

    pub fn get_traces(&self) -> Option<SharedTraceContainer> {
        self.env.get_traces()
    }

    pub fn traces_mut(&mut self) -> &mut SharedTraceContainer {
        &mut self.traces
    }

    pub fn eval_value_public(&mut self, value: Value) -> Result<Value, String> {
        self.eval_value_internal(value)
    }

    fn eval_value_internal(&mut self, value: Value) -> Result<Value, String> {
        match value {
            Value::Symbol(s) => self.eval_symbol(s),
            Value::List(lst) => self.eval_list(lst),
            _ => Ok(value),
        }
    }

    fn eval_dispatch(&mut self, op: Operator, args: &[Value]) -> Result<Value, String> {
        if let Some(err) = SemanticChecker::check_operator_args(op, args) {
            return Err(err.message());
        }

        if args.len() == 2 {
            if let Some(err) = SemanticChecker::check_binary_args(op, &args[0], &args[1]) {
                return Err(err.message());
            }
        }

        let func_opt = {
            self.disp.operators.get(&op).copied()
        };

        match func_opt {
            Some(func) => {
                let env_ptr: *mut Environment = &mut self.env;
                let eval_ptr: *mut Evaluator = self;
                unsafe { func(args, &mut *env_ptr, &mut *eval_ptr) }
            }
            None => Err(format!("Unknown operator: {:?}", op)),
        }
    }
}

fn fuzzy_match_signal<'a>(name: &str, signals: &'a [String]) -> (Option<&'a String>, Vec<&'a String>) {
    let dot_name = format!(".{}", name);
    // 1. Exact or suffix match
    let suffix: Vec<&'a String> = signals.iter().filter(|s| s.as_str() == name || s.ends_with(&dot_name)).collect();
    if suffix.len() == 1 { return (Some(suffix[0]), vec![]); }
    if suffix.len() > 1 { return (Some(suffix[0]), suffix); }

    // 2. Last component match for short names
    if name.len() <= 8 || !name.contains('.') {
        let last_comp: Vec<&'a String> = signals.iter()
            .filter(|s| s.rsplitn(2, '.').next().unwrap_or("") == name)
            .collect();
        if last_comp.len() == 1 { return (Some(last_comp[0]), vec![]); }
        if last_comp.len() > 1 { return (Some(last_comp[0]), last_comp); }
    }

    // 3. Substring match
    let sub: Vec<&'a String> = signals.iter().filter(|s| s.contains(name)).collect();
    if sub.len() == 1 { return (Some(sub[0]), vec![]); }
    if sub.len() > 1 { return (Some(sub[0]), sub); }

    (None, vec![])
}

/// Helper to extract a name from either a Symbol or String value
fn scope_extract_name(v: &Value) -> Result<String, String> {
    match v {
        Value::Symbol(s) => Ok(s.name.clone()),
        Value::String(s) => Ok(s.clone()),
        _ => Err("Expected symbol or string".to_string()),
    }
}

impl Default for Evaluator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eval_int() {
        let mut eval = Evaluator::new();
        let result = eval.eval("42");
        assert_eq!(result.unwrap(), Value::Int(42));
    }

    #[test]
    fn test_eval_add() {
        let mut eval = Evaluator::new();
        let result = eval.eval("(+ 1 2)");
        assert_eq!(result.unwrap(), Value::Int(3));
    }

    #[test]
    fn test_eval_sub() {
        let mut eval = Evaluator::new();
        let result = eval.eval("(- 10 3)");
        assert_eq!(result.unwrap(), Value::Int(7));
    }

    #[test]
    fn test_eval_mul() {
        let mut eval = Evaluator::new();
        let result = eval.eval("(* 3 4)");
        assert_eq!(result.unwrap(), Value::Int(12));
    }

    #[test]
    fn test_eval_div() {
        let mut eval = Evaluator::new();
        let result = eval.eval("(/ 10 2)");
        assert_eq!(result.unwrap(), Value::Float(5.0));
    }

    #[test]
    fn test_eval_nested() {
        let mut eval = Evaluator::new();
        let result = eval.eval("(+ (* 2 3) 4)");
        assert_eq!(result.unwrap(), Value::Int(10));
    }

    #[test]
    fn test_eval_define() {
        let mut eval = Evaluator::new();
        let result = eval.eval("(define x 42)");
        assert_eq!(result.unwrap(), Value::Int(42));
        let result = eval.eval("x");
        assert_eq!(result.unwrap(), Value::Int(42));
    }

    #[test]
    fn test_quasiquote_simple() {
        let mut eval = Evaluator::new();
        let result = eval.eval("(define x 5)");
        assert_eq!(result.unwrap(), Value::Int(5));
        let result = eval.eval("`(+ 1 ,x)");
        let list = result.unwrap();
        assert_eq!(list, Value::List(WList::from_vec(vec![
            Value::Symbol(Symbol::new("+")),
            Value::Int(1),
            Value::Int(5),
        ])));
        let add_result = eval.eval("(+ 1 5)");
        assert_eq!(add_result.unwrap(), Value::Int(6));
    }
}

/// Resolve a signal name against a trace's signal list using the same fuzzy matching
/// as `op_get`. Returns the full signal name if found.
pub fn resolve_signal_name(name: &str, sigs: &[String]) -> Option<String> {
    // 1. Exact match
    if let Some(s) = sigs.iter().find(|s| *s == name) {
        return Some(s.clone());
    }
    // 2. Suffix match (short names, or names without dot)
    if name.len() <= 8 || !name.contains('.') {
        if let Some(s) = sigs.iter().find(|s| {
            let last = s.rsplitn(2, '.').next().unwrap_or("");
            last == name
        }) {
            return Some(s.clone());
        }
    }
    // 3. Substring match
    sigs.iter().find(|s| s.contains(name)).cloned()
}