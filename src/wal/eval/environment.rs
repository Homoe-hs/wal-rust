//! Environment for variable scoping

use std::collections::HashMap;
use std::cell::RefCell;
use std::rc::Rc;
use super::super::ast::Value;
use crate::trace::SharedTraceContainer;

#[derive(Debug)]
pub struct Environment {
    parent: Option<Rc<RefCell<Environment>>>,
    bindings: HashMap<String, Value>,
    scope: String,
    group: String,
    traces: Option<SharedTraceContainer>,
}

impl Environment {
    pub fn new() -> Self {
        Self {
            parent: None,
            bindings: HashMap::new(),
            scope: String::new(),
            group: String::new(),
            traces: None,
        }
    }

    pub fn with_parent(parent: Rc<RefCell<Environment>>) -> Self {
        Self {
            parent: Some(parent),
            bindings: HashMap::new(),
            scope: String::new(),
            group: String::new(),
            traces: None,
        }
    }

    pub fn set_parent(&mut self, parent: Option<Rc<RefCell<Environment>>>) {
        self.parent = parent;
    }

    pub fn get_parent(&self) -> Option<Rc<RefCell<Environment>>> {
        self.parent.clone()
    }

    pub fn set_traces(&mut self, traces: SharedTraceContainer) {
        self.traces = Some(traces);
    }

    pub fn get_traces(&self) -> Option<SharedTraceContainer> {
        self.traces.clone().or_else(|| {
            self.parent.as_ref().and_then(|p| p.borrow().get_traces())
        })
    }

    pub fn child(&self) -> Environment {
        Environment::with_parent(Rc::new(RefCell::new(self.clone())))
    }

    pub fn define(&mut self, name: impl Into<String>, value: Value) {
        self.bindings.insert(name.into(), value);
    }

    pub fn lookup(&self, name: &str) -> Option<Value> {
        self.bindings.get(name).cloned().or_else(|| {
            self.parent.as_ref().and_then(|p| p.borrow().lookup(name))
        })
    }

    pub fn lookup_global(&self, name: &str) -> Option<Value> {
        self.bindings.get(name).cloned()
    }

    pub fn set(&mut self, name: &str, value: Value) -> Result<(), String> {
        if self.bindings.contains_key(name) {
            self.bindings.insert(name.to_string(), value);
            Ok(())
        } else if let Some(ref parent) = self.parent {
            parent.borrow_mut().set(name, value)
        } else {
            Err(format!("Undefined variable: {}", name))
        }
    }

    pub fn get_scope(&self) -> &str {
        &self.scope
    }

    pub fn set_scope(&mut self, scope: impl Into<String>) {
        self.scope = scope.into();
    }

    pub fn get_group(&self) -> &str {
        &self.group
    }

    pub fn set_group(&mut self, group: impl Into<String>) {
        self.group = group.into();
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.bindings.keys()
    }
}

impl Default for Environment {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for Environment {
    fn clone(&self) -> Self {
        Self {
            parent: self.parent.clone(),
            bindings: self.bindings.clone(),
            scope: self.scope.clone(),
            group: self.group.clone(),
            traces: self.traces.clone(),
        }
    }
}

impl Environment {
    pub fn extend(&self) -> Environment {
        self.child()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_environment_basic() {
        let mut env = Environment::new();
        env.define("x", Value::Int(42));
        assert_eq!(env.lookup("x"), Some(Value::Int(42)));
    }

    #[test]
    fn test_environment_parent() {
        let mut parent = Environment::new();
        parent.define("x", Value::Int(10));

        let child = Environment::with_parent(Rc::new(RefCell::new(parent)));
        assert_eq!(child.lookup("x"), Some(Value::Int(10)));
    }

    #[test]
    fn test_environment_override() {
        let mut env = Environment::new();
        env.define("x", Value::Int(1));
        env.define("x", Value::Int(2));
        assert_eq!(env.lookup("x"), Some(Value::Int(2)));
    }

    #[test]
    fn test_environment_set_undefined() {
        let mut env = Environment::new();
        let result = env.set("x", Value::Int(1));
        assert!(result.is_err());
    }

    #[test]
    fn test_environment_set_in_parent() {
        let mut parent = Environment::new();
        parent.define("x", Value::Int(10));

        let parent_rc = Rc::new(RefCell::new(parent));
        let mut child = Environment::with_parent(parent_rc.clone());

        // set x in child should modify parent's binding
        child.set("x", Value::Int(20)).unwrap();
        assert_eq!(parent_rc.borrow().lookup("x"), Some(Value::Int(20)));
    }
}
