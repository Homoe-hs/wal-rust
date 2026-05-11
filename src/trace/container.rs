//! Trace container for managing multiple traces
//!
//! FST cache: loading a .vcd file automatically generates a .fst cache file
//! in the same directory for faster subsequent access.

use super::{Trace, TraceId, FindCondition};
use super::vcd::VcdTrace;
use super::fst::FstTrace;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::sync::atomic::AtomicU64;
use std::path::Path;
use std::time::Duration;
use std::thread::JoinHandle;

pub struct TraceContainer {
    traces: HashMap<TraceId, Box<dyn Trace>>,
    cache_handles: Vec<JoinHandle<()>>,
}

impl TraceContainer {
    pub fn new() -> Self {
        Self {
            traces: HashMap::new(),
            cache_handles: Vec::new(),
        }
    }

    /// Wait for ongoing FST cache conversions to complete.
    pub fn wait_for_fst_cache(&mut self) {
        for h in self.cache_handles.drain(..) {
            let _ = h.join();
        }
    }

    fn detect_format(path: &Path) -> Option<&'static str> {
        let fname = path.to_string_lossy().to_lowercase();
        if fname.ends_with(".vcd") || fname.ends_with(".vcd.gz") || fname.ends_with(".vcd.bz2") {
            return Some("vcd");
        }
        if fname.ends_with(".fst") {
            return Some("fst");
        }
        if let Ok(mut f) = std::fs::File::open(path) {
            use std::io::Read;
            let mut buf = [0u8; 16];
            if f.read(&mut buf).unwrap_or(0) >= 4 {
                if &buf[..4] == b"$da" || &buf[..4] == b"$ti" || &buf[..4] == b"$ve" || &buf[..4] == b"$sc" {
                    return Some("vcd");
                }
                if buf[0] == 0x00 || buf[0] == 0x01 || buf[0] == 0x03 || buf[0] == 0x04 {
                    return Some("fst");
                }
            }
        }
        None
    }

    /// Check if .fst cache is fresh (exists and newer than .vcd)
    fn cache_is_fresh(vcd_path: &Path, fst_path: &Path) -> bool {
        if !fst_path.exists() { return false; }
        let tmp_path = fst_path.with_extension("fst.tmp");
        if tmp_path.exists() { return false; }
        let vcd_mtime = match std::fs::metadata(vcd_path).and_then(|m| m.modified()) {
            Ok(t) => t, Err(_) => return false,
        };
        let fst_mtime = match std::fs::metadata(fst_path).and_then(|m| m.modified()) {
            Ok(t) => t, Err(_) => return false,
        };
        fst_mtime >= vcd_mtime
    }

    pub fn load(&mut self, path: &Path, id: TraceId) -> Result<(), String> {
        let fmt = Self::detect_format(path)
            .ok_or_else(|| format!("Unsupported file format: {}", path.display()))?;

        match fmt {
            "vcd" => self.load_vcd(path, id),
            "fst" => {
                let fst_trace = FstTrace::load(path, id.clone())?;
                self.traces.insert(id, Box::new(fst_trace));
                Ok(())
            }
            _ => Err(format!("Unsupported file format: {}", path.display())),
        }
    }

    fn load_vcd(&mut self, path: &Path, id: TraceId) -> Result<(), String> {
        let fst_path = path.with_extension("fst");
        let tmp_path = path.with_extension("fst.tmp");
        let resume_path = path.with_extension("fst.resume");

        // Fast path: FST cache is fresh
        if Self::cache_is_fresh(path, &fst_path) {
            match FstTrace::load(&fst_path, id.clone()) {
                Ok(trace) => {
                    self.traces.insert(id, Box::new(trace));
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("[FST cache] Corrupted, falling back to VCD: {}", e);
                    let _ = std::fs::remove_file(&fst_path);
                }
            }
        }

        // Load VCD trace for immediate use
        let vcd_trace = VcdTrace::load(path, id.clone())?;
        self.traces.insert(id.clone(), Box::new(vcd_trace));

        // Spawn background conversion
        let vcd_path = path.to_owned();
        let fst_path = fst_path.to_owned();
        let tmp_path = tmp_path.to_owned();
        let resume_path = resume_path.to_owned();

        let handle = std::thread::spawn(move || {
            let file_size = std::fs::metadata(&vcd_path)
                .map(|m| m.len())
                .unwrap_or(0);
            let progress = Arc::new(AtomicU64::new(0));

            eprintln!("[FST cache] Converting: {} → {}",
                vcd_path.display(), fst_path.display());

            match crate::vcd::convert::vcd_to_fst_streaming(
                &vcd_path, &tmp_path, &resume_path, progress,
            ) {
                Ok(()) => {
                    if let Err(e) = std::fs::rename(&tmp_path, &fst_path) {
                        eprintln!("[FST cache] Rename failed: {}", e);
                    } else {
                        let file_size_fmt = crate::vcd::convert::format_size(file_size);
                        eprintln!("[FST cache] Done: {} ({} → FST)",
                            fst_path.display(), file_size_fmt);
                    }
                    let _ = std::fs::remove_file(&resume_path);
                }
                Err(e) => {
                    eprintln!("[FST cache] Failed: {}", e);
                    let _ = std::fs::remove_file(&tmp_path);
                    let _ = std::fs::remove_file(&resume_path);
                }
            }
        });
        self.cache_handles.push(handle);

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
        let mut all_indices = Vec::new();
        for trace in self.traces_iter() {
            if let Ok(indices) = trace.find_indices(name, cond.clone()) {
                all_indices.extend(indices);
            }
        }
        if all_indices.is_empty() && !self.traces.is_empty() {
            return Err(format!("signal '{}' not found in any loaded trace", name));
        }
        all_indices.sort();
        all_indices.dedup();
        Ok(all_indices)
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