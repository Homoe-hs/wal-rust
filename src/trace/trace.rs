//! Trace trait for waveform access

use std::path::Path;

pub type TraceId = String;

#[derive(Debug, Clone, PartialEq)]
pub enum FindCondition {
    Rising,
    Falling,
    High,
    Low,
    Value(u8),
}

pub trait Trace {
    fn id(&self) -> &TraceId;
    fn filename(&self) -> &str;
    fn load(path: &Path) -> Result<Self, String>
    where
        Self: Sized;
    fn unload(&mut self);
    fn step(&mut self, steps: usize) -> Result<(), String>;
    fn signal_value(&self, name: &str, offset: usize) -> Result<ScalarValue, String>;
    fn signal_width(&self, name: &str) -> Result<usize, String>;
    fn signals(&self) -> Vec<String>;
    fn scopes(&self) -> Vec<String>;
    fn max_index(&self) -> usize;
    fn set_index(&mut self, index: usize) -> Result<(), String>;
    fn index(&self) -> usize;
    fn find_indices(&self, name: &str, cond: FindCondition) -> Result<Vec<usize>, String>;
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