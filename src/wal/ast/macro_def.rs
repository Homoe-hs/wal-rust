//! Macro AST node

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;
use super::{Symbol, Value};
use crate::wal::eval::Environment;

#[derive(Debug, Clone)]
pub struct Macro {
    pub env: Rc<RefCell<Environment>>,
    pub args: Vec<Symbol>,
    pub body: Box<Value>,
    pub name: Option<String>,
    pub variadic: bool,
}

impl Macro {
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
}

impl fmt::Display for Macro {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name {
            Some(name) => write!(f, "<macro {}>", name),
            None => write!(f, "<macro>"),
        }
    }
}

impl PartialEq for Macro {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.args == other.args
    }
}
