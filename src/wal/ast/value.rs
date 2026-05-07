//! Value types for WAL

use std::fmt;
use super::{Symbol, WList, Closure};
use super::macro_def::Macro;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Symbol(Symbol),
    List(WList),
    Closure(Closure),
    Macro(Macro),
    Unquote(Box<Value>),
    UnquoteSplice(Box<Value>),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => write!(f, "nil"),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Int(i) => write!(f, "{}", i),
            Value::Float(fl) => write!(f, "{}", fl),
            Value::String(s) => write!(f, "\"{}\"", s),
            Value::Symbol(s) => write!(f, "{}", s),
            Value::List(l) => write!(f, "{}", l),
            Value::Closure(c) => write!(f, "<closure {}>", c.name().unwrap_or("<anonymous>")),
            Value::Macro(m) => write!(f, "<macro {}>", m.name().unwrap_or("<anonymous>")),
            Value::Unquote(v) => write!(f, ",{}", v),
            Value::UnquoteSplice(v) => write!(f, ",@{}", v),
        }
    }
}

impl Value {
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Nil => false,
            Value::Bool(b) => *b,
            Value::List(lst) => !lst.is_empty(),
            _ => true,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Nil => "nil",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::String(_) => "string",
            Value::Symbol(_) => "symbol",
            Value::List(_) => "list",
            Value::Closure(_) => "closure",
            Value::Macro(_) => "macro",
            Value::Unquote(_) | Value::UnquoteSplice(_) => "unquote",
        }
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::Int(i)
    }
}

impl From<f64> for Value {
    fn from(f: f64) -> Self {
        Value::Float(f)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_string())
    }
}

impl From<Symbol> for Value {
    fn from(s: Symbol) -> Self {
        Value::Symbol(s)
    }
}

impl From<WList> for Value {
    fn from(l: WList) -> Self {
        Value::List(l)
    }
}

impl From<Vec<Value>> for Value {
    fn from(v: Vec<Value>) -> Self {
        Value::List(WList::from_vec(v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_value_display() {
        assert_eq!(format!("{}", Value::Int(42)), "42");
        assert_eq!(format!("{}", Value::String("hello".to_string())), "\"hello\"");
    }

    #[test]
    fn test_truthiness() {
        assert!(!Value::Nil.is_truthy());
        assert!(Value::Bool(true).is_truthy());
        assert!(!Value::Bool(false).is_truthy());
        assert!(Value::Int(0).is_truthy());  // 0 is truthy in Lisp/WAL
        assert!(!Value::List(WList::new()).is_truthy());  // empty list is falsy
    }

    #[test]
    fn test_type_name() {
        assert_eq!(Value::Int(1).type_name(), "int");
        assert_eq!(Value::String("".to_string()).type_name(), "string");
        assert_eq!(Value::Nil.type_name(), "nil");
    }
}