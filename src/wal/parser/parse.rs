//! WalParser using tree-sitter
//!
//! Wraps tree-sitter parser for WAL language.

use tree_sitter::Parser;
use tree_sitter::Tree;
use crate::wal::ast::{Value, Symbol, WList, Operator};

pub struct WalParser {
    parser: Parser,
}

impl WalParser {
    pub fn new() -> Result<Self, String> {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::wal::language())
            .map_err(|e| format!("Failed to set WAL language: {}", e))?;
        Ok(WalParser { parser })
    }

    pub fn parse(&mut self, source: &str) -> Result<Tree, String> {
        self.parser
            .parse(source, None)
            .ok_or_else(|| "Parse failed".to_string())
    }

    pub fn parse_with_errors(&mut self, source: &str) -> Tree {
        self.parser.parse(source, None).unwrap_or_else(|| {
            let mut parser = Parser::new();
            parser.set_language(&crate::wal::language()).unwrap();
            parser.parse(source, None).unwrap()
        })
    }

    pub fn parse_expr(&mut self, source: &str) -> Result<Value, String> {
        let tree = self.parse(source)?;
        let root = tree.root_node();
        if root.kind() == "program" {
            let mut last_result: Result<Value, String> = Ok(Value::Nil);
            let mut cursor = root.walk();
            let mut count = 0;
            for child in root.children(&mut cursor) {
                if should_skip_node(child) {
                    continue;
                }
                count += 1;
                last_result = expr_from_node(child, source);
            }
            if count == 1 {
                return last_result;
            }
            last_result
        } else {
            expr_from_node(root, source)
        }
    }
}

impl Default for WalParser {
    fn default() -> Self {
        Self::new().expect("Failed to create WalParser")
    }
}

pub fn parse_to_value(source: &str) -> Result<Value, String> {
    let mut parser = WalParser::new()?;
    parser.parse_expr(source)
}

fn get_node_text(node: tree_sitter::Node, source: &str) -> String {
    let range = node.byte_range();
    source.get(range).unwrap_or("").to_string()
}

fn is_whitespace_or_comment(kind: &str) -> bool {
    kind == "whitespace" || kind == "_comment"
}

fn is_anon_token(kind: &str) -> bool {
    matches!(kind, "(" | ")" | "[" | "]" | "{" | "}" | "~" | "#" | "'" | "`" | "," | ",@")
}

fn should_skip_node(node: tree_sitter::Node) -> bool {
    let kind = node.kind();
    is_whitespace_or_comment(kind) || is_anon_token(kind)
}

