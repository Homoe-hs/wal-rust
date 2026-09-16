//! VCD trace implementation (two-pass scan + sparse index + LRU cache + memchr jump scan + decoded signal cache)
//!
//! Optimized loading: zero-copy byte-parsing, packed signal IDs, pre-allocation.

use crate::trace::{Trace, TraceId, ScalarValue, FindCondition, BatchEntry};
use crate::vcd::types::VcdValue;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::path::Path;

const MAX_DECODED_SIGNALS: usize = 256;

#[derive(Clone, Copy)]
enum CacheType {
    FullScan,
    PartialScan(u32),
}

struct DecodedSignal {
    change_indices: Vec<u32>,
    values: Vec<VcdValue>,
    full_scan: bool,
    scanned_up_to: u32,
}

/// In-memory columnar change list for one signal (budgeted; built during
/// PASS-1b from the already-parsed lines — queries then never re-scan the
/// file for covered signals). Values are packed 2 bits per bit
/// (0=0, 1=1, x=2, z=3, MSB first) for widths <= 64.
struct Col {
    width: usize,
    idxs: Vec<u32>,
    states: Vec<u128>,
}

/// Column-offset index: (ts_idx, file offset) per change, NO values — the
/// DuckDB-style "only read the bytes you need" variant. Queries binary-search
/// the offsets and fetch the value lines from the mmap on demand (a few dozen
/// 4KB pages per signal instead of a full-file scan).
struct OffCol {
    idxs: Vec<u32>,
    offsets: Vec<u64>,
}

fn col_states_from_vcd(v: &VcdValue, width: usize) -> Option<u128> {
    let bytes: Vec<u8> = match v {
        VcdValue::Bit(b) => vec![*b],
        VcdValue::Vector(vec) if vec.len() == width => vec.clone(),
        _ => return None,
    };
    let mut st: u128 = 0;
    for (p, &c) in bytes.iter().enumerate() {
        let code = match c {
            b'0' => 0u8, b'1' => 1u8,
            b'x' | b'X' => 2u8,
            b'z' | b'Z' => 3u8,
            _ => return None,
        };
        st |= (code as u128) << (2 * (width - 1 - p));
    }
    Some(st)
}

fn vcd_from_states(st: u128, width: usize) -> VcdValue {
    let mut out = Vec::with_capacity(width);
    for p in 0..width {
        let code = ((st >> (2 * (width - 1 - p))) & 0b11) as u8;
        out.push(match code {
            0 => b'0', 1 => b'1', 2 => b'x', _ => b'z',
        });
    }
    if width == 1 { VcdValue::Bit(out[0]) } else { VcdValue::Vector(out) }
}

/// Column cache budget in bytes — OPT-IN via WAL_COL_CACHE_MB (MB). Default
/// 0 keeps the load at the fast sampled-parse speed (no regression); with
/// the env set the load pays the full-parse cost once and every covered
/// query answers from memory (zero file re-scan).
fn col_cache_budget() -> usize {
    if let Ok(mb) = std::env::var("WAL_COL_CACHE_MB") {
        if let Ok(v) = mb.trim().parse::<usize>() {
            if v == 0 { return 0; }
            return v * 1024 * 1024;
        }
    }
    0
}

/// LRU cache capacity (number of entries)
fn adapt_lru_capacity(file_size: usize, signal_count: usize) -> usize {
    // Base: 5 timestamps per signal, capped at 200K signals
    let base = (signal_count as u64).min(200_000) * 5;
    // File size factor: larger files need larger cache
    let file_factor = match file_size {
        0..=1_000_000_000 => 1,        // <1GB
        1_000_000_001..=50_000_000_000 => 3,   // 1-50GB
        _ => 5,                          // >50GB
    };
    (base * file_factor).max(50_000).min(5_000_000) as usize
}

/// 并行估算 header 里的 `$var` 数量(用于一次性预留容量, 避免反复扩容/重哈希)。
/// 指数扩张地并行查找 `$enddefinitions` 定位 header 末端(小 header 只扫几 MB,
/// 大 header 才逐步扩大), 再并行统计 `$var ` 次数。找不到 → 0(退化为按需增长)。
fn estimate_var_count(data: &[u8], cap_bytes: usize) -> usize {
    use rayon::prelude::*;
    let n_threads = num_cpus::get().max(4);
    let hard_cap = data.len().min(cap_bytes);
    let mut hi = (8usize << 20).min(hard_cap);
    let mut header_end: Option<usize> = None;
    loop {
        let chunk = (hi / n_threads).max(1 << 20);
        let ranges: Vec<(usize, usize)> = (0..n_threads)
            .map(|i| (i * chunk, ((i + 1) * chunk + 16).min(hi)))
            .filter(|(a, b)| a < b)
            .collect();
        if let Some(p) = ranges.par_iter()
            .filter_map(|&(a, b)| memchr::memmem::find(&data[a..b], b"$enddefinitions").map(|o| a + o))
            .min()
        {
            header_end = Some(p);
            break;
        }
        if hi >= hard_cap {
            break;
        }
        hi = (hi * 4).min(hard_cap);
    }
    let Some(end) = header_end else { return 0 };
    // 统计: 边界多算几个无妨(只是容量估计)
    let chunk = (end / n_threads).max(1 << 20);
    data[..end]
        .par_chunks(chunk)
        .map(|c| memchr::memmem::find_iter(c, b"$var").count())
        .sum()
}

/// True for real VCD timestamp lines ("#<digits>"). Icarus Verilog dumps
/// 1-bit value lines like "#%" (value '#' + id) that start with '#' but are
/// not timestamps; treating them as timestamps duplicates an entry.
#[inline]
fn is_ts_line(line: &[u8]) -> bool {
    // CRLF 文件的行尾带 \r → 先剥掉再判数字(VCS/Windows 工具产出的 VCD 常见)
    let mut l = line;
    while matches!(l.last(), Some(b'\r') | Some(b'\n') | Some(b' ')) {
        l = &l[..l.len() - 1];
    }
    l.len() > 1 && l[1..].iter().all(|c| c.is_ascii_digit())
}

/// Line-scoped check at absolute position `pos` in `data` (up to the next '\n').
#[inline]
fn is_ts_line_at(data: &[u8], pos: usize) -> bool {
    let line_end = memchr::memchr(b'\n', &data[pos..])
        .map(|n| pos + n)
        .unwrap_or(data.len());
    is_ts_line(&data[pos..line_end])
}

fn build_ts_store(ts: Vec<u64>) -> TsStore {
    if std::env::var("WAL_DEBUG_FIND").is_ok() {
        eprintln!("build_ts_store: len={} first={:?} last={:?}",
                  ts.len(), ts.iter().take(5).collect::<Vec<_>>(), ts.iter().rev().take(3).collect::<Vec<_>>());
    }
    if ts.len() >= 2 {
        let step = ts[1] - ts[0];
        let mut uniform = step > 0;
        if uniform {
            for i in 2..ts.len() {
                if ts[i] - ts[i - 1] != step {
                    uniform = false;
                    break;
                }
            }
        }
        if uniform && step > 0 {
            return TsStore::Uniform { start: ts[0], step, count: ts.len() as u64 };
        }
    }
    TsStore::Dense(ts)
}

fn hash_sig_id(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0;
    for &b in bytes {
        h = (h << 8) | (b as u64);
    }
    h
}

/// Compact timestamp storage. Industrial VCD files usually have strictly
/// uniform timestamp spacing (fixed simulator step), which compresses to
/// O(1) memory regardless of file size; non-uniform traces fall back to a
/// dense vector.
enum TsStore {
    Uniform { start: u64, step: u64, count: u64 },
    Dense(Vec<u64>),
}

impl TsStore {
    fn len(&self) -> usize {
        match self {
            TsStore::Uniform { count, .. } => *count as usize,
            TsStore::Dense(v) => v.len(),
        }
    }
    fn get(&self, i: usize) -> u64 {
        match self {
            TsStore::Uniform { start, step, .. } => start + (i as u64) * step,
            TsStore::Dense(v) => v[i],
        }
    }
    /// binary search; returns Ok(idx) on exact match, Err(insertion point)
    fn search(&self, t: u64) -> Result<usize, usize> {
        match self {
            TsStore::Uniform { start, step, count } => {
                if t < *start {
                    return Err(0);
                }
                let n = *count;
                let span = (t - start) / step;
                if span >= n {
                    return Err(n as usize);
                }
                if start + span * step == t {
                    Ok(span as usize)
                } else {
                    Err(span as usize + 1)
                }
            }
            TsStore::Dense(v) => v.binary_search(&t),
        }
    }
}

/// dump 区(时间戳之后)的索引。**懒构建**: `(load)` 只读文件头(scope/$var/
/// $dumpvars), 时间戳表/采样锚点/事件点在首次真正需要时按需构建。
///
/// 为什么懒: 构建索引要完整读一遍 dump 区。以前它在 load 里无条件跑, 于是
/// "一次查询"在冷盘上要读两遍文件(索引一遍 + 扫描一遍); 查询前能声明需要
/// 哪些信号(`prepare`)时, 变更列在同一次遍历里一起算出来 —— 冷启动只读一遍。
pub struct DumpIndex {
    /// 时间戳表(INDEX ↔ 时间值)
    timestamps: TsStore,
    /// 每 64 个时间戳取一个 '#' 行偏移(随机访问的锚点)
    timestamp_offsets: Vec<u64>,
    /// 每信号采样锚点 (ts, offset), 升序追加
    sparse_index: HashMap<u32, Vec<(u64, u64)>>,
    /// 事件信号(先写后清零)的变更索引
    event_change_points: HashMap<u32, Vec<usize>>,
}

/// 单个 chunk 的遍历产出:
/// (时间戳, 时间戳偏移, 采样锚点, 事件变更, 列缓存项, 查询目标变更)
type ChunkSweep = (
    Vec<u64>, Vec<u64>, Vec<(u32, u64, u64)>, Vec<(u32, u64)>,
    Vec<(u32, i64, u128, u64, u8)>, Vec<(u32, u32, VcdValue)>,
);

pub struct VcdTrace {
    id: TraceId,
    filename: String,
    /// 名字 arena: 所有全名连续存放(零 per-signal 分配;此前是 Vec<Arc<str>>)
    names_blob: Vec<u8>,
    /// 每信号: 低 32 位 = 名字在 arena 中的起始偏移, 高 32 位 = 字节长度
    name_meta: Vec<u64>,
    /// 名字 → 信号索引(开放寻址)
    name_index: OpenIndex,
    /// 信号 id 哈希 → 索引(开放寻址;键本身就是 64 位哈希, 与旧 HashMap 语义一致)
    signal_ids: OpenIndex,
    id_blob: Vec<u8>,          // concatenated signal ids (allocated once)
    id_offsets: Vec<u32>,      // per-signal start offset into id_blob
    signal_widths: Vec<u32>,               // dense by signal index (declared width)
    /// $dumpvars 初值: 紧凑存储(每信号 4B 偏移 + 少量 blob;此前是
    /// HashMap<u32, VcdValue> ≈ 40B/信号)。u32::MAX = 无初值(按 VCD 默认 'x')。
    init_off: Vec<u32>,
    init_blob: Vec<u8>,
    /// 短名/叶子名/子串解析缓存(一次 O(N) 扫描,之后 O(1));
    /// 所有读取路径共用,避免"op_get 能解析、边沿谓词不能"的不一致。
    name_cache: std::cell::RefCell<FxHashMap<String, Option<u32>>>,
    /// 已就"初值未定义(x)"告警过的信号(每个信号只提示一次)
    warned_xinit: std::cell::RefCell<std::collections::HashSet<u32>>,

    // Event signals (VCD event type — auto-reset to 0 at each timestamp boundary)
    event_signals: HashSet<u32>,
    /// dump 区索引(懒构建;见 DumpIndex 注释)
    dump_index: std::cell::OnceCell<DumpIndex>,
    /// 头解析时的时间戳数估计(仅用于预分配容量)
    est_ts: usize,

    // Pass 2: LRU cache
    lru_cache: RefCell<lru::LruCache<(u32, u64), VcdValue>>,
    signal_cache: Mutex<HashMap<u32, DecodedSignal>>,
    /// Budgeted in-memory columnar change lists built during load; covered
    /// signals answer queries with zero file re-scan (B/C 内存内核).
    col_cache: std::cell::RefCell<HashMap<u32, Col>>,
    /// Budget-exhausted signals get offsets-only columns (bytes pulled on
    /// demand from the mmap).
    off_cache: std::cell::RefCell<HashMap<u32, OffCol>>,

    // Persistent mmap
    reader: RefCell<crate::vcd::reader::MmapReader>,
    header_end_offset: u64,
    /// dump 区数据起点($dumpvars 块之后第一个 '#' 行 / 或头结束处)
    dump_data_start: u64,

    // Scopes
    scopes: Vec<String>,

    // Timescale exponent (10^n seconds), parsed from $timescale
    timescale_exp: Option<i8>,

    // Runtime
    current_index: usize,
}

impl VcdTrace {
    /// 边沿查询的数据质量提示: 初值未定义(x/z)时, 首次 x→已知 的跳变
    /// 不计入 rising/falling —— 用户很容易以为"少了一个边沿"是工具算错。
    /// 每个信号只提示一次。
    fn warn_undefined_initial(&self, sig_idx: u32) {
        if vcd_is_defined(&self.initial_value_at(sig_idx)) {
            return;
        }
        let mut w = self.warned_xinit.borrow_mut();
        if !w.insert(sig_idx) {
            return;
        }
        let name = self.name_at(sig_idx).to_string();
        eprintln!(
            "warning: 信号 '{}' 没有确定初值(x): 首个 x→1 / x→0 跳变不计入 rising/falling。\n\
         如需包含这次跳变请用 (changes \"{}\")，或让波形带上 $dumpvars 初值快照。",
            name, name
        );
    }

    /// 第 idx 个信号的全名(arena 视图, 零分配)
    #[inline]
    fn name_at(&self, idx: u32) -> &str {
        match self.name_meta.get(idx as usize) {
            Some(&m) => {
                let off = (m & 0xffff_ffff) as usize;
                let len = (m >> 32) as usize;
                std::str::from_utf8(self.names_blob.get(off..off + len).unwrap_or(&[])).unwrap_or("")
            }
            None => "",
        }
    }

    #[inline]
    fn name_count(&self) -> usize { self.name_meta.len() }

    /// 追加一个名字到 arena, 返回其索引(不建索引表)。
    /// 加载期 vector 尚未移入 self, 故以 (blob, meta) 为参数。
    #[inline]
    fn push_name(blob: &mut Vec<u8>, meta: &mut Vec<u64>, name: &[u8]) -> u32 {
        let idx = meta.len() as u32;
        let off = blob.len() as u64;
        blob.extend_from_slice(name);
        meta.push(off | ((name.len() as u64) << 32));
        idx
    }

