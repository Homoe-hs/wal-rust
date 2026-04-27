//! Trace container for managing multiple traces

use super::{Trace, TraceId, FindCondition};
use super::vcd::VcdTrace;
use super::fst::FstTrace;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::path::Path;

pub struct TraceContainer {
    traces: HashMap<TraceId, Box<dyn Trace>>,
}

impl TraceContainer {
    pub fn new() -> Self {
        Self {
            traces: HashMap::new(),
        }
    }

    pub fn load(&mut self, path: &Path, id: TraceId) -> Result<(), String> {
        let ext = path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        let trace: Box<dyn Trace> = match ext.as_str() {
            "vcd" => {
                let vcd_trace = VcdTrace::load(path, id.clone())?;
                Box::new(vcd_trace)
            }
            "fst" => {
                let fst_trace = FstTrace::load(path, id.clone())?;
                Box::new(fst_trace)
            }
            _ => return Err(format!("Unsupported file format: .{}", ext)),
        };

        self.traces.insert(id, trace);
        Ok(())
    }

    pub fn unload(&mut self, id: &TraceId) -> Result<(), String> {
        self.traces.remove(id);
        Ok(())
    }

    pub fn get(&self, id: &TraceId) -> Option<&dyn Trace> {
        self.traces.get(id).map(|b| b.as_ref())
    }

    pub fn get_mut(&mut self, id: &TraceId) -> Option<&mut Box<dyn Trace>> {
        self.traces.get_mut(id)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.traces.values().any(|t| t.signals().contains(&name.to_string()))
    }

    pub fn trace_ids(&self) -> Vec<TraceId> {
        self.traces.keys().cloned().collect()
    }

    pub fn signals(&self, id: &TraceId) -> Option<Vec<String>> {
        self.get(id).map(|t| t.signals())
    }

    pub fn all_signals(&self) -> Vec<String> {
        let mut signals = Vec::new();
        for trace in self.traces.values() {
            signals.extend(trace.signals());
        }
        signals
    }

    pub fn step_all(&mut self, steps: usize) -> Result<(), String> {
        for trace in self.traces.values_mut() {
            trace.step(steps)?;
        }
        Ok(())
    }

    pub fn first_trace(&self) -> Option<&dyn Trace> {
        self.traces.values().next().map(|b| b.as_ref())
    }

    pub fn traces_iter(&self) -> impl Iterator<Item = &dyn Trace> {
        self.traces.values().map(|b| b.as_ref())
    }

    pub fn traces_iter_mut(&mut self) -> impl Iterator<Item = &mut Box<dyn Trace>> {
        self.traces.values_mut()
    }

    pub fn indices(&self) -> HashMap<TraceId, usize> {
        self.traces.iter()
            .map(|(tid, trace)| (tid.clone(), trace.index()))
            .collect()
    }

    pub fn set_index(&mut self, tid: &TraceId, index: usize) -> Result<(), String> {
        if let Some(trace) = self.traces.get_mut(tid) {
            trace.set_index(index)
        } else {
            Err(format!("Trace not found: {}", tid))
        }
    }

    pub fn find_indices(&self, name: &str, cond: FindCondition) -> Result<Vec<usize>, String> {
        if let Some(trace) = self.first_trace() {
            trace.find_indices(name, cond)
        } else {
            Err("No traces loaded".to_string())
        }
    }
}

impl Default for TraceContainer {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for TraceContainer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraceContainer")
            .field("trace_ids", &self.traces.keys().collect::<Vec<_>>())
            .finish()
    }
}

pub type SharedTraceContainer = Arc<RwLock<TraceContainer>>;

pub fn new_shared() -> SharedTraceContainer {
    Arc::new(RwLock::new(TraceContainer::new()))
}