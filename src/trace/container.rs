use super::{Trace, TraceId, FindCondition};
use super::vcd::VcdTrace;
use super::fst::FstTrace;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::path::Path;

pub struct TraceContainer {
    traces: HashMap<TraceId, Box<dyn Trace>>,
    /// 加载顺序(与 `traces` 同一批 key, 只是有序)。
    ///
    /// 多文件时必须**确定性**地选"谁回答": `HashMap` 的迭代顺序每个进程都不一样,
    /// 于是同一条命令 `-l fsdb -l vcd` 会在两条波形都含该信号时随机选中其一 ——
    /// 实测同一命令 6 次里 5 次答 30000、1 次答 40(同一设计两条波形)。按加载顺序
    /// 迭代 → **先 `-l` 的先算**, 结果可复现。
    order: Vec<TraceId>,
}

impl TraceContainer {
    /// 任一条 trace 报告致命解码错误 → 上报(见 Trace::fatal_error)
    pub fn fatal_error(&self) -> Option<String> {
        for tr in self.ordered_values() {
            if let Some(e) = tr.fatal_error() {
                return Some(e);
            }
        }
        None
    }

    /// 查询前置声明(见 `Trace::prepare`): 转发给所有 trace。
    pub fn prepare(&self, names: &[String]) {
        for tr in self.ordered_values() {
            tr.prepare(names);
        }
    }

    pub fn new() -> Self {
        Self {
            traces: HashMap::new(),
            order: Vec::new(),
        }
    }

    /// 按**加载顺序**迭代 (id, trace)。所有需要"挑一条"的地方都必须走这里。
    fn ordered(&self) -> impl Iterator<Item = (&TraceId, &Box<dyn Trace>)> {
        self.order.iter().filter_map(|tid| self.traces.get(tid).map(|t| (tid, t)))
    }

    fn ordered_values(&self) -> impl Iterator<Item = &dyn Trace> {
        self.ordered().map(|(_, t)| t.as_ref())
    }

    /// 顺序无关的批量操作(逐拍/复位/加载/卸载)。需要"挑一条"的读者请用
    /// `ordered*`;这里按 HashMap 顺序没有任何可观察影响。
    fn any_order_mut(&mut self) -> impl Iterator<Item = &mut Box<dyn Trace>> {
        self.traces.values_mut()
    }

