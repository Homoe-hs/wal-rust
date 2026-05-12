//! Trace trait for waveform access

use std::collections::HashMap;

pub type TraceId = String;

/// A single entry in a batch find/index operation.
#[derive(Debug, Clone)]
pub enum BatchEntry {
    /// Simple single-signal condition: signal must match condition
    Simple(String, FindCondition),
    /// AND of multiple signals: ALL must match their respective conditions
    And(Vec<(String, FindCondition)>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum FindCondition {
    Rising,
    Falling,
    High,
    Low,
    Value(u8),
    ValueI64(i64),
    Neq(u8),
    NeqI64(i64),
}

pub trait Trace {
    fn id(&self) -> &TraceId;
    fn filename(&self) -> &str;
    fn step(&mut self, steps: usize) -> Result<(), String>;
    fn signal_value(&self, name: &str, offset: usize) -> Result<ScalarValue, String>;
    fn signal_width(&self, name: &str) -> Result<usize, String>;
    fn signals(&self) -> Vec<String>;
    fn scopes(&self) -> Vec<String>;
    fn max_index(&self) -> usize;
    fn set_index(&mut self, index: usize) -> Result<(), String>;
    fn index(&self) -> usize;
    fn find_indices(&self, name: &str, cond: FindCondition) -> Result<Vec<usize>, String>;
    /// Batch find: single pass, multiple entries. Returns counts per entry in order.
    fn find_indices_batch(&self, entries: &[BatchEntry]) -> Result<Vec<(String, Vec<usize>)>, String>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum ScalarValue {
    Bit(u8),     // 0, 1, x, z
    Vector(Vec<u8>),
    Real(f64),
}

impl ScalarValue {
    pub fn to_int(&self) -> Option<i64> {
        match self {
            ScalarValue::Bit(v) => Some(*v as i64),
            ScalarValue::Vector(_) => None,
            ScalarValue::Real(_) => None,
        }
    }

    pub fn to_float(&self) -> Option<f64> {
        match self {
            ScalarValue::Bit(v) => Some(*v as f64),
            ScalarValue::Vector(_) => None,
            ScalarValue::Real(v) => Some(*v),
        }
    }
}