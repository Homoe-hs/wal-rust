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
        // Budgeted rendering: huge lists (e.g. 90k signals) must not blow up
        // terminals or model context — show a bounded prefix + item count.
        const MAX_ITEMS: usize = 24;
        const MAX_CHARS: usize = 1024;
        let mut out = String::new();
        fmt_list_limited(&self.0, &mut out, MAX_CHARS, 0, MAX_ITEMS);
        write!(f, "{}", out)
    }
}

/// Budgeted recursive list rendering: at most `items_left` elements and
/// `budget` characters; silenced tail → ` ...(N items)`.
fn fmt_list_limited(items: &[Value], out: &mut String, budget: usize, depth: usize, items_left: usize) {
    out.push('(');
    let mut first = true;
    let mut truncated = false;
    for item in items {
        if truncated || out.len() >= budget || items_left == 0 {
            truncated = true;
            break;
        }
        if !first {
            out.push(' ');
        }
        first = false;
        match item {
            Value::List(inner) => {
                if depth < 8 {
                    fmt_list_limited(&inner.0, out, budget, depth + 1, items_left.saturating_sub(1));
                } else {
                    out.push_str("[...]");
                }
            }
            other => {
                let s = format!("{}", other);
                if out.len() + s.len() > budget {
                    out.push_str("[...]");
                    truncated = true;
                    break;
                }
                out.push_str(&s);
            }
        }
    }
    if truncated {
        out.push_str(&format!(" ...({} items)", items.len()));
    }
    out.push(')');
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
    fn test_wlist_display_short_unaffected() {
        let l = WList::from_vec(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        assert_eq!(format!("{}", l), "(1 2 3)");
    }

    #[test]
    fn test_wlist_display_long_truncated() {
        let big: Vec<Value> = (0..10000).map(|i| Value::Int(i as i64)).collect();
        let s = format!("{}", WList::from_vec(big));
        assert!(s.contains("(10000 items)"), "truncation marker missing: {}", &s[..s.len().min(80)]);
        assert!(s.len() < 1200, "long list printed too much: {} chars", s.len());
        // exact count must be reported
        assert!(s.contains("(10000 items)"), "{}", s);
    }

    #[test]
    fn test_wlist_first_rest() {
        let list = WList::from_vec(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        assert_eq!(list.first(), Some(&Value::Int(1)));
        assert_eq!(list.rest(), vec![Value::Int(2), Value::Int(3)]);
    }
}