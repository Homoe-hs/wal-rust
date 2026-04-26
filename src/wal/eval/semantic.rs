//! Semantic error checker for WAL
//!
//! Performs semantic analysis on WAL expressions to detect type errors,
//! arity mismatches, undefined references, and other semantic issues.

use crate::wal::ast::{Value, WList, Symbol, Operator};

#[derive(Debug, Clone, PartialEq)]
pub enum SemanticError {
    TypeMismatch {
        expected: &'static str,
        found: &'static str,
        context: String,
    },
    ArityMismatch {
        operator: String,
        expected_min: usize,
        expected_max: Option<usize>,
        found: usize,
    },
    UndefinedSymbol {
        name: String,
        context: String,
    },
    NotCallable {
        found_type: &'static str,
        context: String,
    },
    InvalidOperation {
        operation: String,
        reason: String,
    },
    UnboundVariable {
        name: String,
        context: String,
    },
    InvalidArgument {
        expected: String,
        found: &'static str,
        context: String,
    },
}

impl SemanticError {
    pub fn message(&self) -> String {
        match self {
            SemanticError::TypeMismatch { expected, found, context } => {
                format!("Type error: expected {}, found {} in {}", expected, found, context)
            }
            SemanticError::ArityMismatch { operator, expected_min, expected_max, found } => {
                if let Some(max) = expected_max {
                    format!("Arity error: {} expects {}-{} arguments, got {}", operator, expected_min, max, found)
                } else {
                    format!("Arity error: {} expects at least {} arguments, got {}", operator, expected_min, found)
                }
            }
            SemanticError::UndefinedSymbol { name, context } => {
                format!("Undefined symbol: {} in {}", name, context)
            }
            SemanticError::NotCallable { found_type, context } => {
                format!("Cannot call value of type {}: {}", found_type, context)
            }
            SemanticError::InvalidOperation { operation, reason } => {
                format!("Invalid operation '{}': {}", operation, reason)
            }
            SemanticError::UnboundVariable { name, context } => {
                format!("Unbound variable: {} in {}", name, context)
            }
            SemanticError::InvalidArgument { expected, found, context } => {
                format!("Invalid argument: expected {}, found {} in {}", expected, found, context)
            }
        }
    }
}

pub struct SemanticChecker;

impl SemanticChecker {
    pub fn check_value(value: &Value) -> Vec<SemanticError> {
        let mut errors = Vec::new();
        Self::check_value_recursive(value, &mut errors);
        errors
    }

    fn check_value_recursive(value: &Value, errors: &mut Vec<SemanticError>) {
        match value {
            Value::List(lst) => {
                for v in &lst.0 {
                    Self::check_value_recursive(v, errors);
                }
            }
            Value::Unquote(v) | Value::UnquoteSplice(v) => {
                Self::check_value_recursive(v, errors);
            }
            _ => {}
        }
    }

    pub fn check_operator_args(op: Operator, args: &[Value]) -> Option<SemanticError> {
        let (min_arity, max_arity) = Self::operator_arity(op)?;
        
        if args.len() < min_arity {
            return Some(SemanticError::ArityMismatch {
                operator: op.as_str().to_string(),
                expected_min: min_arity,
                expected_max: max_arity,
                found: args.len(),
            });
        }
        
        if let Some(max) = max_arity {
            if args.len() > max {
                return Some(SemanticError::ArityMismatch {
                    operator: op.as_str().to_string(),
                    expected_min: min_arity,
                    expected_max: Some(max),
                    found: args.len(),
                });
            }
        }
        
        None
    }

    fn operator_arity(op: Operator) -> Option<(usize, Option<usize>)> {
        match op {
            Operator::Add | Operator::Sub | Operator::Mul | Operator::Div => Some((2, None)),
            Operator::Exp | Operator::Mod => Some((2, Some(2))),
            Operator::Eq | Operator::Neq | Operator::Larger | Operator::Smaller 
            | Operator::LargerEqual | Operator::SmallerEqual => Some((2, Some(2))),
            Operator::Not => Some((1, Some(1))),
            Operator::And | Operator::Or => Some((2, None)),
            Operator::Bor | Operator::Band | Operator::Bxor => Some((2, None)),
            
            Operator::Define => Some((2, Some(2))),
            Operator::Set => Some((2, Some(2))),
            Operator::Let => Some((1, None)),
            Operator::If => Some((3, None)),
            Operator::Fn => Some((1, None)),
            Operator::Defmacro => Some((2, None)),
            
            Operator::Quote | Operator::Quasiquote => Some((1, Some(1))),
            Operator::Unquote => Some((1, Some(1))),
            Operator::Eval => Some((1, Some(1))),
            
            Operator::List | Operator::Map | Operator::Fold | Operator::Zip => Some((1, None)),
            Operator::First | Operator::Second | Operator::Last | Operator::Rest => Some((1, Some(1))),
            Operator::Length => Some((1, Some(1))),
            Operator::In => Some((2, Some(2))),
            Operator::Max | Operator::Min | Operator::Sum | Operator::Average => Some((1, None)),
            
            Operator::IsDefined | Operator::IsAtom | Operator::IsSymbol 
            | Operator::IsString | Operator::IsInt | Operator::IsList => Some((1, Some(1))),
            
            Operator::ConvertBinary | Operator::StringToInt | Operator::BitsToSint
            | Operator::SymbolToString | Operator::StringToSymbol | Operator::IntToString => Some((1, Some(1))),
            
            Operator::Floor | Operator::Ceil | Operator::Round => Some((1, Some(1))),
            
            Operator::Print | Operator::Printf => Some((1, None)),
            Operator::Exit => Some((0, Some(1))),
            
            _ => None,
        }
    }

