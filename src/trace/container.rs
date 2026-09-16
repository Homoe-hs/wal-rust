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
    /// 任一条 trace 报告致命解码错误 → 上报(见 Trace::fatal_error)
    pub fn fatal_error(&self) -> Option<String> {
        for tr in self.traces.values() {
            if let Some(e) = tr.fatal_error() {
                return Some(e);
            }
        }
        None
    }

    /// 查询前置声明(见 `Trace::prepare`): 转发给所有 trace。
    pub fn prepare(&self, names: &[String]) {
        for tr in self.traces.values() {
            tr.prepare(names);
        }
    }

    pub fn new() -> Self {
        Self {
            traces: HashMap::new(),
        }
    }

    fn detect_format(path: &Path) -> Option<&'static str> {
        let fname = path.to_string_lossy().to_lowercase();
        if fname.ends_with(".vcd.gz") || fname.ends_with(".vcd.bz2") {
            // 压缩包由 reject_unsupported 明确拒绝(见 load)
            return None;
        }
        if fname.ends_with(".vcd") {
            return Some("vcd");
        }
        if fname.ends_with(".fst") {
            return Some("fst");
        }
        if fname.ends_with(".fsdb") {
            return Some("fsdb");
        }
        if let Ok(mut f) = std::fs::File::open(path) {
            use std::io::Read;
            let mut buf = [0u8; 16];
            if f.read(&mut buf).unwrap_or(0) >= 4 {
                // 注意: 必须比较 3 字节(VCD 头 `$date`/`$timescale`/`$version`/`$scope`),
                // 之前写成 &buf[..4] == b"$da" 长度不等 → 恒假, 非 .vcd 扩展名的文件
                // 一律被判成不支持。
                if &buf[..3] == b"$da" || &buf[..3] == b"$ti" || &buf[..3] == b"$ve" || &buf[..3] == b"$sc" {
                    return Some("vcd");
                }
                if buf[0] == 0x00 || buf[0] == 0x01 || buf[0] == 0x03 || buf[0] == 0x04 {
                    return Some("fst");
                }
                if buf.windows(4).any(|w| w == b"FSDB") {
                    return Some("fsdb");
                }
            }
        }
        None
    }

    /// 压缩包 / FSDB 等"看似支持实则读不出"的格式: 明确报错, 不要静默退化。
    fn reject_unsupported(path: &Path) -> Result<(), String> {
        use std::io::Read;
        // 头部取样 8KB: EVCD 的 `$dumpports` / `$var port` 可能出现在前 16 字节之后
        let mut buf = [0u8; 8192];
        let n = match std::fs::File::open(path) {
            Ok(mut f) => f.read(&mut buf).unwrap_or(0),
            Err(_) => 0,
        };
        let lower = path.to_string_lossy().to_lowercase();
        if n >= 2 && buf[0] == 0x1f && buf[1] == 0x8b {
            return Err(format!(
                "{}: gzip 压缩波形暂不支持, 请先解压(如 gunzip)再查询", path.display()));
        }
        if n >= 3 && &buf[..3] == b"BZh" {
            return Err(format!(
                "{}: bzip2 压缩波形暂不支持, 请先解压(如 bunzip2)再查询", path.display()));
        }
        let head = &buf[..n];
        let is_fsdb = lower.ends_with(".fsdb")
            || head.windows(4).any(|w| w == b"FSDB")
            || (n >= 8 && head[..4] == [0, 0, 0, 0] && &head[4..8] == b"FSDB");
        if is_fsdb {
            // 有 Verdi/NPI 就用 NPI 读;没有则维持原来的明确报错(不静默退化)
            if super::fsdb::npi_available() {
                return Ok(());
            }
            let why = super::fsdb::npi_unavailable_reason();
            return Err(format!(
                "{}: FSDB 需要 Verdi 的 NPI 读库(libNPI.so)。\n  \
                 设置 $VERDI_HOME(Verdi 安装根)或 $WAL_NPI_LIB(libNPI.so 绝对路径)后重试;\n  \
                 原因: {}",
                path.display(),
                why
            ));
        }
        // EVCD($dumpports / $var port): 值行是 p<strength><strength> 语法, 与 VCD 不同,
        // 直接拒绝而不是按普通 VCD 误读。
        if lower.ends_with(".evcd")
            || head.windows(10).any(|w| w == b"$dumpports")
            || head.windows(9).any(|w| w == b"$var port")
        {
            return Err(format!(
                "{}: EVCD(端口 dump)格式暂不支持; 请改用 $dumpfile/$dumpvars 生成 VCD",
                path.display()));
        }
        Ok(())
    }

    pub fn load(&mut self, path: &Path, id: TraceId) -> Result<(), String> {
        Self::reject_unsupported(path)?;
        let fmt = Self::detect_format(path)
            .ok_or_else(|| format!("Unsupported file format: {}", path.display()))?;

        match fmt {
            "vcd" => {
                let trace = VcdTrace::load(path, id.clone())?;
                self.traces.insert(id, Box::new(trace));
                Ok(())
            }
            "fst" => {
                let trace = FstTrace::load(path, id.clone())?;
                self.traces.insert(id, Box::new(trace));
                Ok(())
            }
            "fsdb" => {
                let trace = super::fsdb::FsdbTrace::load(path, id.clone())?;
                self.traces.insert(id, Box::new(trace));
                Ok(())
            }
            _ => Err(format!("Unsupported file format: {}", path.display())),
        }
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