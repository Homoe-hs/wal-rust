//! Dispatcher for operator evaluation

use super::super::ast::{Value, Operator};
use super::environment::Environment;
use super::evaluator::Evaluator;
use std::collections::HashMap;
use crate::trace::SharedTraceContainer;

pub type BuiltinFn = fn(&[Value], &mut Environment, &mut Evaluator) -> Result<Value, String>;

#[derive(Debug)]
pub struct Dispatcher {
    pub operators: HashMap<Operator, BuiltinFn>,
    traces: Option<SharedTraceContainer>,
}

impl Dispatcher {
    pub fn new() -> Self {
        Self {
            operators: HashMap::new(),
            traces: None,
        }
    }

    pub fn set_traces(&mut self, traces: SharedTraceContainer) {
        self.traces = Some(traces);
    }

    pub fn get_traces(&self) -> Option<&SharedTraceContainer> {
        self.traces.as_ref()
    }

    pub fn register(&mut self, op: Operator, f: BuiltinFn) {
        self.operators.insert(op, f);
    }

    pub fn get(&self, op: Operator) -> Option<BuiltinFn> {
        self.operators.get(&op).copied()
    }

    pub fn dispatch(&self, op: Operator, args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
        match self.operators.get(&op) {
            Some(f) => f(args, env, eval),
            None => Err(format!("Unknown operator: {:?}", op)),
        }
    }
}

impl Default for Dispatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dispatcher_basic() {
        let mut disp = Dispatcher::new();

        let add_fn: BuiltinFn = |args, _env, _eval| {
            if args.len() != 2 {
                return Err("add expects 2 args".to_string());
            }
            match (&args[0], &args[1]) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
                _ => Err("add expects integers".to_string()),
            }
        };

        disp.register(Operator::Add, add_fn);

        let mut env = Environment::new();
        let mut eval = Evaluator::new();
        let result = disp.dispatch(Operator::Add, &[Value::Int(1), Value::Int(2)], &mut env, &mut eval);
        assert_eq!(result, Ok(Value::Int(3)));
    }
}