//! WList AST node (list wrapper)

use std::fmt;
use std::ops::Deref;
use super::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct WList(pub Vec<Value>);

impl WList {
    pub fn new() -> Self {
        WList(Vec::new())
    }

    pub fn from_vec(v: Vec<Value>) -> Self {
        WList(v)
    }

    pub fn push(&mut self, v: Value) {
        self.0.push(v);
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, i: usize) -> Option<&Value> {
        self.0.get(i)
    }

    pub fn first(&self) -> Option<&Value> {
        self.0.first()
    }

    pub fn rest(&self) -> Vec<Value> {
        self.0[1..].to_vec()
    }
}

impl Default for WList {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for WList {
    type Target = Vec<Value>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl fmt::Display for WList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({})", self.0.iter().map(|v| format!("{}", v)).collect::<Vec<_>>().join(" "))
    }
}

impl From<Vec<Value>> for WList {
    fn from(v: Vec<Value>) -> Self {
        WList(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wal::ast::Value;

    #[test]
    fn test_wlist_basic() {
        let mut list = WList::new();
        list.push(Value::Int(1));
        list.push(Value::Int(2));
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_wlist_first_rest() {
        let list = WList::from_vec(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        assert_eq!(list.first(), Some(&Value::Int(1)));
        assert_eq!(list.rest(), vec![Value::Int(2), Value::Int(3)]);
    }
}