    /// 解析信号名: 精确 → 叶子名(短名/无点) → 子串;结果缓存。
    /// 与 evaluator::resolve_signal_name 同口径;精确查找走 arena 索引。
    fn resolve_idx(&self, name: &str) -> Option<u32> {
        let h = hash_name(name.as_bytes());
        if let Some(i) = self.name_index.find_verified(h, |i| self.name_at(i) == name) {
            return Some(i);
        }
        if let Some(c) = self.name_cache.borrow().get(name) {
            return *c;
        }
        fn leaf(s: &str) -> &str { s.rsplitn(2, '.').next().unwrap_or("") }
        let mut hit = None;
        if name.len() <= 8 || !name.contains('.') {
            hit = (0..self.name_count() as u32).find(|&i| leaf(self.name_at(i)) == name);
        }
        if hit.is_none() {
            hit = (0..self.name_count() as u32).find(|&i| self.name_at(i).contains(name));
        }
        self.name_cache.borrow_mut().insert(name.to_string(), hit);
        hit
    }
    pub fn load(path: &Path, id: TraceId) -> Result<Self, String> {
        use rayon::prelude::*;

        let filename = path.to_string_lossy().to_string();
        let mut reader = crate::vcd::reader::MmapReader::new(path)
            .map_err(|e| format!("Failed to mmap {}: {}", filename, e))?;

        let file_len = reader.data_len();
        // 跨进程缓存(§8): 命中即跳过 PASS 1a/1b
        let cmode = cache_mode();
        if cmode != CacheMode::Off {
            if let Some(cp) = cache_file_for(path) {
                if let Some(t) = Self::try_load_cache(&cp, path, id.clone()) {
                    return Ok(t);
                }
            }
        }
        let est_ts = (file_len / 200).max(1024) as usize;
        // 预估信号数并一次性预留(4M 信号时反复扩容会吃掉一半时间)
        let est_vars = {
            let mmap = reader.data.clone();
            estimate_var_count(&mmap[..], 2usize << 30)
        };

        // ====== PASS 1a: Scan header (single-thread) ======
        let cap0 = est_vars.max(256);
        let mut names_blob: Vec<u8> = Vec::with_capacity(cap0 * 32);
        let mut name_meta: Vec<u64> = Vec::with_capacity(cap0);
        let mut name_index = OpenIndex::new(cap0);
        let mut signal_ids = OpenIndex::new(cap0);
        let mut id_blob: Vec<u8> = Vec::with_capacity(cap0 * 4);
        let mut id_offsets: Vec<u32> = Vec::with_capacity(cap0);
        let mut signal_widths: Vec<u32> = Vec::with_capacity(cap0);
        let mut event_signals: HashSet<u32> = HashSet::new();
        let mut header_end_offset: u64 = 0;

        // 复用缓冲: 每个信号的全名拼接 + 预拼 scope 前缀(避免 per-signal join)
        let mut name_buf = String::with_capacity(256);
        let mut scope_prefix = String::new();
        let mut scope_prefix_lens: Vec<usize> = Vec::new();

        // Track $scope / $upscope for hierarchical signal names
        let mut scope_stack: Vec<String> = Vec::new();
        let mut scopes: Vec<String> = Vec::new();
        let mut timescale_exp: Option<i8> = None;

        loop {
            let line_offset = reader.current_offset();
            let line = match reader.read_line_bytes() {
                Some(l) => l, None => break,
            };
            if line.is_empty() { continue; }
            // Verilator 等工具生成的 VCD header 行首带缩进空白（" $scope..."），
            // 先跳过空白再匹配 $ 指令，否则 header 全部被跳过导致 0 信号
            let mut ls = 0;
            while ls < line.len() && (line[ls] == b' ' || line[ls] == b'\t') {
                ls += 1;
            }
            let hl = &line[ls..];
            if !hl.is_empty() && hl[0] == b'$' {
                if hl.len() > 4 && hl[1] == b'v' && hl[2] == b'a' && hl[3] == b'r' {
                    if let Some((sig_hash, short_name, width, id_bytes, is_event)) = parse_var_decl_fast2(hl) {
                        let idx = name_meta.len() as u32;
                        // Build full hierarchical name: 复用 buffer + 预拼好的 scope 前缀
                        // (此前每个信号都 scope_stack.join(".") 一次, 深层次时是热点)
                        name_buf.clear();
                        if !scope_prefix.is_empty() {
                            name_buf.push_str(&scope_prefix);
                            name_buf.push('.');
                        }
                        name_buf.push_str(&String::from_utf8_lossy(short_name));
                        // arena 追加(零 per-signal 分配) + 开放寻址索引
                        Self::push_name(&mut names_blob, &mut name_meta, name_buf.as_bytes());
                        name_index.insert(hash_name(name_buf.as_bytes()), idx, |i| {
                            let m = name_meta[i as usize];
                            let o = (m & 0xffff_ffff) as usize;
                            let l = (m >> 32) as usize;
                            hash_name(&names_blob[o..o + l])
                        });
                        id_offsets.push(id_blob.len() as u32);
                        id_blob.extend_from_slice(id_bytes);
                        signal_ids.insert(sig_hash, idx, |_| 0);
                        if signal_widths.len() <= idx as usize {
                            signal_widths.resize(idx as usize + 1, 1);
                        }
                        signal_widths[idx as usize] = width as u32;
                        if is_event { event_signals.insert(idx); }
                    }
                } else if hl.starts_with(b"$scope") {
                    // $scope module name $end → push scope name
                    if let Some(scope_name) = parse_scope_name(hl) {
                        scope_prefix_lens.push(scope_prefix.len());
                        if !scope_prefix.is_empty() { scope_prefix.push('.'); }
                        scope_prefix.push_str(&scope_name);
                        scope_stack.push(scope_name);
                        scopes.push(scope_prefix.clone());
                    }
                } else if hl.starts_with(b"$timescale") {
                    // 单行: "$timescale 1ns $end"; 多行: "$timescale\n 1ns\n $end".
                    // 只有指令行本身无值(多行形态)才读下一行——单行解析失败时不得吞行。
                    timescale_exp = crate::vcd::convert::parse_timescale(hl);
                    let inline_value = hl.iter().any(|c| *c == b' ') || hl.len() > b"$timescale".len();
                    if !inline_value {
                        if let Some(next) = reader.read_line_bytes() {
                            timescale_exp = crate::vcd::convert::parse_timescale(next);
                        }
                    }
                } else if hl.starts_with(b"$upscope") {
                    scope_stack.pop();
                    if let Some(l) = scope_prefix_lens.pop() { scope_prefix.truncate(l); }
                } else if hl.starts_with(b"$enddefinitions") {
                    header_end_offset = line_offset + hl.len() as u64 + 1;
                    break;
                }
            }
        }

        // If no $enddefinitions found, use current position
        if header_end_offset == 0 {
            // 文件被截断(或不是 VCD): 明确告警, 否则用户拿到的是"部分信号/空结果"
            eprintln!(
                "warning: {}: 未找到 $enddefinitions — 文件可能被截断或不是完整 VCD(信号/时间戳可能不完整)",
                filename
            );
            header_end_offset = reader.current_offset();
        }

        // ====== PASS 1b: Parallel chunk scan of dump section ======
        let data = reader.data();
        // Sequential access hint: the scan touches every page of the dump once
        // — favors readahead over page-fault-by-page stalls (cold files).
        unsafe {
            libc::madvise(data.as_ptr() as *mut libc::c_void, data.len(), libc::MADV_SEQUENTIAL);
        }
        let dump_start = header_end_offset as usize;

        // Capture the $dumpvars snapshot (lines between header end and the
        // first '#' timestamp). These are the values held at t0 — used as the
        // initial value before any change; previously dropped, which let the
        // FST/VCD is-x (and get/at) initial semantics diverge (152GB §8.4 #1).
        let mut init_off: Vec<u32> = Vec::new();
        let mut init_blob: Vec<u8> = Vec::new();
        let mut pos = dump_start;
        {
            let mut line_start = dump_start;
            // 口径 A(与内测确认): t0 的整片初值 dump **永远是初值快照, 不是 INDEX**。
            // VCS 有两种渲染: ① `$dumpvars` 出现在任何 '#' 之前(e85 类);
            // ② 先来一个"空 #0", 紧跟 `$dumpvars`(mini 类)。两种都按快照处理,
            // 时间轴从第一个真正的变化步开始 —— 这样同一份仿真的 VCD/FST/FSDB
            // 给出同一个 INDEX 空间。只跳过"空 #0 + $dumpvars"这一种组合;
            // 若 #0 之后是真实值行(不是 dumpvars), 那它就是普通时间步, 不动。
            let mut allow_skip_leading_zero = true;
            while line_start < data.len() {
                let line_end = match memchr::memchr(b'\n', &data[line_start..]) {
                    Some(n) => line_start + n,
                    None => break,
                };
                let line = &data[line_start..line_end];
                // 空行/空白行不参与"首个 #0 能否跳过"的判定(mini.vcd 的 #0 前就有空行)
                if !line.is_empty() && line[0] == b'#' {
                    let next_is_dumpvars = {
                        let mut p2 = line_end + 1;
                        let mut found = false;
                        while p2 < data.len() {
                            let e2 = memchr::memchr(b'\n', &data[p2..]).map(|n| p2 + n).unwrap_or(data.len());
                            let l2 = &data[p2..e2];
                            if !l2.is_empty() {
                                found = l2.starts_with(b"$dumpvars");
                                break;
                            }
                            p2 = e2 + 1;
                        }
                        found
                    };
                    if allow_skip_leading_zero
                        && is_ts_line(line)
                        && parse_timestamp_fast(line) == 0
                        && next_is_dumpvars
                    {
                        allow_skip_leading_zero = false;
                        line_start = line_end + 1;
                        continue;
                    }
                    break; // 时间轴起点
                }
                if !line.is_empty() && line[0] != b'$' {
                    allow_skip_leading_zero = false; // 真值行 → 不再允许跳过首 #0
                }
                if !line.is_empty() && line[0] != b'$' {
                    // 值行的拆分规则与主扫描**同一套**(split_value_id):
                    // 空格/制表符分隔、无分隔 `b1010s1`、CRLF 都可识别。
                    // 旧实现按"试 id 长度 + 要求 id 前是空格"推断, 于是
                    // `$dumpvars` 里未分隔的向量行 `b1010s1` 全被拒 → 宽>1 的
                    // 纯初值信号读回来变 x(B10); 标量 `0!` 因 len==id_len+1 侥幸通过。
                    if let Some((value_part, idb)) = split_value_id(line) {
                        if let Some(sidx) = signal_ids.find(hash_sig_id(idb)) {
                            // 字节复核(哈希歧义/碰撞)
                            let id_ok = id_offsets.get(sidx as usize).map(|&st| {
                                let en = id_offsets.get(sidx as usize + 1)
                                    .map(|&v| v as usize).unwrap_or(id_blob.len());
                                id_blob.get(st as usize..en.min(id_blob.len())) == Some(idb)
                            }).unwrap_or(false);
                            if id_ok {
                                let val = vcd_value_of(value_part).unwrap_or(VcdValue::Bit(b'x'));
                                Self::set_initial(&mut init_off, &mut init_blob, sidx, &val);
                            }
                        }
                    }
                }
                line_start = line_end + 1;
            }
            pos = line_start;
        }

        let lru_cap = std::num::NonZeroUsize::new(adapt_lru_capacity(file_len, name_meta.len()))
            .unwrap_or(std::num::NonZeroUsize::MIN);
        let trace = VcdTrace {
            id, filename,
            names_blob, name_meta, name_index, signal_ids,
            id_blob, id_offsets, signal_widths, init_off, init_blob,
            name_cache: std::cell::RefCell::new(FxHashMap::default()),
            warned_xinit: std::cell::RefCell::new(std::collections::HashSet::new()),
            event_signals,
            dump_index: std::cell::OnceCell::new(),
            est_ts,
            lru_cache: RefCell::new(lru::LruCache::new(lru_cap)),
            signal_cache: Mutex::new(HashMap::new()),
            col_cache: std::cell::RefCell::new(HashMap::new()),
            off_cache: std::cell::RefCell::new(HashMap::new()),
            reader: RefCell::new(reader),
            header_end_offset,
            dump_data_start: pos as u64,
            scopes,
            timescale_exp,
            current_index: 0,
        };
        Ok(trace)
    }