    pub fn check_binary_args(op: Operator, left: &Value, right: &Value) -> Option<SemanticError> {
        match op {
            Operator::Add | Operator::Sub | Operator::Mul | Operator::Div | Operator::Exp | Operator::Mod => {
                let expected = "number";
                if !Self::is_number(left) {
                    return Some(SemanticError::TypeMismatch {
                        expected,
                        found: left.type_name(),
                        context: format!("left operand of {}", op.as_str()),
                    });
                }
                if !Self::is_number(right) {
                    return Some(SemanticError::TypeMismatch {
                        expected,
                        found: right.type_name(),
                        context: format!("right operand of {}", op.as_str()),
                    });
                }
            }
            Operator::Eq | Operator::Neq | Operator::Larger | Operator::Smaller 
            | Operator::LargerEqual | Operator::SmallerEqual => {
                if !Self::is_comparable(left) || !Self::is_comparable(right) {
                    return Some(SemanticError::TypeMismatch {
                        expected: "comparable type",
                        found: right.type_name(),
                        context: format!("operands of {}", op.as_str()),
                    });
                }
            }
            Operator::Bor | Operator::Band | Operator::Bxor => {
                if !Self::is_integer(left) {
                    return Some(SemanticError::TypeMismatch {
                        expected: "integer",
                        found: left.type_name(),
                        context: format!("left operand of {}", op.as_str()),
                    });
                }
                if !Self::is_integer(right) {
                    return Some(SemanticError::TypeMismatch {
                        expected: "integer",
                        found: right.type_name(),
                        context: format!("right operand of {}", op.as_str()),
                    });
                }
            }
            _ => {}
        }
        None
    }

    fn is_number(v: &Value) -> bool {
        matches!(v, Value::Int(_) | Value::Float(_))
    }

    fn is_integer(v: &Value) -> bool {
        matches!(v, Value::Int(_))
    }

    fn is_comparable(v: &Value) -> bool {
        matches!(v, Value::Int(_) | Value::Float(_) | Value::String(_) | Value::Bool(_))
    }

    pub fn validate_closure_args(closure_args: &[Symbol], call_args: &[Value]) -> Option<SemanticError> {
        if closure_args.is_empty() {
            return None;
        }
        
        let last_is_variadic = closure_args.last()
            .map(|s| s.name.starts_with("&"))
            .unwrap_or(false);
        
        if last_is_variadic {
            if call_args.len() < closure_args.len() - 1 {
                return Some(SemanticError::ArityMismatch {
                    operator: "closure".to_string(),
                    expected_min: closure_args.len() - 1,
                    expected_max: None,
                    found: call_args.len(),
                });
            }
        } else if call_args.len() != closure_args.len() {
            return Some(SemanticError::ArityMismatch {
                operator: "closure".to_string(),
                expected_min: closure_args.len(),
                expected_max: Some(closure_args.len()),
                found: call_args.len(),
            });
        }
        
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_operator_arity_add() {
        let err = SemanticChecker::check_operator_args(Operator::Add, &[Value::Int(1)]);
        assert!(matches!(err, Some(SemanticError::ArityMismatch { .. })));
        
        let err = SemanticChecker::check_operator_args(Operator::Add, &[Value::Int(1), Value::Int(2)]);
        assert!(err.is_none());
        
        let err = SemanticChecker::check_operator_args(Operator::Add, &[Value::Int(1), Value::Int(2), Value::Int(3)]);
        assert!(err.is_none());
    }

    #[test]
    fn test_operator_arity_define() {
        let err = SemanticChecker::check_operator_args(Operator::Define, &[Value::Int(1)]);
        assert!(matches!(err, Some(SemanticError::ArityMismatch { .. })));
        
        let err = SemanticChecker::check_operator_args(Operator::Define, &[Value::Int(1), Value::Int(2), Value::Int(3)]);
        assert!(matches!(err, Some(SemanticError::ArityMismatch { .. })));
    }

    #[test]
    fn test_binary_args_add() {
        let err = SemanticChecker::check_binary_args(Operator::Add, &Value::Int(1), &Value::Int(2));
        assert!(err.is_none());
        
        let err = SemanticChecker::check_binary_args(Operator::Add, &Value::Int(1), &Value::String("a".to_string()));
        assert!(matches!(err, Some(SemanticError::TypeMismatch { .. })));
    }

    #[test]
    fn test_binary_args_bitwise() {
        let err = SemanticChecker::check_binary_args(Operator::Bor, &Value::Int(1), &Value::Int(2));
        assert!(err.is_none());
        
        let err = SemanticChecker::check_binary_args(Operator::Band, &Value::Int(1), &Value::Float(1.5));
        assert!(matches!(err, Some(SemanticError::TypeMismatch { .. })));
    }

    #[test]
    fn test_semantic_error_message() {
        let err = SemanticError::TypeMismatch {
            expected: "number",
            found: "string",
            context: "addition".to_string(),
        };
        assert_eq!(err.message(), "Type error: expected number, found string in addition");
        
        let err = SemanticError::UndefinedSymbol {
            name: "foo".to_string(),
            context: "body".to_string(),
        };
        assert_eq!(err.message(), "Undefined symbol: foo in body");
    }
}