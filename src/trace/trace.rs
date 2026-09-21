//! Trace trait for waveform access


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
    IsX,
    IsZ,
    Changed,
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

    /// Raw timestamp (in the file's native time unit) at the given index.
    fn timestamp_at(&self, index: usize) -> Option<u64>;

    /// All change points of a signal as (index, value), including the first
    /// record (unlike `find_indices` with `Changed`, which skips it).
    fn change_points(&self, name: &str) -> Result<Vec<(usize, ScalarValue)>, String>;

    /// 变更点, 但时间是**文件原生单位**(与 `(getwave s)`/`(at s T)` 的输出同单位)。
    ///
    /// 为什么单独开一个方法: `change_points` 走索引空间, 而索引空间要求后端先把
    /// **全局时间线**物化出来 —— 对流式/专有后端(如 FSDB 走 NPI)那是一次全量扫描。
    /// 而 `getwave`/`at`/边沿计数这些查询只需要"信号自己的变更点 + 原生时间",
    /// 默认实现(索引→时间回环)对它们是纯浪费。时间优先的后端覆盖本方法即可跳过。
    fn change_points_time(&self, name: &str) -> Result<Vec<(u64, ScalarValue)>, String> {
        let mut out = Vec::new();
        for (idx, sv) in self.change_points(name)? {
            out.push((self.timestamp_at(idx).unwrap_or(0), sv));
        }
        Ok(out)
    }

    /// 只数"匹配多少个索引", 不要索引本身。
    ///
    /// 默认实现就是 `find_indices(..).len()`; 后端若能直接在自己的变更列上数
    /// (边沿类条件与全局时间线无关), 就省掉一次索引空间物化 —— 对 FSDB 后端
    /// 意味着省掉一次全文件扫描。语义必须与 `find_indices(..).len()` 完全一致。
    fn count_matches(&self, name: &str, cond: FindCondition) -> Result<usize, String> {
        Ok(self.find_indices(name, cond)?.len())
    }

    /// 查询前置声明: 本次查询会碰这些信号(按该 trace 自己的命名规则解析)。
    /// 能"顺手算出来"的后端可把"构建索引"和"提取这些信号的变更列"合并成
    /// 一次遍历(冷启动少读一遍文件); 默认无操作。
    fn prepare(&self, _names: &[String]) {}

    /// Timescale exponent: the time unit is 10^n seconds (None if unknown).
    fn timescale_exp(&self) -> Option<i8>;

    /// 解析信号名(精确 → 叶子名(短名/无点) → 子串)。
    /// 默认实现基于 `signals()`(整表分配);后端应覆盖为带缓存的版本。
    fn resolve_name(&self, name: &str) -> Option<String> {
        let sigs = self.signals();
        if let Some(s) = sigs.iter().find(|s| s.as_str() == name) {
            return Some(s.clone());
        }
        if name.len() <= 8 || !name.contains('.') {
            if let Some(s) = sigs.iter().find(|s| s.rsplitn(2, '.').next().unwrap_or("") == name) {
                return Some(s.clone());
            }
        }
        sigs.iter().find(|s| s.contains(name)).cloned()
    }

    /// 波形开始前**已确定**的初值($dumpvars 快照;含 x/z 或后端无初值概念 → None)。
    /// 用于索引 0 的边沿判定: dumpvars 0 → 首条变化 1 是一次真实上升沿。
    fn defined_initial_value(&self, _name: &str) -> Option<ScalarValue> {
        None
    }

    /// `$dumpvars` 快照原值(**含 x/z**; 该信号没有初值条目 → None)。
    /// 与 `defined_initial_value` 的区别: 后者只给"确定"初值(过滤 x/z), 用于
    /// 边沿判定; 这里要的是"t0 到底写了什么", 供 `(at s T)` 在首变化之前回退、
    /// 以及 `(initial s)` 查询。默认 None(如 FST 后端没有独立的 dumpvars 概念)。
    fn initial_value(&self, _name: &str) -> Option<ScalarValue> {
        None
    }

    /// 严格解析: 名字必须唯一(exact 或唯一叶子/子串); 有歧义 → Err(候选列表)。
    /// 与 `resolve_name` 的区别: 后者取第一个匹配, 这里用于 `at/getwave` 等
    /// "名字有歧义就要报错"的入口。
    fn resolve_name_strict(&self, name: &str) -> Result<String, String> {
        let sigs = self.signals();
        if let Some(s) = sigs.iter().find(|s| s.as_str() == name) {
            return Ok(s.clone());
        }
        let matches: Vec<&String> = sigs.iter().filter(|s| s.contains(name)).collect();
        if matches.len() == 1 {
            return Ok(matches[0].clone());
        }
        Err(format!(
            "signal '{}' not found or ambiguous ({} candidates: {:?})",
            name,
            matches.len(),
            matches.iter().take(5).map(|s| s.as_str()).collect::<Vec<_>>()
        ))
    }

    /// 找不到信号时的近似候选(按编辑距离/包含关系排序, 最多 k 个)。
    /// 默认实现会拉整张信号表; 后端应覆盖为按需遍历(大波形上差别巨大)。
    fn suggest_names(&self, name: &str, k: usize) -> Vec<String> {
        let sigs = self.signals();
        let mut scored: Vec<(usize, &String)> = sigs.iter()
            .map(|s| {
                let leaf = s.rsplit('.').next().unwrap_or(s.as_str());
                (crate::wal::builtins::signal::lev_distance_local(name, s)
                    .min(crate::wal::builtins::signal::lev_distance_local(name, leaf)), s)
            })
            .collect();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        scored.iter().take(k).map(|(_, s)| (*s).clone()).collect()
    }

    /// 致命解码错误(文件损坏/编码不支持)。一旦发生,任何"静默吞错"的查询路径
    /// 都可能给出看似正常的错误结果(如 count=0);顶层求值结束前必须上报。
    fn fatal_error(&self) -> Option<String> {
        None
    }

    /// Top-`k` signals by number of value changes. Default: per-signal
    /// change_points (fine for small signal sets); VCD overrides this with a
    /// single-pass counter so 90k-signal waves stay fast.
    fn signal_change_counts_top(&self, k: usize) -> Vec<(String, usize)> {
        let mut counts: Vec<(String, usize)> = Vec::new();
        for s in self.signals() {
            if let Ok(cp) = self.change_points(&s) {
                counts.push((s, cp.len()));
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        counts.truncate(k);
        counts
    }
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