    /// 插入(保持加载顺序; 同 id 重复加载视为替换, 位置不变)。
    fn insert(&mut self, id: TraceId, trace: Box<dyn Trace>) {
        if self.traces.insert(id.clone(), trace).is_none() {
            self.order.push(id);
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
                self.insert(id, Box::new(trace));
                Ok(())
            }
            "fst" => {
                let trace = FstTrace::load(path, id.clone())?;
                self.insert(id, Box::new(trace));
                Ok(())
            }
            "fsdb" => {
                let trace = super::fsdb::FsdbTrace::load(path, id.clone())?;
                self.insert(id, Box::new(trace));
                Ok(())
            }
            _ => Err(format!("Unsupported file format: {}", path.display())),
        }
    }

    pub fn unload(&mut self, id: &TraceId) -> Result<(), String> {
        self.traces.remove(id);
        self.order.retain(|t| t != id);
        Ok(())
    }

    pub fn get(&self, id: &TraceId) -> Option<&dyn Trace> {
        self.traces.get(id).map(|b| b.as_ref())
    }

    pub fn get_mut(&mut self, id: &TraceId) -> Option<&mut Box<dyn Trace>> {
        self.traces.get_mut(id)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.ordered_values().any(|t| t.signals().contains(&name.to_string()))
    }

    /// 已加载的 trace id, **按加载顺序**(多文件选源必须确定, 见 `order`)。
    pub fn trace_ids(&self) -> Vec<TraceId> {
        self.order.clone()
    }

    pub fn signals(&self, id: &TraceId) -> Option<Vec<String>> {
        self.get(id).map(|t| t.signals())
    }

    pub fn all_signals(&self) -> Vec<String> {
        let mut signals = Vec::new();
        for trace in self.ordered_values() {
            signals.extend(trace.signals());
        }
        signals
    }

    pub fn step_all(&mut self, steps: usize) -> Result<(), String> {
        for trace in self.any_order_mut() {
            trace.step(steps)?;
        }
        Ok(())
    }

    /// 第一条 `-l` 加载的波形(多文件时的默认"主"波形)。
    pub fn first_trace(&self) -> Option<&dyn Trace> {
        self.ordered_values().next()
    }

    pub fn traces_iter(&self) -> impl Iterator<Item = &dyn Trace> {
        self.ordered_values()
    }

    pub fn traces_iter_mut(&mut self) -> impl Iterator<Item = &mut Box<dyn Trace>> {
        self.any_order_mut()
    }

    pub fn indices(&self) -> HashMap<TraceId, usize> {
        self.ordered()
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

    /// 查询**实际会读值的**波形集合(每条名字取"第一条能解析出它的波形",
    /// 与解释器/统一引擎的 first-match 选源完全一致), 按加载顺序返回。
    ///
    /// `names` 为空(常量条件、`MAX-INDEX`/INDEX/TS 这类"整条时间线"的问题)时取
    /// **主波形**(第一条 `-l`): 确定, 且不必为一个不引用信号的查询去物化一条
    /// 时间优先后端的全局时间线。
    ///
    /// 为什么要按"选源"而不是"能解析"收口: 时间优先后端(FSDB 走 NPI)的
    /// `max_index` = 索引空间长度 = 全文件所有信号变更时间的并集, 只能全文件扫描
    /// 得到。若把"能解析出该名字的所有波形"都算进来, 那么 `-l vcd -l fsdb` 查一个
    /// **两条波形都有的信号**(同一设计导出两份是常态)时, 虽然值只从 VCD 读, 却仍要
    /// 先把 FSDB 的全局时间线扫出来 —— 内网实测 >900s, 单加载秒级(#32)。
    /// 只有真正会读值的波形才该进索引空间。
    pub fn source_ids(&self, names: &[String]) -> Vec<TraceId> {
        if names.is_empty() {
            return self.order.first().cloned().into_iter().collect();
        }
        let mut out: Vec<TraceId> = Vec::new();
        for n in names {
            // 加载顺序里第一条能解析出该名字的波形 = 解释器会读的那条
            if let Some((tid, _)) = self.ordered().find(|(_, tr)| tr.resolve_name(n).is_some()) {
                if !out.iter().any(|t| t == tid) {
                    out.push(tid.clone());
                }
            }
        }
        out
    }

    /// 查询的索引空间上界(= `source_ids` 里各波形 `max_index` 的最大值)。
    ///
    /// 没被读值的波形不参与索引空间: 否则一个无关(或只是"也有同名信号")的时间优先
    /// 后端会把每条查询拖成一次全文件扫描(内网 #32)。没有任何波形参与 → 0。
    pub fn index_space_max(&self, names: &[String]) -> usize {
        self.source_ids(names)
            .iter()
            .filter_map(|tid| self.get(tid).map(|tr| tr.max_index()))
            .max()
            .unwrap_or(0)
    }

    /// 跨 trace 查索引集: 只认**第一条能解析出该名字的波形**(加载顺序)。
    ///
    /// 不能把多条波形的索引集合合并: 索引是"各自变更时间的排名", 两条不同波形的
    /// 索引空间根本不是一回事, 合并出来的集合既不是这条也不是那条的结果(实测
    /// `-l s1 -l s2` 同名信号会答出 s2 的数)。与解释器/统一引擎的选源(fist-match)
    /// 保持一致。
    pub fn find_indices(&self, name: &str, cond: FindCondition) -> Result<Vec<usize>, String> {
        for trace in self.traces_iter() {
            if let Some(resolved) = trace.resolve_name(name) {
                return trace.find_indices(&resolved, cond);
            }
        }
        Err(format!("signal '{}' not found in any loaded trace", name))
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
            .field("trace_ids", &self.order)
            .finish()
    }
}

pub type SharedTraceContainer = Arc<RwLock<TraceContainer>>;

pub fn new_shared() -> SharedTraceContainer {
    Arc::new(RwLock::new(TraceContainer::new()))
}