    /// 构建 dump 区索引(时间戳表 + 采样锚点 + 事件点)。
    ///
    /// 由 `dump()` 懒触发 —— `(load)` 只读文件头($scope/$var/$dumpvars),
    /// 谁需要 dump 区数据谁付这一遍扫描; 跨进程缓存命中时整块跳过。
    fn build_dump_index(&self, want: &[u32]) -> DumpIndex {
        use rayon::prelude::*;
        let mmap = self.reader.borrow().data.clone();
        let data: &[u8] = &mmap[..];
        // Sequential access hint: the scan touches every page of the dump once
        // — favors readahead over page-fault-by-page stalls (cold files).
        unsafe {
            libc::madvise(data.as_ptr() as *mut libc::c_void, data.len(), libc::MADV_SEQUENTIAL);
        }
        let dump_start = self.header_end_offset as usize;
        let dump_len = data.len().saturating_sub(dump_start);
        let actual_start = (self.dump_data_start as usize).min(data.len());
        let est_ts = self.est_ts;
        let n_threads = num_cpus::get();
        let chunk_size = (dump_len / n_threads).max(1);
        // 查询目标信号: 同一次遍历里把它们的完整变更列一并算出来(零额外 IO)
        let want_ids: Vec<(u32, Vec<u8>)> = want.iter()
            .filter_map(|&sg| self.id_bytes(sg).map(|b| (sg, b.to_vec())))
            .collect();
        let mut want_changes: Vec<(u32, u32, VcdValue)> = Vec::new();

        // Find chunk boundaries at timestamp-line starts. A boundary must fall
        // exactly at a '#' line start (never inside a value block), so each
        // timestamp's value lines stay in the same chunk as their '#' line —
        // otherwise a '#' line straddling a boundary is counted twice (one
        // timestamp duplicated in the merged ts array).
        let mut boundaries = vec![actual_start];
        for i in 1..n_threads {
            let mut p = actual_start + i * chunk_size;
            if p >= data.len() {
                break; // no more chunks needed
            }
            while p < data.len() && data[p] != b'\n' {
                p += 1;
            }
            if p < data.len() { p += 1; } // skip newline
            if p >= data.len() { break; } // don't push past the end
            // advance to the next real '#' timestamp line start
            while p < data.len() {
                if data[p] == b'#' && is_ts_line_at(data, p) {
                    break;
                }
                match memchr::memchr(b'\n', &data[p..]) {
                    Some(n) => p += n + 1,
                    None => { p = data.len(); break; }
                }
            }
            if p < data.len() { boundaries.push(p); }
        }
        boundaries.push(data.len());

        // Shared read-only data for parallel threads
        let col_enabled = col_cache_budget() > 0;
        let has_events = !self.event_signals.is_empty();
        let sid_all = &self.signal_ids;
        let widths_all = &self.signal_widths;
        let event_all = &self.event_signals;

        // Parallel processing
        let sparse_interval: u64 = 100;
        // Chunk-local flat triples (sig, ts, offset) — sorted by construction
        // (file order); merged per signal in chunk order, no per-signal trees.
        // Stream in batches: each batch's whole-column output is merged into
        // the final maps BEFORE the next batch is collected — transient chunk
        // memory stays bounded (~1-2GB) instead of the whole dump.
        // ====== MERGE STATE (spans all batches) ======
        let mut timestamps: Vec<u64> = Vec::with_capacity(est_ts);
        let mut timestamp_offsets: Vec<u64> = Vec::with_capacity(est_ts);
        let mut event_change_ts: HashMap<u32, Vec<u64>> = HashMap::new();
        let mut sparse_index: HashMap<u32, Vec<(u64, u64)>> = HashMap::with_capacity(128);
        let col_budget = col_cache_budget();
        let mut col_bytes: usize = 0;
        let mut col_cache: HashMap<u32, Col> = self.col_cache.take();
        let mut off_cache: HashMap<u32, OffCol> = self.off_cache.take();
        let mut col_ts_base: usize = 0;
        let mut col_ts_next: usize = 0;

        const COL_BATCH: usize = 8;
        let stream: Vec<&[usize]> = boundaries.windows(2).collect();
        let batch_list: Vec<Vec<&[usize]>> = stream.chunks(COL_BATCH).map(|c| c.to_vec()).collect();
        for batch in batch_list {
            let all: Vec<ChunkSweep> = batch
                .par_iter()
            .map(|w| {
                let chunk_start = w[0];
                let chunk_end = w[1];
                let chunk = &data[chunk_start..chunk_end];
                let sid = sid_all;
                let evt = event_all;
                let widths = widths_all;
                let col_on = col_enabled;
                // 冷文件: 先让内核按大块预读本 chunk(逐页 4KB 缺页在 58.7GB 上
                // 会比 1MB 顺序读多花一倍 sys 时间)。分窗口调用, 避免一次排队过多。
                unsafe {
                    const WIN: usize = 64 << 20;
                    let mut off = 0usize;
                    while off < chunk.len() {
                        let n = (chunk.len() - off).min(WIN);
                        libc::madvise(chunk[off..].as_ptr() as *mut libc::c_void, n,
                                      libc::MADV_WILLNEED);
                        off += n;
                    }
                }

                let mut ts = Vec::new();
                let mut ts_offsets = Vec::new();
                let mut si: Vec<(u32, u64, u64)> = Vec::new();
                let mut cols: Vec<(u32, i64, u128, u64, u8)> = Vec::new();
                let mut event_cp: Vec<(u32, u64)> = Vec::new();
                // 查询目标信号的变更(本块内 ts 序号; u32::MAX = 属于上一块最后一个 ts)
                let mut want_hits: Vec<(u32, u32, VcdValue)> = Vec::new();
                let mut current_timestamp: u64 = 0;
                let base_offset = chunk_start as u64;
                let mut ts_seen: i64 = 0;
                // Sparse anchors are sampled GLOBALLY (every sparse_interval-th
                // value line) — anchor density per signal unchanged. Every
                // value line is still parsed ONCE: it also feeds the budgeted
                // in-memory columnar change lists (col cache).
                let mut anchor_counter: u64 = 0;

                let mut lp = 0usize;
                while lp < chunk.len() {
                    let line_start = lp;
                    let line_end = match memchr::memchr(b'\n', &chunk[lp..]) {
                        Some(nl_pos) => {
                            lp += nl_pos;
                            let end = lp;
                            lp += 1; // skip newline
                            end
                        }
                    None => { break; }
                    };
                    let line = &chunk[line_start..line_end];
                    if line.is_empty() { continue; }

                    let first = line[0];
                    match first {
                        b'#' => {
                            // only real timestamp lines: '#<digits>'. Icarus dumps
                            // 1-bit value lines like "#%" (value '#' + id) which
                            // start with '#' but are NOT timestamps. 走共享判定
                            // (会剥掉 CRLF 的 \r / \n)。
                            if is_ts_line(line) {
                                current_timestamp = parse_timestamp_fast(line);
                                ts_seen += 1;
                                ts.push(current_timestamp);
                                ts_offsets.push(base_offset + line_start as u64);
                            }
                        }
                        b'$' => {}
                        _ => {
                            anchor_counter += 1;
                            let sampled = anchor_counter % sparse_interval == 0;
                            // Column cache off → parse only the sampled fraction
                            // (fast load, 0.12.2 speed); on → parse every line
                            // once for anchors AND the columnar change list.
                            // 查询目标信号的行无论采样与否都要处理 —— 索引与变更列
                            // 在同一次遍历里算出来, 冷启动不再多读一遍文件。
                            let hot = has_events || col_on || sampled;
                            if !want_ids.is_empty() {
                                // 查询目标的**廉价预筛**: 先比行尾字节, 再比 <id> 结尾与
                                // 分隔符 —— 命中才做完整拆分。逐行只要一两次比较就能滤掉
                                // 绝大部分行(否则每行一次反向找分隔符 + 拆分, 983MB 真实
                                // dump 上从 0.25s 退化到 0.95s)。
                                let body = match line.last() {
                                    Some(b'\r') => &line[..line.len() - 1],
                                    _ => line,
                                };
                                if let Some(&tail) = body.last() {
                                    for (wsig, wid) in &want_ids {
                                        if wid.last() != Some(&tail) || !body.ends_with(wid.as_slice()) {
                                            continue;
                                        }
                                        let n = body.len() - wid.len();
                                        if n > 0 {
                                            let sep = body[n - 1];
                                            if sep != b' ' && sep != b'\t'
                                                && !matches!(sep, b'0' | b'1' | b'x' | b'X' | b'z' | b'Z') {
                                                continue;
                                            }
                                        }
                                        // 预筛只是"可能命中"; 真正判定必须用解析出的 ID
                                        // (行 `1 11` 以 '1' 结尾且前一字符是值字符 '1', 但它的
                                        //  ID 是 `11` —— 少这一次比较就会把别的信号记成目标信号)。
                                        if let Some((value_part, idb)) = split_value_id(line) {
                                            if idb == wid.as_slice() {
                                                let ts_local = if ts_seen > 0 { (ts_seen - 1) as u32 } else { u32::MAX };
                                                want_hits.push((*wsig, ts_local,
                                                    vcd_value_of(value_part).unwrap_or(VcdValue::Bit(b'x'))));
                                            }
                                        }
                                        break;
                                    }
                                }
                            }
                            if hot {
                                if let Some((value_part, idb)) = split_value_id(line) {
                                    {
                                        let value = match vcd_value_of(value_part) { Some(v) => v, None => continue };
                                        let sig_hash = hash_sig_id(idb);
                                        if let Some(sig_idx) = sid.find(sig_hash) {
                                            if sampled {
                                                si.push((sig_idx, current_timestamp, base_offset + line_start as u64));
                                            }
                                            if has_events && evt.contains(&sig_idx) {
                                                event_cp.push((sig_idx, current_timestamp));
                                            }
                                            if col_on {
                                                let vw = match &value {
                                                    VcdValue::Vector(v) => v.len(),
                                                    VcdValue::Bit(_) => 1,
                                                    _ => 0,
                                                };
                                                if vw >= 1 && vw <= 64 {
                                                    if let Some(st) = col_states_from_vcd(&value, vw) {
                                                        cols.push((sig_idx, ts_seen - 1, st, base_offset + line_start as u64, vw as u8));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // Group per signal so the merge concatenates spans instead of
                // HashMap-probing per entry (load cost for the column cache).
                if std::env::var("WAL_NO_SORT").is_ok() {
                    // bisect knob
                } else {
                    // stable by (sig, local_ts): same-(sig,ts) delta writes keep
                    // their FILE order so the merge's last-write-wins dedupe is
                    // correct (the intermediate/final order must be preserved).
                    cols.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
                }
                (ts, ts_offsets, si, event_cp, cols, want_hits)
            })
            .collect();

        if std::env::var("WAL_DEBUG_FIND").is_ok() {
            for (i, r) in all.iter().enumerate() {
                eprintln!("  pass1b chunk{}: ts={:?} offs0={}",
                          i, r.0.iter().take(3).collect::<Vec<_>>(),
                          r.1.first().copied().unwrap_or(0));
            }
        }

        for (chunk_ts, chunk_offsets, chunk_si, chunk_evt, chunk_cols, chunk_want) in all {
            col_ts_next = col_ts_base + chunk_ts.len();
            for (wsig, local_ts, val) in chunk_want {
                // 本块首个 '#' 之前的行属于上一块最后一个时间戳(块 0 = $dumpvars 块 → 跳过)
                let gts = if local_ts == u32::MAX {
                    if col_ts_base == 0 { continue; }
                    (col_ts_base - 1) as u32
                } else {
                    (col_ts_base + local_ts as usize) as u32
                };
                match want_changes.last_mut() {
                    Some((s0, i0, v0)) if *s0 == wsig && *i0 == gts => *v0 = val,
                    _ => want_changes.push((wsig, gts, val)),
                }
            }
            timestamps.extend(chunk_ts);
            timestamp_offsets.extend(chunk_offsets);
            for (sig_idx, s_ts, s_off) in chunk_si {
                sparse_index.entry(sig_idx).or_default().push((s_ts, s_off));
            }
            for (sig_idx, ts) in chunk_evt {
                event_change_ts.entry(sig_idx).or_default().push(ts);
            }
            let mut i = 0usize;
            while i < chunk_cols.len() {
                let sig_idx = chunk_cols[i].0;
                let mut j = i;
                while j < chunk_cols.len() && chunk_cols[j].0 == sig_idx {
                    j += 1;
                }
                // Offsets index (budget-exhausted signals): still full coverage.
                _ = &sig_idx;
                // -1 before the chunk's first '#': the previous chunk's last
                // timestamp (chunk 0's -1 = the $dumpvars block → skip).
                let gts_base = col_ts_base as i64 + chunk_cols[i].1;
                let present = gts_base >= 0;
                if present {
                    if let Some(col) = col_cache.get_mut(&sig_idx) {
                        for (_, local_ts, st, _off, _vw) in &chunk_cols[i..j] {
                            let gts = (col_ts_base as i64 + local_ts) as u32;
                            if col.idxs.last() == Some(&gts) {
                                *col.states.last_mut().unwrap() = *st; // last write wins
                            } else {
                                col.idxs.push(gts);
                                col.states.push(*st);
                                col_bytes += 20;
                            }
                        }
                    } else if col_bytes < col_budget {
                        let w = chunk_cols[i].4 as usize;
                        let mut col = Col { width: w, idxs: Vec::with_capacity(j - i), states: Vec::with_capacity(j - i) };
                        for (_, local_ts, st, off, _vw) in &chunk_cols[i..j] {
                            let gts = (col_ts_base as i64 + local_ts) as u32;
                            col.idxs.push(gts);
                            col.states.push(*st);
                            col_bytes += 20;
                        }
                        col_cache.insert(sig_idx, col);
                    } else if std::env::var("WAL_NO_OFF").is_ok() {
                        // bisect knob: skip offsets columns entirely
                    } else if let Some(offcol) = off_cache.get_mut(&sig_idx) {
                        for (_, local_ts, _st, offv, _vw) in &chunk_cols[i..j] {
                            let gts = (col_ts_base as i64 + local_ts) as u32;
                            if offcol.idxs.last() == Some(&gts) {
                                *offcol.offsets.last_mut().unwrap() = *offv;
                            } else {
                                offcol.idxs.push(gts);
                                offcol.offsets.push(*offv);
                            }
                        }
                    } else if col_bytes < col_budget * 2 {
                        // value-column budget exhausted → offsets-only index
                        let mut offcol = OffCol { idxs: Vec::with_capacity(j - i), offsets: Vec::with_capacity(j - i) };
                        for (_, local_ts, _st, offv, _vw) in &chunk_cols[i..j] {
                            let gts = (col_ts_base as i64 + local_ts) as u32;
                            offcol.idxs.push(gts);
                            offcol.offsets.push(*offv);
                        }
                        off_cache.insert(sig_idx, offcol);
                    }
                    // else: no index at all — anchored scan for this signal
                }
                i = j;
            }
            col_ts_base = col_ts_next;
        }
        }

        // Convert event timestamps to sequential INDEX using sorted timestamps
        let mut event_change_points: HashMap<u32, Vec<usize>> = HashMap::with_capacity(event_change_ts.len());
        for (sig_idx, ts_list) in &event_change_ts {
            let mut indices = Vec::with_capacity(ts_list.len());
            let mut ti = 0usize;
            for ts in ts_list {
                while ti < timestamps.len() && timestamps[ti] < *ts { ti += 1; }
                if ti < timestamps.len() && timestamps[ti] == *ts {
                    indices.push(ti);
                }
            }
            event_change_points.insert(*sig_idx, indices);
        }

        // Sample offsets: keep every 64th. read_signal_value_at locates exact
        // '#' lines with a short memchr scan from the sampled anchor.
        let timestamp_offsets: Vec<u64> = timestamp_offsets.iter().step_by(64).copied().collect();
        // Release file pages touched by the full PASS-1b scan: queries re-read
        // on demand. Keeps steady-state RSS at heap size, not file size.
        unsafe {
            libc::madvise(data.as_ptr() as *mut libc::c_void, data.len(), libc::MADV_DONTNEED);
        }
        let _ = self.reader.borrow_mut().seek_to(0);

        if std::env::var("WAL_DEBUG_FIND").is_ok() {
            let sparse_entries: usize = sparse_index.values().map(|m| m.len()).sum();
            let event_entries: usize = event_change_points.values().map(|v| v.len()).sum();
            eprintln!("load: ts={} offsets={} sparse_entries={} event_entries={} signals={}",
                      timestamps.len(), timestamp_offsets.len(), sparse_entries, event_entries, self.name_count());
        }

        // 目标信号的变更列: 写进 signal_cache(同进程复用) + 旁挂列缓存(跨进程)
        if !want_changes.is_empty() {
            let mut by_sig: HashMap<u32, Vec<(u32, VcdValue)>> = HashMap::new();
            for (wsig, gts, val) in want_changes {
                by_sig.entry(wsig).or_default().push((gts, val));
            }
            for (wsig, all) in by_sig {
                if all.len() > 1 {
                    self.try_cache_decoded_signal(wsig, &all, CacheType::FullScan);
                    self.save_col_sidecar(wsig, &all);
                }
            }
        }
        let dump = DumpIndex {
            timestamps: build_ts_store(timestamps),
            timestamp_offsets,
            sparse_index,
            event_change_points,
        };
        // 跨进程索引缓存写回(auto 只对大波形: 小文件本来就是毫秒级)
        let should_write = match cache_mode() {
            CacheMode::Build => true,
            CacheMode::Auto => (self.reader.borrow().data_len() as u64) >= cache_min_bytes(),
            _ => false,
        };
        if should_write {
            if let Some(cp) = cache_file_for(std::path::Path::new(&self.filename)) {
                self.save_cache(&cp, &dump);
            }
        }
        dump
    }


    /// dump 区索引(懒构建)。**这是唯一入口** —— 所有需要时间戳表/锚点/事件点的
    /// 路径都经它, 保证只有一套实现、按需付费。
    #[inline]
    fn dump(&self) -> &DumpIndex {
        self.dump_index.get_or_init(|| self.build_dump_index(&[]))
    }

    /// 需要 dump 区数据、且"顺带想要这些信号的变更列"时用这个入口:
    /// 索引还没建 → 同一次遍历里把变更列一起算出来(写进 signal_cache);
    /// 索引已建(或已被别人用别的 want 建过) → 直接返回, 变更列走常规冷扫描。
    fn ensure_dump_index(&self, want: &[u32]) -> &DumpIndex {
        if let Some(d) = self.dump_index.get() { return d; }
        self.dump_index.get_or_init(|| self.build_dump_index(want))
    }

    /// 查询前置声明: 告诉 trace 这次查询要碰哪些信号(名字可解析任意一条 trace 的
    /// 命名规则)。默认空实现 —— 只有能"顺手算出来"的后端需要。
    pub fn prepare_names(&self, names: &[String]) {
        if self.dump_index.get().is_some() || names.is_empty() { return; }
        let mut want: Vec<u32> = Vec::new();
        for n in names {
            if let Some(i) = self.name_index.find_verified(hash_name(n.as_bytes()), |i| self.name_at(i) == n.as_str()) {
                want.push(i);
            }
        }
        if !want.is_empty() { let _ = self.ensure_dump_index(&want); }
    }

    /// Find the index of a given timestamp (returns nearest <= target)
    #[allow(dead_code)]
    fn find_timestamp_index(&self, target: u64) -> usize {
        match self.dump().timestamps.search(target) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        }
    }

    /// Sampled offset anchor for a timestamp index: the '#' line of the
    /// timestamp at (idx >> 6) * 64 (<= idx). Exact lines are located by a
    /// short memchr scan from this anchor.
    fn ts_line_anchor(&self, ts_idx: usize) -> u64 {
        let d = self.dump();
        let b = ts_idx >> 6;
        if b < d.timestamp_offsets.len() {
            d.timestamp_offsets[b]
        } else {
            *d.timestamp_offsets.last().unwrap_or(&(self.header_end_offset as u64))
        }
    }
    /// Sampled offset anchor strictly after a timestamp index (next sample).
    /// Returns u64::MAX when past the last sample (caller uses file end).
    fn ts_end_anchor(&self, ts_idx: usize) -> u64 {
        let d = self.dump();
        let b = (ts_idx >> 6) + 1;
        if b < d.timestamp_offsets.len() {
            d.timestamp_offsets[b]
        } else {
            u64::MAX
        }
    }

    /// On-demand: read signal value at a specific timestamp (memchr jump scan + sampled offsets)
    /// Signal id bytes for a signal index (blob + offsets: one allocation).
    #[inline]
    fn id_bytes(&self, sig_idx: u32) -> Option<&[u8]> {
        let start = *self.id_offsets.get(sig_idx as usize)? as usize;
        let end = self.id_offsets.get(sig_idx as usize + 1).map(|v| *v as usize).unwrap_or(self.id_blob.len());
        if start > self.id_blob.len() { return None; }
        Some(&self.id_blob[start..end.min(self.id_blob.len())])
    }

    /// Initial value of a signal: $dumpvars snapshot if present, else 'x'
    /// (the VCD default for values that never got an explicit write).
    #[inline]
    fn initial_value_at(&self, sig_idx: u32) -> VcdValue {
        match self.init_off.get(sig_idx as usize) {
            Some(&off) if off != u32::MAX => decode_initial(&self.init_blob[off as usize..]),
            _ => VcdValue::Bit(b'x'),
        }
    }

    /// 记录某信号的 $dumpvars 初值(加载期与缓存恢复共用)
    fn set_initial(off: &mut Vec<u32>, blob: &mut Vec<u8>, sig_idx: u32, val: &VcdValue) {
        let i = sig_idx as usize;
        if off.len() <= i {
            off.resize(i + 1, u32::MAX);
        }
        let at = blob.len() as u32;
        encode_initial(blob, val);
        off[i] = at;
    }

    /// 该信号是否带显式初值(缓存序列化用)
    fn has_initial(&self, sig_idx: u32) -> bool {
        matches!(self.init_off.get(sig_idx as usize), Some(&o) if o != u32::MAX)
    }

    /// Last sparse anchor with ts ≤ target (entries are appended in ascending
    /// ts order — binary search instead of BTreeMap range probes).
    fn sparse_anchor(&self, sig_idx: u32, target_ts: u64) -> Option<(u64, u64)> {
        let v = self.dump().sparse_index.get(&sig_idx)?;
        let idx = v.partition_point(|(ts, _)| *ts <= target_ts);
        if idx == 0 { None } else { Some(v[idx - 1]) }
    }

    /// Per-index change list for a signal — cached warm or id-anchored
    /// cold scan. Single source for find_indices AND change_points.
    fn anchored_changes(&self, sig_idx: u32) -> Result<Vec<(u32, VcdValue)>, String> {
        // 同进程重复查询(以及 signal_value 已解码过的信号): 直接复用已解码变更列。
        // 此前只有 signal_value 读 signal_cache, find_indices/change_points 每次
        // 都重新做锚定扫描(58.7GB 上第二次同信号查询仍要 ~85s)。
        if let Some(ds) = self.signal_cache.lock().unwrap().get(&sig_idx) {
            if ds.full_scan {
                return Ok(ds.change_indices.iter().copied()
                    .zip(ds.values.iter().cloned())
                    .collect());
            }
        }
        // 跨进程旁挂列缓存(首次冷扫描已落盘)→ 直接命中, 不碰文件
        if let Some(all) = self.try_load_col_sidecar(sig_idx) {
            if all.len() > 1 {
                self.try_cache_decoded_signal(sig_idx, &all, CacheType::FullScan);
            }
            return Ok(all);
        }
        // In-memory column (built during load) — zero file re-scan.
        let col_ref = self.col_cache.borrow();
        if let Some(col) = col_ref.get(&sig_idx) {
            let mut out = Vec::with_capacity(col.idxs.len());
            for (i, st) in col.states.iter().enumerate() {
                out.push((col.idxs[i], vcd_from_states(*st, col.width)));
            }
            if std::env::var("WAL_DEBUG_FIND").is_ok() {
                eprintln!("column: sig={} n={} first5={:?}", sig_idx, out.len(),
                    out.iter().take(5).map(|(i, v)| (*i, format!("{:?}", v))).collect::<Vec<_>>());
            }
            if out.len() > 1 {
                self.try_cache_decoded_signal(sig_idx, &out, CacheType::FullScan);
                self.save_col_sidecar(sig_idx, &out);
            }
            return Ok(out);
        }
        // Offsets-only column: walk ts indices, pull the value lines from the
        // mmap on demand (DuckDB-style byte-range reads — a few KB per signal).
        let off_ref = self.off_cache.borrow();
        if let Some(off) = off_ref.get(&sig_idx) {
            let mmap = self.reader.borrow().data.clone();
            let data: &[u8] = &mmap[..];
            let mut out = Vec::with_capacity(off.idxs.len());
            for k in 0..off.idxs.len() {
                let pos = off.offsets[k] as usize;
                if pos >= data.len() { continue; }
                let line_start = match memchr::memrchr(b'\n', &data[..pos]) {
                    Some(n) => n + 1,
                    None => 0,
                };
                let line_end = match memchr::memchr(b'\n', &data[pos..]) {
                    Some(n) => pos + n,
                    None => data.len(),
                };
                if let Some((_, val)) = parse_value_change_fast(&data[line_start..line_end]) {
                    out.push((off.idxs[k], val));
                }
            }
            if out.len() > 1 {
                self.try_cache_decoded_signal(sig_idx, &out, CacheType::FullScan);
                self.save_col_sidecar(sig_idx, &out);
            }
            return Ok(out);
        }
        // 冷扫描(该信号没有任何索引): **单遍逐行**, 与加载期同一套行循环。
        //
        // 旧实现每 chunk 走两遍: ① memchr('#') 收时间戳位置 ② memmem(id) 找变更行。
        // ② 对"ID 字符在数据里高频出现"的信号是灾难: 合成夹具里 s0 的 VCD ID 是
        // `1`, 而二进制值行全是 0/1 —— memmem 每命中一次只前进 1 字节, 11.5GB
        // 上 s0 的扫描烧掉 265 CPU·s, 同文件 s1(ID `2`)只要 ~0。逐行单遍把
        // 代价变成 O(行数)(与 ID 内容无关), 同时省掉 ① 的一整遍 IO。
        let target_id = self.id_bytes(sig_idx)
            .ok_or_else(|| "find_indices: signal ID bytes not found".to_string())?;
        use rayon::prelude::*;
        let shared_mmap = self.reader.borrow().data.clone();
        // 起点用 dump_data_start(规则 A 之后它 = 第一个真正的时间步):
        // 用 header_end_offset 会把被跳过的 "#0 + $dumpvars" 那一段当成时间步,
        // 于是冷扫描自己数出 12 个时间戳、而索引表是 11 个 → 索引整体错位一格。
        let hdr_end = self.dump_data_start as usize;
        let data_len = shared_mmap.len();
        let n_threads = num_cpus::get().max(4);
        let chunk_size = (data_len.saturating_sub(hdr_end) / n_threads.max(1)).max(64 * 1024);

        // 换行对齐的分块边界(值行不会跨块; 块首值行的时间戳归属见下)
        let mut boundaries = vec![hdr_end];
        for i in 1..n_threads {
            let mut p = hdr_end + i * chunk_size;
            if p >= data_len { break; }
            match memchr::memchr(b'\n', &shared_mmap[p..data_len]) {
                Some(n) => p += n + 1,
                None => break,
            }
            if p >= data_len { break; }
            boundaries.push(p);
        }
        boundaries.push(data_len);

        // (本块 '#' 行数, 命中: (块内 ts 序号或 u32::MAX, pos, value))
        let results: Vec<(usize, Vec<(u32, usize, VcdValue)>)> = boundaries[..boundaries.len() - 1]
            .par_iter()
            .enumerate()
            .map(|(ci, &start)| {
                let end = boundaries[ci + 1];
                let chunk = &shared_mmap[start..end];
                let mut hits: Vec<(u32, usize, VcdValue)> = Vec::new();
                let mut ts_count = 0usize;
                // 本块内最后一个 '#' 的序号; u32::MAX = 本块还没有 '(该行属于上一块)
                let mut cur_ts = u32::MAX;
                let mut p = 0usize;
                while p < chunk.len() {
                    let line_start = p;
                    let line_end = match memchr::memchr(b'\n', &chunk[p..]) {
                        Some(n) => p + n,
                        None => chunk.len(),
                    };
                    let line = &chunk[line_start..line_end];
                    p = line_end + 1;
                    match line.first() {
                        None => {}
                        Some(b'#') => {
                            if is_ts_line(line) {
                                ts_count += 1;
                                cur_ts = (ts_count - 1) as u32;
                            }
                        }
                        Some(b'$') => {}
                        Some(_) => {
                            if let Some((value_part, id)) = split_value_id(line) {
                                if id == target_id {
                                    // 取值与 parse_value_change_fast 同一套规则
                                    let vfirst = value_part[0];
                                    let value = match vfirst {
                                        b'b' => VcdValue::Vector(value_part[1..].to_vec()),
                                        b'r' => match std::str::from_utf8(&value_part[1..])
                                            .unwrap_or("0").trim().parse::<f64>()
                                        {
                                            Ok(r) => VcdValue::Real(r),
                                            Err(_) => VcdValue::Bit(b'x'),
                                        },
                                        other => VcdValue::Bit(other),
                                    };
                                    hits.push((cur_ts, line_start, value));
                                }
                            }
                        }
                    }
                }
                (ts_count, hits)
            })
            .collect();

        // 按块序合并(块内已是文件序); 块内首个 '#' 之前的值行属于上一块的最后一个
        // 时间戳(u32::MAX); 整个文件首个 '#' 之前 = $dumpvars 块(已作为初值, 跳过)。
        // 同一时间戳多次写入折叠为最后一次(last write wins)。
        let mut all: Vec<(u32, VcdValue)> = Vec::new();
        {
            let mut base = 0usize;
            for (ts_count, hits) in results {
                for (local_idx, _pos, val) in hits {
                    let ts_idx = if local_idx == u32::MAX {
                        if base == 0 { continue; }
                        base - 1
                    } else {
                        base + local_idx as usize
                    };
                    match all.last_mut() {
                        Some((i, v)) if *i as usize == ts_idx => *v = val,
                        _ => all.push((ts_idx as u32, val)),
                    }
                }
                base += ts_count;
            }
        }

        if std::env::var("WAL_DEBUG_FIND").is_ok() {
            eprintln!("anchored: sig={} id={:?} all={:?} init={:?}", sig_idx, target_id, all, self.initial_value_at(sig_idx));
        }
        // Cache the change list for warm queries / signal_value.
        if all.len() > 1 {
            self.try_cache_decoded_signal(sig_idx, &all, CacheType::FullScan);
            self.save_col_sidecar(sig_idx, &all);
        }
        Ok(all)
    }



    fn read_signal_value_at(&self, sig_idx: u32, target_timestamp: u64) -> VcdValue {
        let target_id = match self.id_bytes(sig_idx) {
            Some(id) => id,
            None => return VcdValue::Bit(b'x'),
        };
        let id_len = target_id.len();
        let is_event = self.event_signals.contains(&sig_idx);

        // 1. Target index and scan end
        let target_idx = match self.dump().timestamps.search(target_timestamp) {
            Ok(i) => i,
            Err(0) => return VcdValue::Bit(b'x'),
            Err(i) => i - 1,
        };

        // 2. Scan start: anchor TIMESTAMP's # line offset (not change point offset).
        //    This guarantees we start at a # line, not in the middle of value changes.
        if std::env::var("WAL_DEBUG_FIND").is_ok() {
            eprintln!("read_signal_value_at: sig={} target_ts={} target_idx={}", sig_idx, target_timestamp, target_idx);
        }
        let sparse_anchor = self.sparse_anchor(sig_idx, target_timestamp);
        let scan_start = sparse_anchor
            .and_then(|(ts, _)| self.dump().timestamps.search(ts).ok())
            .map(|i| self.ts_line_anchor(i))
            .unwrap_or(self.header_end_offset) as usize;
        if std::env::var("WAL_DEBUG_FIND").is_ok() {
            eprintln!("  sparse_anchor={:?} scan_start={} scan_end={}",
                sparse_anchor, scan_start,
                self.ts_end_anchor(target_idx + 1));
        }
        if std::env::var("WAL_DEBUG_FIND").is_ok() && sparse_anchor.is_none() {
            eprintln!("no sparse anchor for sig={} ts={}", sig_idx, target_timestamp);
        }

        let next_ts_idx = target_idx + 1;
        let mmap = self.reader.borrow().data.clone();
        let data = &mmap[..];
        let scan_end = if next_ts_idx < self.dump().timestamps.len() {
            let e = self.ts_end_anchor(next_ts_idx);
            if e == u64::MAX { data.len() } else { e as usize }
        } else {
            data.len()
        };

        if std::env::var("WAL_DEBUG_FIND").is_ok() && scan_end.saturating_sub(scan_start) > 1_000_000 {
            eprintln!("read region huge: sig={} ts={} scan_start={} scan_end={} region={}",
                      sig_idx, target_timestamp, scan_start, scan_end,
                      scan_end.saturating_sub(scan_start));
        }
        // Fallback for huge sparse gaps
        if scan_start >= data.len() || scan_start >= scan_end || scan_end.saturating_sub(scan_start) > 1_000_000_000 {
            if std::env::var("WAL_DEBUG_FIND").is_ok() {
                static LEGACY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let n = LEGACY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if n < 10 {
                    eprintln!("read legacy fallback: sig={} scan_start={} scan_end={} len={}",
                              sig_idx, scan_start, scan_end, data.len());
                }
            }
            return self.read_signal_value_at_legacy(sig_idx, target_timestamp);
        }

        // 3. memchr jump scan + collect changes for cache
        let region = &data[scan_start..scan_end];
        let mut last_value: Option<VcdValue> = None;
        let mut pos: usize = 0;

        while pos < region.len() {
            // Find next '#' at line start (preceded by \n or at region start)
            let ts_start = loop {
                let p = match memchr::memchr(b'#', &region[pos..]) {
                    Some(p) => p,
                    None => break None,
                };
                let abs_p = pos + p;
                if abs_p == 0 || region[abs_p - 1] == b'\n' {
                    break Some(abs_p);
                }
                pos = abs_p + 1;
            };
            let ts_start = match ts_start { Some(p) => p, None => break };

            // Find next '#' at line start for block end
            let mut search_pos = ts_start + 1;
            let next_ts = loop {
                let p = match memchr::memchr(b'#', &region[search_pos..]) {
                    Some(p) => search_pos + p,
                    None => break region.len(),
                };
                if region[p - 1] == b'\n' {
                    break p;
                }
                search_pos = p + 1;
            };

            let ts = parse_timestamp_fast(&region[ts_start..next_ts]);
            if ts > target_timestamp { break; }

            if is_event {
                last_value = Some(VcdValue::Bit(b'0'));
            }

            let value_block = &region[ts_start + 1..next_ts];
            if !value_block.is_empty() {
                if let Some(val) = find_signal_in_block(value_block, target_id, id_len) {
                    last_value = Some(val);
                }
            }

            pos = next_ts;
        }

        last_value.unwrap_or_else(|| self.initial_value_at(sig_idx))
    }

    fn read_signal_value_at_legacy(&self, sig_idx: u32, target_timestamp: u64) -> VcdValue {
        let target_id = match self.id_bytes(sig_idx) {
            Some(id) => id,
            None => return VcdValue::Bit(b'x'),
        };
        let id_len = target_id.len();
        let mut reader = self.reader.borrow_mut();

        let start_offset = self
            .sparse_anchor(sig_idx, target_timestamp)
            .map(|(_, off)| off)
            .unwrap_or(self.header_end_offset);

        let _ = reader.seek_to(start_offset);
        let mut last_value: Option<VcdValue> = None;

        loop {
            let line = match reader.read_line_bytes() {
                Some(l) => l,
                None => break,
            };
            if line.is_empty() { continue; }
            let first = line[0];

            if first == b'#' && is_ts_line(line) {
                let ts = parse_timestamp_fast(line);
                if ts > target_timestamp { break; }
                if self.event_signals.contains(&sig_idx) {
                    last_value = Some(VcdValue::Bit(b'0'));
                }
            } else if first != b'$' && line.len() > id_len {
                let id_start = line.len() - id_len;
                if (line.len() == id_len + 1 || (id_start > 0 && line[id_start - 1] == b' ')) && &line[id_start..] == target_id {
                    let val = match first {
                        b'b' => {
                            let ve = id_start.saturating_sub(1);
                            let vs = if ve > 1 && line[ve] == b' ' { &line[1..ve] } else { &line[1..id_start] };
                            VcdValue::Vector(vs.to_vec())
                        }
                        b'r' => {
                            let vs = std::str::from_utf8(&line[1..id_start]).unwrap_or("0");
                            if let Ok(r) = vs.trim().parse::<f64>() { VcdValue::Real(r) } else { continue; }
                        }
                        _ => VcdValue::Bit(first),
                    };
                    last_value = Some(val);
                }
            }
        }
        last_value.unwrap_or_else(|| self.initial_value_at(sig_idx))
    }

    /// On-demand find bit value at timestamp
    #[allow(dead_code)]
    fn read_bit_value_at(&self, sig_idx: u32, target_timestamp: u64) -> Option<u8> {
        let mut reader = self.reader.borrow_mut();

        let start_offset = self
            .sparse_anchor(sig_idx, target_timestamp)
            .map(|(_, off)| off)
            .unwrap_or(self.header_end_offset);

        let _ = reader.seek_to(start_offset);
        let mut last_value: Option<u8> = None;

        loop {
            let line = match reader.read_line_bytes() {
                Some(l) => l,
                None => break,
            };
            if line.is_empty() { continue; }
            let first = line[0];

            if first == b'#' && is_ts_line(line) {
                let ts = parse_timestamp_fast(line);
                if ts > target_timestamp { break; }
                if self.event_signals.contains(&sig_idx) {
                    last_value = Some(b'0');
                }
            } else if first != b'$' {
                if let Some((sig_hash, value)) = parse_value_change_fast(line) {
                    if self.signal_ids.find(sig_hash) == Some(sig_idx) {
                        last_value = match value {
                            VcdValue::Bit(b) => Some(b),
                            _ => None,
                        };
                    }
                }
            }
        }
        last_value
    }

    /// 读取旁挂列缓存(跨进程): 命中即返回已解码变更列。
    fn try_load_col_sidecar(&self, sig_idx: u32) -> Option<Vec<(u32, VcdValue)>> {
        if cache_mode() == CacheMode::Off {
            return None;
        }
        let name = if (sig_idx as usize) < self.name_count() { self.name_at(sig_idx).to_string() } else { return None };
        let path = col_sidecar_file(std::path::Path::new(&self.filename), &name)?;
        let data = std::fs::read(&path).ok()?;
        let mut r = BufR::new(&data);
        if r.bytes()? != b"WALCOL01".to_vec() {
            return None;
        }
        let n = r.u32()? as usize;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let idx = r.u32()?;
            let val = r.val()?;
            out.push((idx, val));
        }
        if std::env::var("WAL_DEBUG_FIND").is_ok() {
            eprintln!("col-sidecar hit: sig={} n={}", name, out.len());
        }
        Some(out)
    }

    /// 冷扫描/列构建之后把该信号的变更列落盘(下一个进程直接命中)。
    fn save_col_sidecar(&self, sig_idx: u32, changes: &[(u32, VcdValue)]) {
        if changes.len() < 2 {
            return;
        }
        let Ok(meta) = std::fs::metadata(&self.filename) else { return };
        if !col_sidecar_write_enabled(meta.len()) {
            return;
        }
        if (sig_idx as usize) >= self.name_count() { return; }
        let name = self.name_at(sig_idx).to_string();
        let Some(path) = col_sidecar_file(std::path::Path::new(&self.filename), &name) else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut w = BufW::new();
        w.bytes(b"WALCOL01");
        w.u32(changes.len() as u32);
        for (i, v) in changes {
            w.u32(*i);
            w.val(v);
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, &w.b).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    fn try_cache_decoded_signal(&self, sig_idx: u32, changes: &[(u32, VcdValue)], scan_type: CacheType) {
        if changes.len() < 2 { return; }
        let mut cache = self.signal_cache.lock().unwrap();
        if cache.len() >= MAX_DECODED_SIGNALS {
            cache.clear();
        }
        let (ci, vals): (Vec<u32>, Vec<VcdValue>) = changes.iter().cloned().unzip();
        let (full_scan, scanned_up_to) = match scan_type {
            CacheType::FullScan => (true, u32::MAX),
            CacheType::PartialScan(end) => (false, end),
        };
        cache.insert(sig_idx, DecodedSignal { change_indices: ci, values: vals, full_scan, scanned_up_to });
    }
}

// ================ 快速哈希(FxHash, 波形内部表用) ================
// 波形里的哈希表都是"内部数据"(信号名/ID), 不需要 SipHash 的抗碰撞性;
// SipHash 在大文件加载/查询的每行查找上是热点。
#[derive(Default)]
struct FxHasher { hash: u64 }
const FX_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;
impl std::hash::Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut b = bytes;
        while b.len() >= 8 {
            let v = u64::from_le_bytes(b[..8].try_into().unwrap());
            self.hash = (self.hash.rotate_left(5) ^ v).wrapping_mul(FX_SEED);
            b = &b[8..];
        }
        for &x in b {
            self.hash = (self.hash.rotate_left(5) ^ x as u64).wrapping_mul(FX_SEED);
        }
    }
    #[inline] fn write_u64(&mut self, i: u64) { self.hash = (self.hash.rotate_left(5) ^ i).wrapping_mul(FX_SEED); }
    #[inline] fn write_u32(&mut self, i: u32) { self.write_u64(i as u64); }
    #[inline] fn write_usize(&mut self, i: usize) { self.write_u64(i as u64); }
    #[inline] fn write_u8(&mut self, i: u8) { self.write_u64(i as u64); }
    #[inline] fn finish(&self) -> u64 { self.hash }
}
type FxBuild = std::hash::BuildHasherDefault<FxHasher>;
type FxHashMap<K, V> = std::collections::HashMap<K, V, FxBuild>;
type FxHashSet<T> = std::collections::HashSet<T, FxBuild>;

/// 名字 → 索引用的哈希(与 id 哈希同族, 64 位 FNV-1a)
#[inline]
fn hash_name(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// $dumpvars 初值的紧凑编码: tag(0=Bit,1=Vector,2=Real) + payload
#[inline]
fn encode_initial(blob: &mut Vec<u8>, v: &VcdValue) {
    match v {
        VcdValue::Bit(b) => { blob.push(0); blob.push(*b); }
        VcdValue::Vector(vec) => {
            blob.push(1);
            blob.extend_from_slice(&(vec.len() as u32).to_le_bytes());
            blob.extend_from_slice(vec);
        }
        VcdValue::Real(r) => { blob.push(2); blob.extend_from_slice(&r.to_bits().to_le_bytes()); }
    }
}

#[inline]
fn decode_initial(b: &[u8]) -> VcdValue {
    match b.first().copied().unwrap_or(0) {
        0 => VcdValue::Bit(*b.get(1).unwrap_or(&b'x')),
        1 => {
            let n = b.get(1..5).map(|s| u32::from_le_bytes(s.try_into().unwrap_or([0; 4])) as usize).unwrap_or(0);
            VcdValue::Vector(b.get(5..5 + n).map(|s| s.to_vec()).unwrap_or_default())
        }
        2 => {
            let bits = b.get(1..9).map(|s| u64::from_le_bytes(s.try_into().unwrap_or([0; 8]))).unwrap_or(0);
            VcdValue::Real(f64::from_bits(bits))
        }
        _ => VcdValue::Bit(b'x'),
    }
}

/// 开放寻址索引(波形内部表): keys[i] = 64 位哈希, slots[i] = 索引+1(0 = 空槽)。
/// 相比 std HashMap: 无 per-entry 分配、无 SipHash、约 12B/槽(负载 ≤0.7 → ~17B/条目)。
#[derive(Clone)]
struct OpenIndex {
    keys: Vec<u64>,
    slots: Vec<u32>,
    mask: usize,
    len: usize,
}

/// 探针位置必须用"混合后"的哈希: FNV 的低位周期性很强, 直接拿低位做
/// 开放寻址会让插入退化成 O(n²)(实测 4M 信号 23s → 0.2s)。
#[inline]
fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

impl OpenIndex {
    fn new(cap_hint: usize) -> Self {
        // 1.5x 预留 → 负载 ≈0.66(增长阈值 0.7), 比 2x 省 ~25% 内存
        let cap = (cap_hint.saturating_mul(3) / 2 + 16).max(16).next_power_of_two();
        OpenIndex { keys: vec![0; cap], slots: vec![0; cap], mask: cap - 1, len: 0 }
    }
    #[inline]
    fn find(&self, h: u64) -> Option<u32> {
        let mut i = (mix64(h) as usize) & self.mask;
        loop {
            let s = self.slots[i];
            if s == 0 { return None; }
            if self.keys[i] == h { return Some(s - 1); }
            i = (i + 1) & self.mask;
        }
    }
    /// 命中哈希后还要比对真实字节的表(名字表: 处理哈希冲突)
    #[inline]
    fn find_verified(&self, h: u64, verify: impl Fn(u32) -> bool) -> Option<u32> {
        let mut i = (mix64(h) as usize) & self.mask;
        loop {
            let s = self.slots[i];
            if s == 0 { return None; }
            if self.keys[i] == h && verify(s - 1) { return Some(s - 1); }
            i = (i + 1) & self.mask;
        }
    }
    fn insert(&mut self, h: u64, idx: u32, rehash: impl Fn(u32) -> u64) {
        if (self.len + 1) * 10 >= self.slots.len() * 7 {
            self.grow(&rehash);
        }
        let mut i = (mix64(h) as usize) & self.mask;
        while self.slots[i] != 0 { i = (i + 1) & self.mask; }
        self.keys[i] = h;
        self.slots[i] = idx + 1;
        self.len += 1;
    }
    fn grow(&mut self, rehash: &impl Fn(u32) -> u64) {
        let old = std::mem::take(&mut self.slots);
        let new_cap = (old.len() * 4).max(16);
        self.slots = vec![0; new_cap];
        self.keys = vec![0; new_cap];
        self.mask = new_cap - 1;
        for s in old {
            if s == 0 { continue; }
            let idx = s - 1;
            let h = rehash(idx);
            let mut i = (mix64(h) as usize) & self.mask;
            while self.slots[i] != 0 { i = (i + 1) & self.mask; }
            self.keys[i] = h;
            self.slots[i] = s;
        }
    }
}

// ================ FAST BYTE-LEVEL PARSE FUNCTIONS ================

/// Parse timestamp: "#12345" → 12345
#[inline(always)]
fn parse_timestamp_fast(line: &[u8]) -> u64 {
    let mut n: u64 = 0;
    for &b in &line[1..] {
        if b < b'0' || b > b'9' { break; }
        n = n * 10 + (b - b'0') as u64;
    }
    n
}

/// Parse a $scope line: "$scope module name $end" → "name"
fn parse_scope_name(line: &[u8]) -> Option<String> {
    let line_str = std::str::from_utf8(line).ok()?;
    let parts: Vec<&str> = line_str.split_whitespace().collect();
    if parts.len() < 3 { return None; }
    let name = if parts.len() >= 4 {
        parts[2..].join(" ")
    } else {
        parts[2].to_string()
    };
    let name = name.trim_end_matches("$end").trim().to_string();
    if name.is_empty() { return None; }
    Some(name)
}

/// Parse a $var declaration: extract (sig_hash, name, width)
/// Format: $var wire 32 ! signal_name $end
#[allow(dead_code)]
fn parse_var_decl_fast(line: &[u8]) -> Option<(u64, String, usize)> {
    let (hash, name, width, _, _) = parse_var_decl_fast2(line)?;
    Some((hash, String::from_utf8_lossy(name).into_owned(), width))
}

/// Parse a $var declaration → (id_hash, name, width, id_bytes, is_event)。
/// 返回的都是 line 的切片(零分配;此前每个信号要分配 name String + id Vec)。
fn parse_var_decl_fast2(line: &[u8]) -> Option<(u64, &[u8], usize, &[u8], bool)> {
    // Find width (3rd field), signal ID (4th field), name (5th field to $end)
    let mut parts = [0usize; 6]; // start offsets of fields
    let mut part = 0;
    let mut in_field = false;

    for (i, &b) in line.iter().enumerate() {
        if b == b' ' || b == b'\t' {
            if in_field { part += 1; in_field = false; }
        } else if !in_field {
            if part < 6 { parts[part] = i; }
            in_field = true;
        }
        if part >= 6 { break; }
    }
    if part < 5 {
        return None;
    }

    // Detect type (2nd field, e.g. "wire", "reg", "event")
    let type_str = std::str::from_utf8(&line[parts[1]..parts[2]]).unwrap_or("wire");
    let is_event = type_str.trim() == "event";

    let width: usize = {
        let mut w: usize = 0;
        for &b in &line[parts[2]..] {
            if b < b'0' || b > b'9' { break; }
            w = w * 10 + (b - b'0') as usize;
        }
        w
    };
    if width == 0 {
        return None;
    }

    // Signal ID: bytes from parts[3] to next space
    let id_start = parts[3];
    let id_end = line[id_start..].iter().position(|&b| b == b' ').map(|p| id_start + p).unwrap_or(line.len());
    let id_bytes = &line[id_start..id_end];
    let sig_hash = hash_sig_id(id_bytes);

    // Name: from parts[4] to $end (spaces are valid in signal names); 去掉尾部空白
    let name_start = parts[4];
    let name_end = line[name_start..].iter().position(|&b| b == b'$').map(|p| name_start + p).unwrap_or(line.len());
    let mut ne = name_end;
    while ne > name_start && (line[ne - 1] == b' ' || line[ne - 1] == b'\t') { ne -= 1; }
    let name = &line[name_start..ne];

    Some((sig_hash, name, width, id_bytes, is_event))
}

/// Parse value change: "0!" or "b1010 !" or "r3.14 !" → (sig_hash, value)
/// Also handles no-space formats: "0!", "1!", "b1010!"
/// Ultra-fast byte parsing — no from_utf8(), no memchr, no allocations
#[inline(always)]
fn parse_value_change_fast(line: &[u8]) -> Option<(u64, VcdValue)> {
    let (value_part, sig_id_bytes) = split_value_id(line)?;
    Some((hash_sig_id(sig_id_bytes), vcd_value_of(value_part)?))
}

/// 值部分的取值规则(加载/扫描两条路径共用, 避免"同一行两种解释"):
/// `b…` → Vector, `r…` → Real(解析失败 None), 其它 → 单比特 Bit。
#[inline]
fn vcd_value_of(value_part: &[u8]) -> Option<VcdValue> {
    match value_part.first()? {
        b'b' => Some(VcdValue::Vector(value_part[1..].to_vec())),
        b'r' => std::str::from_utf8(&value_part[1..]).ok()?
            .trim().parse::<f64>().ok().map(VcdValue::Real),
        other => Some(VcdValue::Bit(*other)),
    }
}

/// 把一行值行拆成 (值部分, 信号 ID), 去掉 CRLF 的 '\r'; 非值行返回 None。
///
/// 扫描路径只要 ID(用 memcmp 与目标 ID 比对, 免去每行一次哈希+查表),
/// 加载/缓存路径要 (hash, VcdValue)。
#[inline]
fn split_value_id(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let len = line.len();
    if len < 2 { return None; }

    // Find last separator (space/tab) by scanning backwards — VCD lines are short
    let mut sp = len;
    for i in (0..len).rev() {
        if line[i] == b' ' || line[i] == b'\t' {
            sp = i;
            break;
        }
    }

    let (value_part, mut sig_id_bytes) = if sp < len {
        // Space found: "b1010 !" or "0 !"; VCS 的 real 行是 "r1.5  #"(两个空格),
        // 反向找最后一个空格后 value_part 会带尾随空格 → 必须 trim(否则 f64 解析失败,
        // real 信号 (get) 退化成 "x")。
        let mut vp = &line[..sp];
        while !vp.is_empty() && (vp[vp.len() - 1] == b' ' || vp[vp.len() - 1] == b'\t') {
            vp = &vp[..vp.len() - 1];
        }
        (vp, &line[sp+1..])
    } else {
        // No space: "0!" (scalar) or "b1010!" (vector no-space)
        let first = line[0];
        if matches!(first, b'0' | b'1' | b'x' | b'X' | b'z' | b'Z') && line.len() >= 2 {
            (&line[..1], &line[1..])
        } else if first == b'b' && line.len() >= 3 {
            // No-space vector: "b1010!" → find boundary between binary digits and signal ID
            // Binary digits are '0','1','x','z'; signal ID starts with printable ASCII
            let split = 1 + line[1..].iter()
                .position(|&b| !matches!(b, b'0' | b'1' | b'x' | b'z' | b'X' | b'Z' | b' ' | b'\t'))
                .unwrap_or(line.len().saturating_sub(1));
            if split > 1 && split < line.len() {
                (&line[..split], &line[split..])
            } else {
                return None;
            }
        } else {
            return None;
        }
    };

    // 去掉行尾 \r(CRLF 文件)→ 否则 id 哈希不匹配, 所有值都读不到
    while matches!(sig_id_bytes.last(), Some(b'\r')) {
        sig_id_bytes = &sig_id_bytes[..sig_id_bytes.len() - 1];
    }
    // 只排除空 id;`$` 开头的 id 是合法 VCD 标识符(如 counter 夹具的 rst `$`),
    // 指令行由调用方按"行首 $`"过滤, 不能在这里按 id 首字符误杀。
    if sig_id_bytes.is_empty() || value_part.is_empty() { return None; }

    Some((value_part, sig_id_bytes))
}


// ================= 跨进程缓存(§8): 可再生中间文件, 非格式转换 =================
//
// WAL_CACHE=off|auto|build|read (默认 auto: 命中即读, 未命中按常加载并在 auto
// 下写回)。文件位于 WAL_CACHE_DIR 或 ./.wal-rust-cache/, 名字含波形
// 长度/修改时间/格式版本; 头部另有首尾 64KB 指纹。任一不符即视为未命中。

#[derive(PartialEq, Clone, Copy)]
enum CacheMode { Off, Auto, Build, Read }

fn cache_mode() -> CacheMode {
    match std::env::var("WAL_CACHE").unwrap_or_default().to_ascii_lowercase().as_str() {
        "off" | "0" | "none" => CacheMode::Off,
        "build" => CacheMode::Build,
        "read" => CacheMode::Read,
        _ => CacheMode::Auto,
    }
}

/// 缓存目录: 默认 **CWD 相对** `./.wal-rust-cache`(执行命令的目录一定可写;
/// 波形目录可能是只读挂载/共享盘, 因此绝不写在波形旁边)。`WAL_CACHE_DIR` 可覆盖。
fn cache_dir() -> std::path::PathBuf {
    std::env::var_os("WAL_CACHE_DIR").map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(".wal-rust-cache"))
}

fn wave_fingerprint(path: &std::path::Path) -> u64 {
    use std::io::Read;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut buf = vec![0u8; 64 * 1024];
    if let Ok(mut f) = std::fs::File::open(path) {
        if let Ok(n) = f.read(&mut buf) {
            for &b in &buf[..n] { h ^= b as u64; h = h.wrapping_mul(0x100_0000_01b3); }
        }
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len > 64 * 1024 {
            use std::io::Seek;
            let _ = f.seek(std::io::SeekFrom::End(-(64 * 1024)));
            if let Ok(n) = f.read(&mut buf) {
                for &b in &buf[..n] { h ^= b as u64; h = h.wrapping_mul(0x100_0000_01b3); }
            }
        }
    }
    h
}

fn cache_file_for(vcd: &std::path::Path) -> Option<std::path::PathBuf> {
    let meta = std::fs::metadata(vcd).ok()?;
    let mtime = meta.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())?;
    let base = vcd.file_name()?.to_string_lossy().replace('/', "_");
    Some(cache_dir().join(format!("{}-{}-{}-v1.wcol", base, meta.len(), mtime)))
}

/// 列缓存旁挂目录: 与 .wcol 同 key(波形 basename+size+mtime), 后缀换 .cols。
/// 每个信号一个文件, 首次冷扫描后落盘 → 下一个进程直接命中, 不再扫全文件。
fn col_sidecar_dir(vcd: &std::path::Path) -> Option<std::path::PathBuf> {
    let f = cache_file_for(vcd)?;
    let name = f.file_name()?.to_string_lossy().replace(".wcol", ".cols");
    let mut d = f;
    d.set_file_name(name);
    Some(d)
}

fn col_sidecar_file(vcd: &std::path::Path, sig_name: &str) -> Option<std::path::PathBuf> {
    let dir = col_sidecar_dir(vcd)?;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in sig_name.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    Some(dir.join(format!("{:016x}.col", h)))
}

/// 缓存写回的最小波形大小(默认 8MB; `WAL_CACHE_MIN_MB` 可调, 测试用 0)。
fn cache_min_bytes() -> u64 {
    std::env::var("WAL_CACHE_MIN_MB").ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|mb| mb.saturating_mul(1024 * 1024))
        .unwrap_or(8 * 1024 * 1024)
}

/// 旁挂列缓存与 .wcol 同一套开关与阈值(auto/build 写, read/off 不写; ≥ 阈值)。
fn col_sidecar_write_enabled(wave_len: u64) -> bool {
    wave_len >= cache_min_bytes()
        && matches!(cache_mode(), CacheMode::Auto | CacheMode::Build)
}

struct BufW { b: Vec<u8> }
impl BufW {
    fn new() -> Self { BufW { b: Vec::with_capacity(1 << 16) } }
    fn u8(&mut self, v: u8) { self.b.push(v); }
    fn u32(&mut self, v: u32) { self.b.extend_from_slice(&v.to_le_bytes()); }
    fn u64(&mut self, v: u64) { self.b.extend_from_slice(&v.to_le_bytes()); }
    fn bytes(&mut self, v: &[u8]) { self.u32(v.len() as u32); self.b.extend_from_slice(v); }
    fn val(&mut self, v: &VcdValue) {
        match v {
            VcdValue::Bit(b) => { self.u8(0); self.u8(*b); }
            VcdValue::Vector(vec) => { self.u8(1); self.bytes(vec); }
            VcdValue::Real(r) => { self.u8(2); self.u64(r.to_bits()); }
        }
    }
}

struct BufR<'a> { b: &'a [u8], p: usize }
impl<'a> BufR<'a> {
    fn new(b: &'a [u8]) -> Self { BufR { b, p: 0 } }
    fn u8(&mut self) -> Option<u8> { let v = *self.b.get(self.p)?; self.p += 1; Some(v) }
    fn u32(&mut self) -> Option<u32> {
        let e = self.p.checked_add(4)?;
        let v = u32::from_le_bytes(self.b.get(self.p..e)?.try_into().ok()?);
        self.p = e; Some(v)
    }
    fn u64(&mut self) -> Option<u64> {
        let e = self.p.checked_add(8)?;
        let v = u64::from_le_bytes(self.b.get(self.p..e)?.try_into().ok()?);
        self.p = e; Some(v)
    }
    fn bytes(&mut self) -> Option<Vec<u8>> {
        let n = self.u32()? as usize;
        let e = self.p.checked_add(n)?;
        let v = self.b.get(self.p..e)?.to_vec();
        self.p = e; Some(v)
    }
    fn val(&mut self) -> Option<VcdValue> {
        match self.u8()? {
            0 => Some(VcdValue::Bit(self.u8()?)),
            1 => Some(VcdValue::Vector(self.bytes()?)),
            2 => Some(VcdValue::Real(f64::from_bits(self.u64()?))),
            _ => None,
        }
    }
}

impl VcdTrace {
    /// 写出可再生缓存(失败静默, 缓存不影响正确性)。
    fn save_cache(&self, path: &std::path::Path, d: &DumpIndex) {
        let mut w = BufW::new();
        w.b.extend_from_slice(b"WALVC001");
        w.u32(1);
        let meta = match std::fs::metadata(std::path::Path::new(&self.filename)) { Ok(m) => m, Err(_) => return };
        w.u64(meta.len());
        w.u64(meta.modified().ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs()).unwrap_or(0));
        w.u64(wave_fingerprint(std::path::Path::new(&self.filename)));
        w.u64(self.header_end_offset);
        // scopes
        w.u32(self.scopes.len() as u32);
        for sc in &self.scopes { w.bytes(sc.as_bytes()); }
        // timescale
        match self.timescale_exp { Some(e) => { w.u8(1); w.u8(e as u8); } None => { w.u8(0); } }
        // signals: name/id/width/initial
        w.u32(self.name_count() as u32);
        for i in 0..self.name_count() {
            w.bytes(self.name_at(i as u32).as_bytes());
            let idb = self.id_bytes(i as u32).unwrap_or_default();
            w.bytes(idb);
            w.u32(self.signal_widths.get(i).copied().unwrap_or(1));
            if self.has_initial(i as u32) {
                w.u8(1);
                w.val(&self.initial_value_at(i as u32));
            } else {
                w.u8(0);
            }
        }
        // timestamps
        match &d.timestamps {
            TsStore::Uniform { start, step, count } => { w.u8(0); w.u64(*start); w.u64(*step); w.u64(*count); }
            TsStore::Dense(v) => { w.u8(1); w.u32(v.len() as u32); for &t in v { w.u64(t); } }
        }
        // timestamp_offsets
        w.u32(d.timestamp_offsets.len() as u32);
        for &o in &d.timestamp_offsets { w.u64(o); }
        // sparse_index
        w.u32(d.sparse_index.len() as u32);
        for (sig, v) in &d.sparse_index {
            w.u32(*sig); w.u32(v.len() as u32);
            for &(ts, off) in v { w.u64(ts); w.u64(off); }
        }
        // event signals + change points
        w.u32(self.event_signals.len() as u32);
        for s in &self.event_signals { w.u32(*s); }
        w.u32(d.event_change_points.len() as u32);
        for (sig, pts) in &d.event_change_points {
            w.u32(*sig); w.u32(pts.len() as u32);
            for &p in pts { w.u64(p as u64); }
        }
        let _ = std::fs::create_dir_all(cache_dir());
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, &w.b).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// 尝试从缓存构造(命中返回 Some)。
    fn try_load_cache(cache: &std::path::Path, vcd: &std::path::Path, id: TraceId) -> Option<VcdTrace> {
        let data = std::fs::read(cache).ok()?;
        let mut r = BufR::new(&data);
        if data.len() < 8 || &data[..8] != b"WALVC001" { return None; }
        r.p = 8;
        if r.u32()? != 1 { return None; }
        let meta = std::fs::metadata(vcd).ok()?;
        if r.u64()? != meta.len() { return None; }
        let mtime = meta.modified().ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs()).unwrap_or(0);
        if r.u64()? != mtime { return None; }
        if r.u64()? != wave_fingerprint(vcd) { return None; }
        // mmap 仍然需要(值按需从 VCD 读取)
        let reader = crate::vcd::reader::MmapReader::new(vcd).ok()?;
        let header_end_offset = r.u64()?;
        let mut scopes = Vec::new();
        for _ in 0..r.u32()? { scopes.push(String::from_utf8(r.bytes()?).ok()?); }
        let timescale_exp = match r.u8()? { 1 => Some(r.u8()? as i8), _ => None };
        let n_sigs = r.u32()? as usize;
        let mut names_blob: Vec<u8> = Vec::with_capacity(n_sigs * 32);
        let mut name_meta: Vec<u64> = Vec::with_capacity(n_sigs);
        let mut name_index = OpenIndex::new(n_sigs);
        let mut signal_ids = OpenIndex::new(n_sigs);
        let mut id_blob: Vec<u8> = Vec::with_capacity(n_sigs * 4);
        let mut id_offsets: Vec<u32> = Vec::with_capacity(n_sigs);
        let mut signal_widths: Vec<u32> = Vec::with_capacity(n_sigs);
        let mut init_off: Vec<u32> = Vec::with_capacity(n_sigs);
        let mut init_blob: Vec<u8> = Vec::new();
        for i in 0..n_sigs {
            let name = r.bytes()?;
            let idb = r.bytes()?;
            let width = r.u32()?;
            if r.u8()? == 1 {
                let v = r.val()?;
                let off = init_blob.len() as u32;
                encode_initial(&mut init_blob, &v);
                init_off.push(off);
            } else {
                init_off.push(u32::MAX);
            }
            id_offsets.push(id_blob.len() as u32);
            signal_ids.insert(hash_sig_id(&idb), i as u32, |_| 0);
            id_blob.extend_from_slice(&idb);
            signal_widths.push(width);
            Self::push_name(&mut names_blob, &mut name_meta, &name);
            let idx = i as u32;
            name_index.insert(hash_name(&name), idx, |i| {
                let m = name_meta[i as usize];
                let o = (m & 0xffff_ffff) as usize;
                let l = (m >> 32) as usize;
                hash_name(&names_blob[o..o + l])
            });
        }
        let timestamps = match r.u8()? {
            0 => TsStore::Uniform { start: r.u64()?, step: r.u64()?, count: r.u64()? },
            _ => {
                let n = r.u32()? as usize;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n { v.push(r.u64()?); }
                TsStore::Dense(v)
            }
        };
        let n_off = r.u32()? as usize;
        let mut timestamp_offsets = Vec::with_capacity(n_off);
        for _ in 0..n_off { timestamp_offsets.push(r.u64()?); }
        let n_sp = r.u32()? as usize;
        let mut sparse_index: HashMap<u32, Vec<(u64, u64)>> = HashMap::with_capacity(n_sp);
        for _ in 0..n_sp {
            let sig = r.u32()?; let n = r.u32()? as usize;
            let mut v = Vec::with_capacity(n);
            for _ in 0..n { let ts = r.u64()?; let off = r.u64()?; v.push((ts, off)); }
            sparse_index.insert(sig, v);
        }
        let n_ev = r.u32()? as usize;
        let mut event_signals: HashSet<u32> = HashSet::with_capacity(n_ev);
        for _ in 0..n_ev { event_signals.insert(r.u32()?); }
        let n_ecp = r.u32()? as usize;
        let mut event_change_points: HashMap<u32, Vec<usize>> = HashMap::with_capacity(n_ecp);
        for _ in 0..n_ecp {
            let sig = r.u32()?; let n = r.u32()? as usize;
            let mut v = Vec::with_capacity(n);
            for _ in 0..n { v.push(r.u64()? as usize); }
            event_change_points.insert(sig, v);
        }
        let lru_cap = std::num::NonZeroUsize::new(64 * 1024).unwrap();
        let filename = vcd.to_string_lossy().to_string();
        let dump = DumpIndex {
            timestamps, timestamp_offsets, sparse_index, event_change_points,
        };
        let est_ts = dump.timestamps.len();
        Some(VcdTrace {
            id, filename,
            names_blob, name_meta, name_index, signal_ids,
            id_blob, id_offsets, signal_widths, init_off, init_blob,
            name_cache: std::cell::RefCell::new(FxHashMap::default()),
            warned_xinit: std::cell::RefCell::new(std::collections::HashSet::new()),
            event_signals,
            dump_index: {
                let c = std::cell::OnceCell::new();
                let _ = c.set(dump);
                c
            },
            est_ts,
            lru_cache: RefCell::new(lru::LruCache::new(lru_cap)),
            signal_cache: Mutex::new(HashMap::new()),
            col_cache: std::cell::RefCell::new(HashMap::new()),
            off_cache: std::cell::RefCell::new(HashMap::new()),
            reader: RefCell::new(reader),
            header_end_offset,
            dump_data_start: header_end_offset,
            scopes, timescale_exp,
            current_index: 0,
        })
    }
}

impl Trace for VcdTrace {
    fn prepare(&self, names: &[String]) {
        self.prepare_names(names);
    }

    fn id(&self) -> &TraceId {
        &self.id
    }

    fn filename(&self) -> &str {
        &self.filename
    }

    fn step(&mut self, steps: usize) -> Result<(), String> {
        let new_index = self.current_index.saturating_add(steps);
        if new_index > self.max_index() {
            return Err(format!(
                "Step {} would exceed max index {}",
                steps, self.max_index()
            ));
        }
        self.current_index = new_index;
        Ok(())
    }

    fn signal_value(&self, name: &str, offset: usize) -> Result<ScalarValue, String> {
        if self.dump().timestamps.len() == 0 {
            // 只有头、没有 dump 段(空/截断到定义结束): 时间线为空 → 返回初值
            let sig_idx = self.resolve_idx(name).ok_or_else(|| format!("Unknown signal: {}", name))?;
            let width = self.signal_widths.get(sig_idx as usize).copied().unwrap_or(1) as usize;
            let init = self.initial_value_at(sig_idx);
            let sv = value_to_scalar(&init);
            return Ok(match sv {
                ScalarValue::Bit(b) if (b == b'x' || b == b'z') && width > 1 => ScalarValue::Vector(vec![b; width]),
                other => other,
            });
        }
        let idx = if offset < self.dump().timestamps.len() {
            offset
        } else {
            return Err(format!(
                "signal_value: offset {} out of range (max {}) for signal '{}'",
                offset, self.dump().timestamps.len().max(1u64 as usize)-1, name
            ));
        };

        let target_time = self.dump().timestamps.get(idx);
        let sig_idx = self.resolve_idx(name)
            .ok_or_else(|| format!(
                "signal '{}' not found. Available signals (first 5): {:?}",
                name,
                (0..self.name_count().min(5) as u32).map(|i| self.name_at(i)).collect::<Vec<_>>()
            ))?;
        let cache_key = (sig_idx, target_time);

        // Initial-value representation shared by every read path: the value
        // before the first change is 'x' — as a full-width vector for
        // multi-bit signals (the same shape a change line "bxxx…x id" yields),
        // so `(changes s)` / comparisons never see a Bit-vs-Vector mismatch.
        let width = self.signal_widths.get(sig_idx as usize).copied().unwrap_or(1) as usize;
        let normalize = |sv: ScalarValue| -> ScalarValue {
            match sv {
                ScalarValue::Bit(b) if (b == b'x' || b == b'z') && width > 1 => {
                    ScalarValue::Vector(vec![b; width])
                }
                other => other,
            }
        };

        // Check decoded signal cache
        if let Some(ds) = self.signal_cache.lock().unwrap().get(&sig_idx) {
            if ds.full_scan || (idx as u32) <= ds.scanned_up_to {
                let val_idx = match ds.change_indices.binary_search(&(idx as u32)) {
                    Ok(i) => i,           // exact match → value at this change
                    Err(0) => {
                        // before any change: $dumpvars initial snapshot, else x
                        return Ok(normalize(value_to_scalar(&self.initial_value_at(sig_idx))));
                    }
                    Err(i) => i - 1,       // between changes → previous value
                };
                if val_idx < ds.values.len() {
                    let v = &ds.values[val_idx];
                    self.lru_cache.borrow_mut().put(cache_key, v.clone());
                    return Ok(value_to_scalar(v));
                }
            }
        }

        // Check LRU cache
        if let Some(cached) = self.lru_cache.borrow_mut().get(&cache_key).cloned() {
            return Ok(normalize(value_to_scalar(&cached)));
        }

        // On-demand read from mmap
        let val = self.read_signal_value_at(sig_idx, target_time);
        self.lru_cache.borrow_mut().put(cache_key, val.clone());
        Ok(normalize(value_to_scalar(&val)))
    }

    fn signal_width(&self, name: &str) -> Result<usize, String> {
        let sig_idx = self.resolve_idx(name)
            .ok_or_else(|| format!("Unknown signal: {}", name))?;
        Ok(self.signal_widths.get(sig_idx as usize).copied().unwrap_or(1) as usize)
    }

    fn resolve_name(&self, name: &str) -> Option<String> {
        self.resolve_idx(name).map(|i| self.name_at(i).to_string())
    }

    fn resolve_name_strict(&self, name: &str) -> Result<String, String> {
        // exact 优先
        if let Some(i) = self.name_index.find_verified(hash_name(name.as_bytes()), |i| self.name_at(i) == name) {
            return Ok(self.name_at(i).to_string());
        }
        fn leaf(s: &str) -> &str { s.rsplitn(2, '.').next().unwrap_or("") }
        let allow_leaf = name.len() <= 8 || !name.contains('.');
        let mut hits: Vec<String> = Vec::new();
        for i in 0..self.name_count() as u32 {
            let s = self.name_at(i);
            let matched = (allow_leaf && leaf(s) == name) || s.contains(name);
            if matched {
                if !hits.is_empty() {
                    hits.push(s.to_string());
                    return Err(format!(
                        "signal '{}' is ambiguous ({} candidates: {:?}) — 请用完整名字",
                        name, hits.len(), &hits[..hits.len().min(5)]
                    ));
                }
                hits.push(s.to_string());
            }
        }
        match hits.len() {
            1 => Ok(hits.pop().unwrap()),
            _ => Err(format!("signal '{}' not found in any loaded trace.", name)),
        }
    }

    fn defined_initial_value(&self, name: &str) -> Option<ScalarValue> {
        let idx = self.resolve_idx(name)?;
        let init = self.initial_value_at(idx);
        if vcd_is_defined(&init) { Some(value_to_scalar(&init)) } else { None }
    }

    fn initial_value(&self, name: &str) -> Option<ScalarValue> {
        let idx = self.resolve_idx(name)?;
        if !self.has_initial(idx) { return None; }
        Some(value_to_scalar(&self.initial_value_at(idx)))
    }

    fn signals(&self) -> Vec<String> {
        (0..self.name_count() as u32).map(|i| self.name_at(i).to_string()).collect()
    }

    fn scopes(&self) -> Vec<String> {
        self.scopes.clone()
    }

    fn max_index(&self) -> usize {
        let d = self.dump();
        if d.timestamps.len() == 0 { 0 } else { d.timestamps.len() - 1 }
    }

    fn set_index(&mut self, index: usize) -> Result<(), String> {
        if index > self.max_index() {
            return Err(format!("Index {} exceeds max {}", index, self.max_index()));
        }
        self.current_index = index;
        Ok(())
    }

    fn index(&self) -> usize {
        self.current_index
    }

    fn find_indices(&self, name: &str, cond: FindCondition) -> Result<Vec<usize>, String> {
        if std::env::var("WAL_DEBUG_FIND").is_ok() {
            eprintln!("find_indices enter: {}", name);
        }
        let sig_idx = self.resolve_idx(name)
            .ok_or_else(|| format!("Unknown signal: {}", name))?;
        // 先声明"我要这个信号的变更列": 索引尚未构建时, 时间戳表与变更列在
        // 同一次遍历里一起算出(冷启动只读一遍文件)。
        if self.ensure_dump_index(&[sig_idx]).timestamps.len() == 0 {
            return Ok(Vec::new()); // 空时间线
        }
        if matches!(cond, FindCondition::Rising | FindCondition::Falling) {
            self.warn_undefined_initial(sig_idx);
        }
        if std::env::var("WAL_DEBUG_FIND").is_ok() {
            eprintln!("  sig_idx={} id={:?} anchors={}",
                      sig_idx,
                      self.id_bytes(sig_idx),
                      self.dump().sparse_index.get(&sig_idx).map(|m| m.len()).unwrap_or(0));
            let idb = self.id_bytes(sig_idx).unwrap_or_default().to_vec();
            let h = crate::trace::vcd::hash_sig_id(&idb);
            eprintln!("  hash={:#x} sid has it: {}", h,
                      self.signal_ids.find(h) == Some(sig_idx));
        }

        // Event signal fast path: change points already recorded during load
        if self.event_signals.contains(&sig_idx) {
            if let Some(points) = self.dump().event_change_points.get(&sig_idx) {
                if matches!(&cond,
                    FindCondition::Value(1) | FindCondition::ValueI64(1)
                    | FindCondition::Rising | FindCondition::Neq(0)
                    | FindCondition::NeqI64(0) | FindCondition::High)
                {
                    return Ok(points.clone());
                }
                if matches!(&cond, FindCondition::Neq(1) | FindCondition::Low) {
                    // All timestamps except event points
                    let all: Vec<usize> = (0..=self.max_index()).collect();
                    let points_set: std::collections::HashSet<usize> = points.iter().copied().collect();
                    return Ok(all.into_iter().filter(|i| !points_set.contains(i)).collect());
                }
            }
            return Ok(vec![]);
        }

        // Single authoritative per-index semantics over the change list.
        let all = self.anchored_changes(sig_idx)?;
        let init = self.initial_value_at(sig_idx);
        Ok(eval_change_list(self.max_index(), &all, &init, &cond))
    }

    fn find_indices_batch(&self, entries: &[BatchEntry]) -> Result<Vec<(String, Vec<usize>)>, String> {
        // Extract simple conditions from entries
        let signals: Vec<(String, FindCondition)> = entries.iter().filter_map(|e| {
            if let BatchEntry::Simple(name, cond) = e {
                Some((name.clone(), cond.clone()))
            } else {
                None
            }
        }).collect();

        if signals.is_empty() {
            return Ok(Vec::new());
        }

        struct BatchSig {
            name: String,
            cond: FindCondition,
            sig_idx: u32,
        }
        let mut batch_sigs: Vec<BatchSig> = Vec::new();
        // A signal may appear in several conditions with different values
        // (e.g. (count (= s 1) (= s 0))); each id must keep ALL batch indices,
        // not just the last one (the old single-usize map dropped counts).
        let mut id_to_batch: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();

        for (name, cond) in signals {
            let sig_idx = match self.name_index.find_verified(hash_name(name.as_bytes()), |i| self.name_at(i) == name.as_str()) {
                Some(i) => i,
                None => continue,
            };
            let id_bytes = match self.id_bytes(sig_idx) {
                Some(b) => b,
                None => continue,
            };
            let batch_idx = batch_sigs.len();
            id_to_batch.entry(id_bytes.to_vec()).or_default().push(batch_idx);
            batch_sigs.push(BatchSig {
                name: name.clone(),
                cond: cond.clone(),
                sig_idx,
            });
        }

        if batch_sigs.is_empty() {
            return Ok(Vec::new());
        }

        use rayon::prelude::*;
        let shared_mmap = self.reader.borrow().data.clone();
        let hdr_end = self.dump_data_start as usize; // 同 anchored_changes: 规则 A 的起点
        let data_len = shared_mmap.len();

        let n_threads = num_cpus::get().max(4);
        let dump_len = data_len.saturating_sub(hdr_end);
        // Minimum chunk size: tiny chunks fragment the scan and lose cross-chunk
        // previous-value state (Rising/Falling/Changed conditions).
        let chunk_size = (dump_len / n_threads.max(1)).max(64 * 1024);
        let mut boundaries = vec![hdr_end];
        for i in 1..n_threads {
            let mut p = hdr_end + i * chunk_size;
            if p >= data_len { break; }
            loop {
                match memchr::memchr(b'\n', &shared_mmap[p..data_len]) {
                    Some(n) => p += n + 1,
                    None => { p = data_len; break; }
                }
                if p >= data_len { break; }
                if shared_mmap[p] == b'#' { break; }
            }
            boundaries.push(p);
        }
        boundaries.push(data_len);

        // Only '#' at line start counts (see find_indices); record count BEFORE
        // the boundary position so a boundary '#' line belongs to the next chunk.
        //
        // 每块用 memchr 定位 '#'(SIMD), 并行统计后前缀和 —— 旧实现是**逐字节
        // 标量循环**扫整个 dump(58.7GB ≈ 数百 CPU·s, 且完全不并行)。
        let boundary_ts: Vec<usize> = {
            let counts: Vec<usize> = boundaries[..boundaries.len() - 1]
                .par_iter()
                .enumerate()
                .map(|(i, &bs)| {
                    let seg = &shared_mmap[bs..boundaries[i + 1]];
                    let mut count = 0usize;
                    let mut p = 0usize;
                    while p < seg.len() {
                        match memchr::memchr(b'#', &seg[p..]) {
                            Some(n) => {
                                let abs = p + n;
                                if (abs == 0 || seg[abs - 1] == b'\n') && is_ts_line_at(seg, abs) {
                                    count += 1;
                                }
                                p = abs + 1;
                            }
                            None => break,
                        }
                    }
                    count
                })
                .collect();
            let mut ts = Vec::with_capacity(boundaries.len());
            let mut acc = 0usize;
            ts.push(0);
            for c in counts {
                acc += c;
                ts.push(acc);
            }
            ts
        };

        // Organize IDs by length for fast lookup: id_by_len[len] = vec![(id_bytes, batch_idx)]
        let mut id_by_len: Vec<Vec<(Vec<u8>, usize)>> = vec![Vec::new(); 8];
        for (id_bytes, batch_idxs) in &id_to_batch {
            if id_bytes.len() < 8 {
                for &b in batch_idxs {
                    id_by_len[id_bytes.len()].push((id_bytes.clone(), b));
                }
            }
        }

        let chunks: Vec<(usize, usize, usize, Vec<Option<VcdValue>>)> = (0..boundaries.len() - 1)
            .map(|i| {
                let seeds = if boundary_ts[i] > 0 {
                    batch_sigs.iter().map(|bs| {
                        if std::env::var("WAL_DEBUG_FIND").is_ok() {
                            eprintln!("batch seed[{}] sig={}", i, bs.sig_idx);
                        }
                        Some(self.read_signal_value_at(bs.sig_idx, self.dump().timestamps.get(boundary_ts[i] - 1)))
                    }).collect()
                } else {
                    vec![None; batch_sigs.len()]
                };
                (boundaries[i], boundaries[i+1], boundary_ts[i], seeds)
            })
            .collect();

        let batch_sigs_arc = Arc::new(batch_sigs);
        let id_by_len_arc = Arc::new(id_by_len);

        if std::env::var("WAL_DEBUG_FIND").is_ok() {
            eprintln!("batch: chunks={} sigs={}", chunks.len(), batch_sigs_arc.len());
        }
        let results: Vec<Vec<Vec<usize>>> = chunks.iter().map(|&(start, end, start_ts, ref seeds)| {
            let chunk = &shared_mmap[start..end];
            if std::env::var("WAL_DEBUG_FIND").is_ok() {
                eprintln!("batch scan chunk start..{}", chunk.len());
            }
            let batch_count = batch_sigs_arc.len();
            let mut local: Vec<Vec<usize>> = vec![Vec::new(); batch_count];
            let mut current_vals: Vec<Option<VcdValue>> = seeds.clone();
            let mut prev_vals: Vec<Option<VcdValue>> = vec![None; batch_count];
            let mut ts_idx = start_ts;
            let mut seen_first_ts = false;

            let mut lp = 0usize;
            while lp < chunk.len() {
                let line_start = lp;
                let line_end = match memchr::memchr(b'\n', &chunk[lp..]) {
                    Some(nl) => { lp += nl; let end = lp; lp += 1; end }
                    None => break,
                };
                let line = &chunk[line_start..line_end];
                if line.is_empty() { continue; }
                let first = line[0];

                if first == b'#' && is_ts_line(line) {
                    if seen_first_ts {
                        for b in 0..batch_count {
                            if let Some(ref val) = current_vals[b] {
                                if find_cond_matches(val, prev_vals[b].as_ref(), &batch_sigs_arc[b].cond) {
                                    local[b].push(ts_idx);
                                }
                                prev_vals[b] = current_vals[b].clone();
                            }
                        }
                        ts_idx += 1;
                    }
                    seen_first_ts = true;
                } else if first != b'$' && !line.is_empty() {
                    // Check IDs longest-first for unambiguous match
                    let max_id_len = line.len().min(6);
                    'idscan: for id_len in (1..max_id_len).rev() {
                        let id_start = line.len() - id_len;
                        let candidates = &id_by_len_arc[id_len];
                        // Same signal may carry several conditions → apply the
                        // value to EVERY batch slot that shares this id.
                        let mut matched: Vec<usize> = Vec::new();
                        for (ref id_bytes, batch_idx) in candidates.iter() {
                            if (line.len() == id_len + 1 || (id_start > 0 && line[id_start - 1] == b' ')) && &line[id_start..] == id_bytes.as_slice() {
                                matched.push(*batch_idx);
                            }
                        }
                        if !matched.is_empty() {
                            let val = match first {
                                b'b' => {
                                    let ve = id_start.saturating_sub(1);
                                    let vs = if ve > 1 && line[ve] == b' ' { &line[1..ve] } else { &line[1..id_start] };
                                    VcdValue::Vector(vs.to_vec())
                                }
                                b'r' => {
                                    let vs = std::str::from_utf8(&line[1..id_start]).unwrap_or("0");
                                    if let Ok(r) = vs.trim().parse::<f64>() { VcdValue::Real(r) } else { break 'idscan; }
                                }
                                _ => VcdValue::Bit(first),
                            };
                            for &batch_idx in &matched {
                                if !seen_first_ts {
                                    // orphan value line before first '#': previous timestamp value
                                    prev_vals[batch_idx] = Some(val.clone());
                                } else {
                                    current_vals[batch_idx] = Some(val.clone());
                                }
                            }
                            break 'idscan;
                        }
                    }
                }
            }

            if seen_first_ts {
                for b in 0..batch_count {
                    if let Some(ref val) = current_vals[b] {
                        if find_cond_matches(val, prev_vals[b].as_ref(), &batch_sigs_arc[b].cond) {
                            local[b].push(ts_idx);
                        }
                    }
                }
            }

            if std::env::var("WAL_DEBUG_FIND").is_ok() {
                eprintln!("batch scan done");
            }
            local
        }).collect();
        if std::env::var("WAL_DEBUG_FIND").is_ok() {
            eprintln!("batch collect done: {}", results.len());
        }

        // Merge per-batch results
        let batch_count = batch_sigs_arc.len();
        let mut merged: Vec<Vec<usize>> = vec![Vec::new(); batch_count];
        for chunk_result in results {
            for b in 0..batch_count {
                merged[b].extend(chunk_result[b].iter());
            }
        }

        let mut result: Vec<(String, Vec<usize>)> = Vec::new();
        for (b, bs) in batch_sigs_arc.iter().enumerate() {
            let mut idxs = std::mem::take(&mut merged[b]);
            idxs.sort();
            idxs.dedup();
            result.push((bs.name.clone(), idxs));
        }
        Ok(result)
    }

    /// Single-pass top-`k` by value-change count. Uses the LOAD-time hash map
    /// (signal_ids) + a dense counts vec — no per-signal map rebuild (that was
    /// the 42M-signal 15-minute path: a HashMap<Vec<u8>,u32> with 42M entries).
    fn signal_change_counts_top(&self, k: usize) -> Vec<(String, usize)> {
        let data = self.reader.borrow().data.clone();
        let hdr = self.dump_data_start as usize; // 同 anchored_changes
        let dump = &data[hdr..];

        let n_sigs = self.id_offsets.len();
        let mut counts: Vec<u32> = vec![0; n_sigs];
        let mut i = 0usize;
        while i < dump.len() {
            let line_start = i;
            let line_end = match memchr::memchr(b'\n', &dump[i..]) {
                Some(nl) => {
                    i += nl;
                    let e = i;
                    i += 1;
                    e
                }
                None => break,
            };
            let line = &dump[line_start..line_end];
            if line.is_empty() {
                continue;
            }
            let first = line[0];
            // value lines: 0/1/x/z/b/r (case-insensitive bits)
            if !matches!(first, b'0' | b'1' | b'x' | b'X' | b'z' | b'Z' | b'b' | b'B' | b'r' | b'R') {
                continue;
            }
            let id: &[u8] = if matches!(first, b'b' | b'B' | b'r' | b'R') {
                match line.iter().rposition(|&c| c == b' ') {
                    Some(pos) => &line[pos + 1..],
                    None => continue,
                }
            } else {
                &line[1..]
            };
            if id.is_empty() || id.len() > 64 { continue; }
            let hash = hash_sig_id(id);
            if let Some(idx) = self.signal_ids.find(hash) {
                if let Some(c) = counts.get_mut(idx as usize) {
                    *c += 1;
                }
            }
        }

        let mut v: Vec<(String, usize)> = counts.iter().enumerate()
            .filter(|(_, c)| **c > 0)
            .filter_map(|(idx, c)| if idx < self.name_count() { Some((self.name_at(idx as u32).to_string(), *c as usize)) } else { None })
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.truncate(k);
        v
    }

    fn timestamp_at(&self, index: usize) -> Option<u64> {
        if index < self.dump().timestamps.len() { Some(self.dump().timestamps.get(index)) } else { None }
    }

    fn timescale_exp(&self) -> Option<i8> {
        self.timescale_exp
    }

    fn change_points(&self, name: &str) -> Result<Vec<(usize, ScalarValue)>, String> {
        let sig_idx = self.resolve_idx(name)
            .ok_or_else(|| format!("Unknown signal: {}", name))?;
        // Event signals: change points already recorded during load
        if let Some(points) = self.dump().event_change_points.get(&sig_idx) {
            let mut out = Vec::with_capacity(points.len());
            for &i in points {
                let idx = i as usize;
                let sv = self.signal_value(name, idx)?;
                out.push((idx, sv));
            }
            return Ok(out);
        }
        // find_indices(Changed) skips the first record (no previous value vs
        // nothing); prepend the first WRITE from the change list itself — the
        // sparse-index prepend is gone with the sampled anchors.
        let all = self.anchored_changes(sig_idx)?;
        let init = self.initial_value_at(sig_idx);
        let mut changes = eval_change_list(self.max_index(), &all, &init, &FindCondition::Changed);
        if let Some(&(first_idx, _)) = all.first() {
            let fi = first_idx as usize;
            if changes.first() != Some(&fi) {
                changes.insert(0, fi);
            }
        }
        let mut out = Vec::with_capacity(changes.len());
        for &i in &changes {
            out.push((i, self.signal_value(name, i)?));
        }
        Ok(out)
    }
}

/// Check if current value matches the condition
/// 是否"确定值"(不含 x/z)——决定 $dumpvars 初值能否作为索引 0 的前驱。
fn vcd_is_defined(val: &VcdValue) -> bool {
    match val {
        VcdValue::Bit(b) => *b == b'0' || *b == b'1',
        VcdValue::Vector(v) => !v.is_empty() && v.iter().all(|b| *b == b'0' || *b == b'1'),
        VcdValue::Real(_) => true,
    }
}

fn vcd_is_zero(val: &VcdValue) -> Option<bool> {
    match val {
        // x/z 不是"确定的非零"——返回 None 让调用方回退到位比较
        VcdValue::Bit(b) => match *b {
            b'0' => Some(true),
            b'1' => Some(false),
            _ => None,
        },
        VcdValue::Vector(v) => {
            if v.iter().all(|b| *b == b'0' || *b == b'1') {
                Some(v.iter().all(|b| *b == b'0'))
            } else {
                None // contains x/z: not a defined zero
            }
        }
        _ => None,
    }
}

/// Evaluate a condition over a per-index change list (ascending ts_idx, the
/// LAST write per index — the list is already delta-collapsed) with the
/// initial value as predecessor. Single authoritative per-index semantics used
/// by BOTH the warm cached-column path and the id-anchored cold scan.
fn eval_change_list(
    max_index: usize,
    changes: &[(u32, VcdValue)],
    initial: &VcdValue,
    cond: &FindCondition,
) -> Vec<usize> {
    let is_edge = matches!(
        cond,
        FindCondition::Rising | FindCondition::Falling | FindCondition::Changed
    );
    let mut indices = Vec::new();
    // 索引 0 没有前驱: 若首个变更点就在 0, prev 必须为 None(否则 x→v 会被
    // 误判为"变化", 与逐拍 op_changes / 区间扫描引擎不一致)。
    // 索引 0 没有前驱 —— 但若 $dumpvars 给了**确定**初值, 该初值就是索引 0 的前驱:
    // dumpvars 0 → #0 1 是一次真实上升沿(初值为 x/无初值时才视为无前驱)。
    let first_is_zero = changes.first().map(|(i, _)| *i as usize) == Some(0);
    let mut prev_val: Option<VcdValue> = if first_is_zero {
        if vcd_is_defined(initial) { Some(initial.clone()) } else { None }
    } else {
        Some(initial.clone())
    };
    // Initial held segment [0, first change): level conditions see it; edges
    // use the initial value as their previous value (x→x not a change, x→0/1
    // never a rise/fall).
    if !is_edge && find_cond_matches(initial, None, cond) {
        let first_idx = changes.first().map(|(i, _)| *i as usize).unwrap_or(max_index + 1);
        indices.extend(0..first_idx.min(max_index + 1));
    }
    for (k, (ci, v)) in changes.iter().enumerate() {
        let idx = *ci as usize;
        if idx > max_index { break; }
        let matched = find_cond_matches(v, prev_val.as_ref(), cond);
        prev_val = Some(v.clone());
        if !matched { continue; }
        if is_edge {
            indices.push(idx);
        } else {
            let end = changes.get(k + 1).map(|(n, _)| *n as usize).unwrap_or(max_index + 1);
            indices.extend(idx..end.min(max_index + 1));
        }
    }
    indices
}

fn find_cond_matches(val: &VcdValue, prev_val: Option<&VcdValue>, cond: &FindCondition) -> bool {
    let prev_bit = prev_val.and_then(|v| v.as_bit());
    match cond {
        // Vector semantics: rising = previous value is 0, current is non-zero
        // (defined); falling = the reverse. 1-bit signals behave as before.
        FindCondition::Rising => {
            match (prev_val.and_then(vcd_is_zero), vcd_is_zero(val)) {
                (Some(true), Some(false)) => true,
                _ => prev_bit == Some(b'0') && val.as_bit() == Some(b'1'),
            }
        }
        FindCondition::Falling => {
            match (prev_val.and_then(vcd_is_zero), vcd_is_zero(val)) {
                (Some(false), Some(true)) => true,
                _ => prev_bit == Some(b'1') && val.as_bit() == Some(b'0'),
            }
        }
        FindCondition::High => val.as_bit() == Some(b'1'),
        FindCondition::Low => val.as_bit() == Some(b'0'),
        FindCondition::Value(v) => {
            let bit = val.as_bit();
            if bit.is_some() {
                bit == Some(*v) || (bit == Some(b'1') && *v == 1) || (bit == Some(b'0') && *v == 0)
            } else {
                // Vector signal: compare as integer (e.g. 4-bit "0001" == 1)
                val.to_i64() == Some(*v as i64)
            }
        }
        FindCondition::ValueI64(target) => val.to_i64() == Some(*target),
        FindCondition::Neq(v) => {
            let bit = val.as_bit();
            if bit.is_some() {
                !(bit == Some(*v) || (bit == Some(b'1') && *v == 1) || (bit == Some(b'0') && *v == 0))
            } else {
                // Vector signal: compare as integer
                val.to_i64() != Some(*v as i64)
            }
        }
        FindCondition::NeqI64(target) => val.to_i64() != Some(*target),
        FindCondition::IsX => val.has_x(),
        FindCondition::IsZ => val.has_z(),
        FindCondition::Changed => match prev_val {
            Some(p) => !vcd_semantic_eq(p, val),
            None => false,
        },
    }
}

/// Semantic equality for "did the value change" comparisons: the initial value
/// is represented as `Bit('x')` while a set value may be a same-state vector
/// (e.g. 8-bit "xxxxxxxx"), and a fully-uniform vector is the same state as its
/// single-bit form. Without normalization the initial x→x transition counted as
/// a change, making VCD's `(changes s)` / changed-counts disagree with FST.
fn vcd_semantic_eq(a: &VcdValue, b: &VcdValue) -> bool {
    fn normalize(v: &VcdValue) -> VcdValue {
        match v {
            VcdValue::Vector(vec) if !vec.is_empty() && vec.iter().all(|&c| c == vec[0]) => {
                VcdValue::Bit(vec[0])
            }
            other => other.clone(),
        }
    }
    normalize(a) == normalize(b)
}

fn val_to_bit(val: &VcdValue) -> Option<u8> {
    match val {
        VcdValue::Bit(b) => Some(*b),
        VcdValue::Vector(v) if v.len() == 1 => Some(v[0]),
        _ => None,
    }
}

fn value_to_scalar(val: &VcdValue) -> ScalarValue {
    match val {
        VcdValue::Bit(b) => ScalarValue::Bit(*b),
        VcdValue::Vector(v) => ScalarValue::Vector(v.clone()),
        VcdValue::Real(r) => ScalarValue::Real(*r),
    }
}

fn find_signal_in_block(block: &[u8], target_id: &[u8], id_len: usize) -> Option<VcdValue> {
    // A timestamp may contain several value lines for the same id (delta
    // cycles/glitches). VCD semantics: the value at the timestamp is the LAST
    // one written — keep scanning and return the last occurrence. Parsing via
    // parse_value_change_fast covers every line form (with/without the space
    // separator, bit/real/scalar).
    let target_hash = hash_sig_id(target_id);
    let nl = memchr::memchr(b'\n', block)?;
    let mut lp = nl + 1;
    let mut last: Option<VcdValue> = None;
    while lp < block.len() {
        let line_end = match memchr::memchr(b'\n', &block[lp..]) {
            Some(n) => lp + n,
            None => block.len(),
        };
        let line = &block[lp..line_end];
        if !line.is_empty() {
            if let Some((sig_hash, val)) = parse_value_change_fast(line) {
                if sig_hash == target_hash {
                    last = Some(val);
                }
            }
        }
        lp = line_end + 1;
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::trace::FindCondition;

    fn load_vcd(path: &str) -> VcdTrace {
        VcdTrace::load(std::path::Path::new(path), "test".to_string()).unwrap()
    }

    fn make_mini_vcd() -> std::path::PathBuf {
        // mini.vcd: 1-bit clk(!) and a("), 4 timestamps
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let uniq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("wal_mini_{}_{}.vcd", std::process::id(), uniq));
        std::fs::write(&p, "$timescale 1ns $end\n\
$scope module top $end\n\
$var wire 1 ! clk $end\n\
$var wire 1 \" a $end\n\
$upscope $end\n\
$enddefinitions $end\n\
$dumpvars\n\
#0\n\
0!\n\
1\"\n\
$end\n\
#20\n\
1!\n\
0\"\n\
$end\n\
#40\n\
0!\n\
1\"\n\
$end\n").unwrap();
        p
    }

    #[test]
    fn test_find_changed_vector() {
        // a: #0=1, #20=0, #40=1 -> changes at ts 2 (index 1) and 4 (index 3)
        let trace = load_vcd(make_mini_vcd().to_str().unwrap());
        let sigs = trace.signals();
        assert!(sigs.iter().any(|s| s.contains("a")), "a not found: {:?}", &sigs[..5]);
        let name = sigs.iter().find(|s| s.ends_with("a")).unwrap().clone();
        let idx = trace.find_indices(&name, FindCondition::Changed).unwrap();
        eprintln!("changes({}) = {:?}", name, idx);
        assert!(idx.len() >= 1, "no changes detected");
    }

    #[test]
    fn test_find_changed_9bit() {
        // 9-bit vector v changes every 100 ts
        let p = std::env::temp_dir().join(format!("wal_big_{}.vcd", std::process::id()));
        let mut vcd = String::from("$timescale 1ns $end\n$scope module top $end\n$var wire 9 ! v [8:0] $end\n$upscope $end\n$enddefinitions $end\n$dumpvars\n#0\nb000000000 !\n$end\n");
        for t in (100..2000).step_by(100) {
            vcd.push_str(&format!("#{}\nb{:09b} !\n$end\n", t, (t / 100) % 512));
        }
        std::fs::write(&p, vcd).unwrap();
        let trace = load_vcd(p.to_str().unwrap());
        let sigs = trace.signals();
        let name = sigs.iter().find(|s| s.contains("v [")).unwrap().clone();
        eprintln!("9bit signal: {}", name);
        let idx = trace.find_indices(&name, FindCondition::Changed).unwrap();
        eprintln!("changes(9bit {}) = {:?} (len {})", name, &idx[..idx.len().min(5)], idx.len());
        assert!(idx.len() >= 1, "no changes on 9-bit vector");
    }

    #[test]
    fn test_find_changed_runvcd() {
        // Requires the macro_trace TB dump; skip when not present
        let path = std::path::Path::new("/home/hesheng/Projects/macro_trace/run.vcd");
        if !path.exists() {
            eprintln!("SKIP: macro_trace run.vcd not found");
            return;
        }
        let trace = load_vcd(path.to_str().unwrap());
        let sigs = trace.signals();
        let names: Vec<&String> = sigs.iter().filter(|s| s.contains("op_go_i [")).collect();
        eprintln!("op_go_i signals: {:?}", names);
        for n in names {
            let idx = trace.find_indices(n, FindCondition::Changed).unwrap();
            eprintln!("changes({}) len={}", n, idx.len());
        }
        let idx = trace.find_indices("tb_macro_trace.op_go_i", FindCondition::Changed);
        eprintln!("fuzzy name: {:?}", idx.as_ref().map(|v| v.len()).unwrap_or(999));
    }

    #[test]
    fn test_find_rising_vector() {
        let trace = load_vcd(make_mini_vcd().to_str().unwrap());
        let sigs = trace.signals();
        let name = sigs.iter().find(|s| s.ends_with("a")).unwrap().clone();
        // rising a at #40 (index 3)
        let idx = trace.find_indices(&name, FindCondition::Rising).unwrap();
        eprintln!("rising({}) = {:?}", name, idx);
        assert!(!idx.is_empty(), "no rising detected");
    }

    /// Regression (152GB round §8.4 #1): the initial 'x' sample must be shared
    /// by every read path (get/at, find_indices, change_points) with the same
    /// representation — a full-width vector for multi-bit signals — so is-x
    /// counts and `(changes s)` agree with the FST backend.
    #[test]
    fn test_initial_x_unified_across_paths() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let uniq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("wal_initx_{}_{}.vcd", std::process::id(), uniq));
        std::fs::write(&p, "$timescale 1ns $end\n\
$scope module t $end\n\
$var wire 8 ! v [7:0] $end\n\
$enddefinitions $end\n\
$dumpvars\n\
#0\n\
$end\n\
#10\n\
bxxxxxxxx !\n\
$end\n\
#20\n\
b00001111 !\n\
$end\n\
#30\n\
b00000000 !\n\
$end\n").unwrap();
        let trace = load_vcd(p.to_str().unwrap());
        let name = trace.signals().iter().find(|s| s.contains("v [")).unwrap().clone();

        // Initial x is a full-width vector (same shape as "bxxxxxxxx"), not a Bit.
        let sv0 = trace.signal_value(&name, 0).unwrap();
        assert_eq!(sv0, crate::trace::ScalarValue::Vector(vec![b'x'; 8]));
        let sv1 = trace.signal_value(&name, 1).unwrap();
        assert_eq!(sv1, crate::trace::ScalarValue::Vector(vec![b'x'; 8]));

        // is-x counts the initial x-run AND the explicit x at #10.
        let isx = trace.find_indices(&name, FindCondition::IsX).unwrap();
        assert_eq!(isx, vec![0, 1]);

        // The x → x start is NOT a change; changes start at the real value.
        let chg = trace.find_indices(&name, FindCondition::Changed).unwrap();
        assert_eq!(chg, vec![2, 3]);

        // change_points start at the first change record, value x.
        let cp = trace.change_points(&name).unwrap();
        let vals: Vec<(usize, crate::trace::ScalarValue)> =
            cp.iter().map(|(i, sv)| (*i, sv.clone())).collect();
        assert_eq!(vals, vec![
            (1, crate::trace::ScalarValue::Vector(vec![b'x'; 8])),
            (2, crate::trace::ScalarValue::Vector(b"00001111".to_vec())),
            (3, crate::trace::ScalarValue::Vector(b"00000000".to_vec())),
        ]);
    }

    #[test]
    fn test_glitch_timestamp_last_value_wins() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let uniq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("wal_glitch_{}_{}.vcd", std::process::id(), uniq));
        std::fs::write(&p, "$timescale 1ns $end\n\
$scope module top $end\n\
$var wire 4 ! data $end\n\
$upscope $end\n\
$enddefinitions $end\n\
$dumpvars\n\
#0\n\
b0000 !\n\
$end\n\
#10\n\
b1100 !\n\
b0011 !\n\
$end\n\
#20\n\
b1010 !\n\
$end\n\
#30\n\
b0000 !\n\
$end\n").unwrap();
        let trace = load_vcd(p.to_str().unwrap());
        let name = trace.signals().iter().find(|s| s.ends_with("data")).unwrap().clone();

