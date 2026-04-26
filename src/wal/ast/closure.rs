//! Closure AST node

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;
use super::{Symbol, Value};
use crate::wal::eval::Environment;

#[derive(Debug, Clone)]
pub struct Closure {
    pub env: Rc<RefCell<Environment>>,
    pub args: Vec<Symbol>,
    pub body: Box<Value>,
    pub name: Option<String>,
    pub variadic: bool,
}

impl Closure {
    pub fn new(
        env: Rc<RefCell<Environment>>,
        args: Vec<Symbol>,
        body: Value,
    ) -> Self {
        Self {
            env,
            args,
            body: Box::new(body),
            name: None,
            variadic: false,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn arity(&self) -> usize {
        self.args.len()
    }
}

impl fmt::Display for Closure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name {
            Some(name) => write!(f, "<closure {}>", name),
            None => write!(f, "<closure>"),
        }
    }
}

impl PartialEq for Closure {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.args == other.args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_closure_basic() {
        let env = Rc::new(RefCell::new(Environment::new()));
        let closure = Closure::new(env, vec![Symbol::new("x")], Value::Int(42));
        assert_eq!(closure.arity(), 1);
    }

    #[test]
    fn test_closure_with_name() {
        let env = Rc::new(RefCell::new(Environment::new()));
        let closure = Closure::new(env, vec![Symbol::new("x")], Value::Int(42))
            .with_name("add-one");
        assert_eq!(closure.name(), Some("add-one"));
    }
}
