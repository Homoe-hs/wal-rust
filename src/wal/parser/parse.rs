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

    pub fn parse_expr(&mut self, source: &str) -> Result<Value, String> {
        // `%` 不是 tree-sitter 语法里的 token(改语法需重新生成 parser.c),
        // 在解析前把字符串字面量之外的 `%` 归一化成 `mod `。
        let expanded = expand_percent_operator(source);
        let source: &str = expanded.as_ref();
        let tree = self.parse(source)?;
        let root = tree.root_node();
        if root.has_error() {
            return Err("Parse error: syntax error or mismatched parentheses".to_string());
        }
        let mut result = expr_from_node(root, source)?;
        // 词法把 `==`/`===` 拆成多个 `=`: 归一化 (= = a b) → (= a b), 否则
        // `(== x 1)` 会静默得到 false(而不是报错)。
        normalize_eq_aliases(&mut result);
        // Unwrap single-expression program: return the child, not the wrapping list
        if root.kind() == "program" {
            if let Value::List(ref lst) = result {
                if lst.len() == 1 {
                    return Ok(lst[0].clone());
                }
            }
        }
        Ok(result)
    }
}

/// 把字符串字面量之外的独立 `%` token 换成 `mod `(两边保持空格分隔)。
/// 返回 Cow: 没有 `%` 时零拷贝。
///
/// ⚠️ 实现注意: **必须按字节下标切片拷贝**, 不能 `out.push(b as char)` ——
/// 后者会把 UTF-8 的多字节序列逐字节映射成 Latin-1 字符, 于是任何含 `%` 的源码里
/// 的中文都会被双重编码(实测 `(printf "信号: %s\n" "x")` 打印出 `ä¿¡å·: x`)。
/// 原因: 本函数只在源码含 `%` 时触发, 所以以前只有"带格式串的中文"会乱码,
/// 不带 `%` 的中文正常 —— 这也是它长期没被发现的原因。
fn expand_percent_operator(src: &str) -> std::borrow::Cow<'_, str> {
    if !src.contains('%') {
        return std::borrow::Cow::Borrowed(src);
    }
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len() + 8);
    let mut seg = 0usize; // 当前未输出片段的字节起点
    let mut i = 0usize;
    let mut in_str = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            // 字符串内部原样保留(含 \" 转义): 只扫描状态, 不拷贝
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_str = true;
            i += 1;
            continue;
        }
        if b == b'%' {
            // 独立 token 才替换(前一个字符是分隔符、后一个是分隔符/结尾)
            let prev_ok = i == 0 || matches!(bytes[i - 1], b' ' | b'\t' | b'\n' | b'(' | b'[');
            let next_ok = i + 1 >= bytes.len()
                || matches!(bytes[i + 1], b' ' | b'\t' | b'\n' | b')' | b']');
            if prev_ok && next_ok {
                out.push_str(&src[seg..i]); // 按字符边界切片(seg/i 都在 ASCII 位置)
                out.push_str("mod ");
                i += 1;
                seg = i;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&src[seg..]);
    std::borrow::Cow::Owned(out)
}

/// `==`/`===` 在 WAL 词法里是多个 `=` token → 语法树变成 `(= = ...)` / `(= = = ...)`。
/// 这里把 `=`/`!=` 之后多余的前导 `=` 符号去掉, 使 `(== a b)` 等价于 `(= a b)`。
fn normalize_eq_aliases(v: &mut Value) {
    if let Value::List(lst) = v {
        for item in lst.0.iter_mut() {
            normalize_eq_aliases(item);
        }
        let is_eq_op = matches!(&lst.0.first(), Some(Value::Symbol(s)) if s.name == "=" || s.name == "!=");
        if !is_eq_op {
            return;
        }
        // 去掉操作数前导的 `=` 符号(=/=== 被拆开的残余)
        while lst.0.len() > 1 {
            match &lst.0[1] {
                Value::Symbol(s) if s.name == "=" => { lst.0.remove(1); }
                _ => break,
            }
        }
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
    kind == "whitespace" || kind == "_comment" || kind == "_line_comment"
}

fn is_anon_token(kind: &str) -> bool {
    // "@" 是 timed_atom 的分隔符(`expr@offset`); 漏了它时 `x@1` 的 offset
    // 会解析成字符串 "@" → `error: reval: offset must be an integer`。
    matches!(kind, "(" | ")" | "[" | "]" | "{" | "}" | "~" | "#" | "@" | "'" | "`" | "," | ",@")
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
        // 注意: **不能**在这里处理 "symbol" —— 语法里
        //   symbol = choice(base_symbol, scoped_symbol, grouped_symbol)
        // 直接返回原文会把 `#name`(~scope / @offset)语法糖压成字面符号,
        // 下面 grouped_symbol/scoped_symbol/timed_atom 三个分支永远不可达
        // (README §4.1、手册 §4.1 承诺的 `#name` ≡ (resolve-group 'name) 因此失效)。
        "symbol" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if !should_skip_node(child) {
                    return expr_from_node(child, source);
                }
            }
            Err("Empty symbol".to_string())
        }
        "base_symbol" => {
            let text = get_node_text(node, source);
            Ok(Value::Symbol(Symbol::new(text)))
        }
        "int" | "dec_int" | "hex_int" | "bin_int" => {
            let text = get_node_text(node, source);
            let n: i64 = if let Some(hex) = text.strip_prefix("0x") {
                i64::from_str_radix(hex, 16)
            } else if let Some(bin) = text.strip_prefix("0b") {
                i64::from_str_radix(bin, 2)
            } else {
                text.parse()
            }.map_err(|e| format!("Invalid integer '{}': {}", text, e))?;
            Ok(Value::Int(n))
        }
        "float" => {
            let text = get_node_text(node, source);
            let n: f64 = text.parse().map_err(|e| format!("Invalid float '{}': {}", text, e))?;
            Ok(Value::Float(n))
        }
        "string" => {
            let text = get_node_text(node, source);
            let s = text.trim_matches('"').to_string();
            Ok(Value::String(s))
        }
        "bool" => {
            let text = get_node_text(node, source);
            if text == "nil" {
                Ok(Value::Nil)
            } else {
                Ok(Value::Bool(text == "true" || text == "#t"))
            }
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