        // Glitch at #10 (index 1): value line 1100 then 0011 → last wins.
        let sv = trace.signal_value(&name, 1).unwrap();
        assert_eq!(sv, crate::trace::ScalarValue::Vector(b"0011".to_vec()), "get at glitch ts must be last value");

        // find_indices uses per-index last values: 0011 == 3 matches exactly idx 1.
        let idx = trace.find_indices(&name, FindCondition::ValueI64(3)).unwrap();
        assert_eq!(idx, vec![1usize], "value 3 must match only the glitch index");
        // 1100 == 12 never holds (overwritten within #10).
        let idx = trace.find_indices(&name, FindCondition::ValueI64(12)).unwrap();
        assert!(idx.is_empty(), "overwritten 1100 must not match");
        // Same read through the decoded-signal cache (built by find_indices):
        // the cache must also keep the LAST value per index.
        let sv = trace.signal_value(&name, 1).unwrap();
        assert_eq!(sv, crate::trace::ScalarValue::Vector(b"0011".to_vec()), "cached get at glitch ts must be last value");
        // Change points: per-index transitions (no spurious entry for the glitch).
        let cp = trace.change_points(&name).unwrap();
        let vals: Vec<(usize, crate::trace::ScalarValue)> = cp
            .iter().map(|(i, sv)| (*i, sv.clone())).collect();
        assert_eq!(vals, vec![
            (0, crate::trace::ScalarValue::Vector(b"0000".to_vec())),
            (1, crate::trace::ScalarValue::Vector(b"0011".to_vec())),
            (2, crate::trace::ScalarValue::Vector(b"1010".to_vec())),
            (3, crate::trace::ScalarValue::Vector(b"0000".to_vec())),
        ], "change_points must be per-index last values");
    }
}

