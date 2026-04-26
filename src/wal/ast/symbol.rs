//! Symbol AST node

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Symbol {
    pub name: String,
    pub steps: Option<u32>,
}

impl Symbol {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), steps: None }
    }

    pub fn with_steps(name: impl Into<String>, steps: u32) -> Self {
        Self { name: name.into(), steps: Some(steps) }
    }

    pub fn scoped(name: impl Into<String>) -> Self {
        Self::new(format!("~{}", name.into()))
    }

    pub fn grouped(name: impl Into<String>) -> Self {
        Self::new(format!("#{}", name.into()))
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl From<&str> for Symbol {
    fn from(s: &str) -> Self {
        Symbol::new(s)
    }
}

impl From<String> for Symbol {
    fn from(s: String) -> Self {
        Symbol::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol_new() {
        let sym = Symbol::new("foo");
        assert_eq!(sym.name, "foo");
        assert_eq!(sym.steps, None);
    }

    #[test]
    fn test_symbol_scoped() {
        let sym = Symbol::scoped("foo");
        assert_eq!(sym.name, "~foo");
    }

    #[test]
    fn test_symbol_grouped() {
        let sym = Symbol::grouped("foo");
        assert_eq!(sym.name, "#foo");
    }
}