pub fn expr_from_node(node: tree_sitter::Node, source: &str) -> Result<Value, String> {
    let kind = node.kind();
    match kind {
        "program" | "sexpr_list" => {
            let mut values = Vec::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if should_skip_node(child) {
                    continue;
                }
                values.push(expr_from_node(child, source)?);
            }
            if values.is_empty() {
                Ok(Value::List(WList::new()))
            } else {
                Ok(Value::List(WList::from_vec(values)))
            }
        }
        "sexpr" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if should_skip_node(child) {
                    continue;
                }
                return expr_from_node(child, source);
            }
            Err("Empty sexpr".to_string())
        }
        "list" => {
            let mut cursor = node.walk();
            let children: Vec<_> = node.children(&mut cursor)
                .filter(|c| !should_skip_node(c.clone()))
                .collect();

            let mut values = Vec::new();
            for child in &children {
                let val = expr_from_node(child.clone(), source)?;
                if let Value::List(inner_list) = val {
                    values.extend(inner_list.0);
                } else {
                    values.push(val);
                }
            }
            Ok(Value::List(WList::from_vec(values)))
        }
        "atom" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if !should_skip_node(child) {
                    return expr_from_node(child, source);
                }
            }
            Err("Empty atom".to_string())
        }
        "symbol" | "base_symbol" => {
            let text = get_node_text(node, source);
            Ok(Value::Symbol(Symbol::new(text)))
        }
        "int" | "dec_int" => {
            let text = get_node_text(node, source);
            let n: i64 = text.parse().unwrap_or(0);
            Ok(Value::Int(n))
        }
        "float" => {
            let text = get_node_text(node, source);
            let n: f64 = text.parse().unwrap_or(0.0);
            Ok(Value::Float(n))
        }
        "string" => {
            let text = get_node_text(node, source);
            let s = text.trim_matches('"').to_string();
            Ok(Value::String(s))
        }
        "bool" => {
            let text = get_node_text(node, source);
            Ok(Value::Bool(text == "true" || text == "#t"))
        }
        "operator" => {
            let text = get_node_text(node, source);
            if let Some(op) = Operator::from_str(&text) {
                Ok(Value::Symbol(Symbol::new(op.as_str())))
            } else {
                Ok(Value::Symbol(Symbol::new(text)))
            }
        }
        "timed_atom" => {
            let mut values = Vec::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if should_skip_node(child) {
                    continue;
                }
                values.push(expr_from_node(child, source)?);
            }
            if values.is_empty() {
                Err("Invalid timed_atom".to_string())
            } else if values.len() >= 2 {
                // expr@offset format: (rel_eval expr offset)
                Ok(Value::List(WList::from_vec(vec![
                    Value::Symbol(Symbol::new("rel_eval")),
                    values[0].clone(),
                    values[1].clone(),
                ])))
            } else {
                Ok(values[0].clone())
            }
        }
        "grouped_symbol" => {
            let mut values = Vec::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if should_skip_node(child) {
                    continue;
                }
                values.push(expr_from_node(child, source)?);
            }
            if values.is_empty() {
                Err("Invalid grouped_symbol".to_string())
            } else {
                // #signal -> (resolve-group 'signal)
                Ok(Value::List(WList::from_vec(vec![
                    Value::Symbol(Symbol::new("resolve-group")),
                    Value::List(WList::from_vec(vec![
                        Value::Symbol(Symbol::new("quote")),
                        values[0].clone(),
                    ])),
                ])))
            }
        }
        "scoped_symbol" => {
            let mut values = Vec::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if should_skip_node(child) {
                    continue;
                }
                values.push(expr_from_node(child, source)?);
            }
            if values.is_empty() {
                Err("Invalid scoped_symbol".to_string())
            } else {
                // ~scope -> (in-scope scope)
                Ok(Value::List(WList::from_vec(vec![
                    Value::Symbol(Symbol::new("in-scope")),
                    values[0].clone(),
                ])))
            }
        }
        "quoted" => {
            let mut cursor = node.walk();
            let children: Vec<_> = node.children(&mut cursor)
                .filter(|c| !should_skip_node(c.clone()))
                .collect();

            if children.is_empty() {
                return Ok(Value::Nil);
            }
            let inner_val = expr_from_node(children[0].clone(), source)?;
            Ok(Value::List(WList::from_vec(vec![
                Value::Symbol(Symbol::new("quote")),
                inner_val,
            ])))
        }
        "quasiquoted" => {
            let mut cursor = node.walk();
            let children: Vec<_> = node.children(&mut cursor)
                .filter(|c| !should_skip_node(c.clone()))
                .collect();

            if children.is_empty() {
                return Ok(Value::Nil);
            }
            let inner_val = expr_from_node(children[0].clone(), source)?;
            Ok(Value::List(WList::from_vec(vec![
                Value::Symbol(Symbol::new("quasiquote")),
                inner_val,
            ])))
        }
        "unquote" => {
            let mut cursor = node.walk();
            let children: Vec<_> = node.children(&mut cursor)
                .filter(|c| !should_skip_node(c.clone()))
                .collect();

            if children.is_empty() {
                return Ok(Value::Nil);
            }
            let inner_val = expr_from_node(children[0].clone(), source)?;
            Ok(Value::Unquote(Box::new(inner_val)))
        }
        "unquote_splice" => {
            let mut cursor = node.walk();
            let children: Vec<_> = node.children(&mut cursor)
                .filter(|c| !should_skip_node(c.clone()))
                .collect();

            if children.is_empty() {
                return Ok(Value::Nil);
            }
            let inner_val = expr_from_node(children[0].clone(), source)?;
            Ok(Value::UnquoteSplice(Box::new(inner_val)))
        }
        _ => {
            let text = get_node_text(node, source);
            if text.is_empty() {
                Ok(Value::Nil)
            } else {
                Ok(Value::String(text))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple() {
        let mut parser = WalParser::new().unwrap();
        let tree = parser.parse("(+ 1 2)").unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn test_parse_expr() {
        let mut parser = WalParser::new().unwrap();
        let result = parser.parse_expr("(+ 1 2)");
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_quasiquote() {
        let mut parser = WalParser::new().unwrap();
        let result = parser.parse_expr("`(+ 1 ,x)");
        assert!(result.is_ok());
    }
}