#[cfg(test)]
mod cache_path_tests {
    use super::*;

    /// 缓存位置必须是"执行目录"(CWD 相对), 绝不落在波形目录里
    /// (波形可能来自只读挂载; 用户明确要求写 CWD)。
    #[test]
    fn cache_dir_is_cwd_relative_and_keyed_by_basename_only() {
        if std::env::var_os("WAL_CACHE_DIR").is_some() {
            return; // 并发用例可能设置该变量, 跳过避免误判
        }
        let d = cache_dir();
        assert!(!d.is_absolute(), "默认缓存目录必须是 CWD 相对路径: {:?}", d);
        assert_eq!(d.file_name().and_then(|f| f.to_str()), Some(".wal-rust-cache"));

        // 缓存文件名只含波形 basename(不含任何目录成分)
        let dir = std::env::temp_dir().join(format!("wal_cache_path_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wave = dir.join("wave.vcd");
        std::fs::write(&wave, "x").unwrap();
        if let Some(p) = cache_file_for(&wave) {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            assert!(name.starts_with("wave.vcd-"), "缓存名应只取 basename: {}", name);
            assert!(!name.contains('/') && !name.contains('\\'));
            assert!(p.parent().map(|q| q == cache_dir()).unwrap_or(false),
                "缓存必须位于 cache_dir(): {:?}", p);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
