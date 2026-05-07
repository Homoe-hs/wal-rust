//! Basic Evaluator for WAL

use crate::wal::ast::{Value, Symbol, WList, Closure, Operator};
use crate::wal::eval::{Environment, Dispatcher, SemanticChecker};
use crate::wal::builtins;
use crate::wal::builtins::special::quasiquote_eval;
use crate::trace::{TraceContainer, SharedTraceContainer};
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
                    if op == Operator::Define {
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
                    } else if op == Operator::InGroup {
                        return self.eval_in_group(&rest);
                    } else if op == Operator::InGroups {
                        return self.eval_in_groups(&rest);
                    } else if op == Operator::Let {
                        return self.eval_let(&rest);
                    } else if op == Operator::Quasiquote {
                        return self.eval_quasiquote(&rest);
                    } else if op == Operator::Quote {
                        return self.eval_quote(&rest);
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

    fn eval_define(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 2 {
            return Err(format!("define expects 2 arguments, got {}", args.len()));
        }

        match &args[0] {
            Value::Symbol(s) => {
                let name = s.name.clone();
                let value = match &args[1] {
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
                                self.env.define(name.clone(), Value::Closure(closure));
                                return Ok(Value::Nil);
                            }
                        }
                        self.eval_value(args[1].clone())?
                    }
                    _ => self.eval_value(args[1].clone())?,
                };
                self.env.define(name, value);
                Ok(Value::Nil)
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

                self.env.define(func_name, Value::Closure(closure));
                Ok(Value::Nil)
            }
            _ => Err("define expects symbol or function definition".to_string()),
        }
    }

    fn eval_if(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() < 3 {
            return Err(format!("if expects at least 3 arguments, got {}", args.len()));
        }
        let cond = self.eval_value(args[0].clone())?;
        if cond.is_truthy() {
            self.eval_value(args[1].clone())
        } else {
            self.eval_value(args[2].clone())
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
        assert_eq!(result.unwrap(), Value::Nil);
        let result = eval.eval("x");
        assert_eq!(result.unwrap(), Value::Int(42));
    }

    #[test]
    fn test_quasiquote_simple() {
        let mut eval = Evaluator::new();
        let result = eval.eval("(define x 5)");
        assert_eq!(result.unwrap(), Value::Nil);
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