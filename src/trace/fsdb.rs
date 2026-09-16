//! FSDB 后端: 通过 Synopsys **NPI**(`libNPI.so`)读 FSDB。
//!
//! 设计要点(全部实测得出, 见 `examples/npi_probe.rs` 的探针结论):
//!
//! * **纯 Rust FFI**: 运行期 `dlopen` + `dlsym` 调 Itanium mangled 符号,
//!   产品不需要 C++ 编译器, 构建期也不依赖 Verdi;找不到库时优雅退化成
//!   "FSDB 暂不支持"的报错(不影响 VCD/FST 用户)。
//! * **只用 `npiFsdbTimeBasedVcIter`**: 这是多信号"时间→信号"归并迭代器,
//!   一次遍历同时给出 ①所有信号变更时间的并集(全局时间线) ②被查询信号的
//!   变更列。它有个硬约束 —— **用过之后同一进程里 `npi_fsdb_create_vct`
//!   会失效**(实测), 所以本后端全程不碰 create_vct。
//! * **规则 A**: FSDB 里 t=0 的值条目是 `$dumpvars` 初值快照, 不是 INDEX;
//!   时间线从第一个真实变更时刻开始, 索引 0 的值 = 该索引处最后一次写入,
//!   首变化之前 = 初值(无初值条目 → x)。与 VCD 后端的 `eval_change_list`
//!   逐索引语义逐条对齐。
//! * **懒扫描**: `load` 只做树遍历(便宜);时间线/变更列在首次按索引查询时
//!   构建(`Trace::prepare` 会把"建时间线"和"取被查询信号的变更列"合到一次
//!   遍历里)。

use crate::trace::{BatchEntry, FindCondition, ScalarValue, Trace, TraceId};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::Path;
use std::ptr;
use std::rc::Rc;
use std::sync::OnceLock;

// ============================ NPI FFI ============================

type NpiTime = u64;

/// `npiFsdbValue.format` 是**入参**(要什么格式), 不是出参 —— 官方 example
/// `npi_fsdb_vct_value/demo.cpp` 里先 `val.format = npiFsdbBinStrVal` 再调用。
#[repr(C)]
#[derive(Clone, Copy)]
union NpiValueUnion {
    str_: *const c_char,
    sint: i32,
    uint: u32,
    sint64: i64,
    uint64: u64,
    real: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NpiFsdbValue {
    format: i32,
    value: NpiValueUnion,
}

impl NpiFsdbValue {
    fn new(format: i32) -> Self {
        NpiFsdbValue { format, value: NpiValueUnion { str_: ptr::null() } }
    }
}

const VAL_BINSTR: i32 = 0;
const VAL_REAL: i32 = 6;
const VAL_STRING: i32 = 7;
const VAL_UINT64: i32 = 10;

// npiFsdbSigPropertyType
const SIG_NAME: c_int = 0;
#[allow(dead_code)]
const SIG_FULLNAME: c_int = 1;
const SIG_IS_REAL: c_int = 2;
#[allow(dead_code)]
const SIG_LEFT: c_int = 4;
#[allow(dead_code)]
const SIG_RIGHT: c_int = 5;
const SIG_SIZE: c_int = 6;
const SIG_IS_STRING: c_int = 7;

// npiFsdbScopePropertyType
const SCOPE_NAME: c_int = 0;
const SCOPE_FULLNAME: c_int = 1;

// npiFsdbFilePropertyType
#[allow(dead_code)]
const FILE_NAME: c_int = 0;
const FILE_SCALE_UNIT: c_int = 1;
const FILE_VERSION: c_int = 10;

/// `npiFsdbTimeBasedVcIter` 的 C++ 成员(Itanium mangled, 非虚函数 → 可直接 dlsym)。
/// 类布局只有一个 `Impl* m_impl`(8 字节), 纯 Rust 分配缓冲 + 调构造/析构即可。
struct TimeIterSyms {
    /// `set_max_session_load(num)`: 限制迭代器一次装载多少个 session 的数据。
    /// NPI 默认会把加进来的信号的 VC 数据都攒在内存里(内网 174MB 波形实测峰值
    /// 17.2GB RSS), 调小可以显著压内存。
    set_max_session_load: Option<unsafe extern "C" fn(*mut c_void, u32)>,
    ctor: unsafe extern "C" fn(*mut c_void),
    dtor: unsafe extern "C" fn(*mut c_void),
    add: unsafe extern "C" fn(*mut c_void, *mut c_void) -> i64,
    start: unsafe extern "C" fn(*mut c_void, NpiTime, NpiTime),
    next: unsafe extern "C" fn(*mut c_void, *mut NpiTime, *mut *mut c_void) -> i64,
    get_value: unsafe extern "C" fn(*mut c_void, *mut NpiFsdbValue) -> c_int,
    stop: unsafe extern "C" fn(*mut c_void),
}

struct Npi {
    #[allow(dead_code)]
    handle: *mut c_void,
    open: unsafe extern "C" fn(*const c_char) -> *mut c_void,
    close: unsafe extern "C" fn(*mut c_void) -> c_int,
    is_fsdb: Option<unsafe extern "C" fn(*const c_char) -> c_int>,
    min_time: unsafe extern "C" fn(*mut c_void, *mut NpiTime) -> c_int,
    max_time: unsafe extern "C" fn(*mut c_void, *mut NpiTime) -> c_int,
    file_property_str: unsafe extern "C" fn(c_int, *mut c_void) -> *const c_char,
    /// `npi_fsdb_unload_vc(file)`: 丢掉 NPI 为这个文件缓存的 VC 数据
    unload_vc: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    sig_property: unsafe extern "C" fn(c_int, *mut c_void, *mut c_int) -> c_int,
    sig_property_str: unsafe extern "C" fn(c_int, *mut c_void) -> *const c_char,
    /// `npi_fsdb_sig_by_name(file, name, scope)`: 按全名取句柄(不遍历树)
    sig_by_name: unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void) -> *mut c_void,
    iter_top_scope: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    iter_child_scope: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    iter_scope_next: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    iter_scope_stop: unsafe extern "C" fn(*mut c_void) -> c_int,
    scope_property_str: unsafe extern "C" fn(c_int, *mut c_void) -> *const c_char,
    iter_sig: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    iter_sig_next: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    iter_sig_stop: unsafe extern "C" fn(*mut c_void) -> c_int,
    iter: TimeIterSyms,
}

// dlopen 句柄 + 函数指针: 全进程一份, 永不 dlclose/卸载。
unsafe impl Send for Npi {}
unsafe impl Sync for Npi {}

static NPI: OnceLock<Result<Npi, String>> = OnceLock::new();

/// FSDB 支持是否可用(库能找到 + `npi_init` 成功)。结果缓存, 第二次是纯查询。
pub fn npi_available() -> bool {
    npi().is_ok()
}

/// 不可用的原因(用于报错文案)。
pub fn npi_unavailable_reason() -> String {
    match npi() {
        Ok(_) => String::new(),
        Err(e) => e.clone(),
    }
}

fn npi() -> Result<&'static Npi, String> {
    NPI.get_or_init(Npi::load).as_ref().map_err(|e| e.clone())
}

/// 库候选路径: `WAL_NPI_LIB` → `$VERDI_HOME/share/{vcst,NPI}/…/libNPI.so`
/// → 从 PATH 上的 `verdi`/`fsdbdebug` 反推 `VERDI_HOME`。
fn lib_candidates() -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(p) = std::env::var("WAL_NPI_LIB") {
        v.push(p);
    }
    let home = std::env::var("VERDI_HOME").ok().or_else(|| {
        std::env::var("PATH").ok().and_then(|p| {
            for d in p.split(':') {
                if let Some(parent) = Path::new(d).parent() {
                    if parent.join("share/NPI/inc/npi_fsdb.h").exists() {
                        return Some(parent.to_string_lossy().to_string());
                    }
                }
            }
            None
        })
    });
    if let Some(h) = home {
        v.push(format!("{}/share/NPI/lib/linux64/libNPI.so", h));
        v.push(format!("{}/share/vcst/linux64/libNPI.so", h));
    }
    v
}

/// NPI 要在 `LD_LIBRARY_PATH` 的某个目录下找到 `etc/`(Verdi 资源目录), 否则
/// `[NPI ERROR] Failed to find Verdi resource directory (etc/)`。这里自动补齐:
/// `share/NPI/lib/linux64/etc` 通常是指向 `$VERDI_HOME/etc` 的软链。
fn ensure_ld_library_path(lib: &Path) {
    let mut add: Option<String> = None;
    if let Some(d) = lib.parent() {
        if d.join("etc").exists() {
            add = Some(d.to_string_lossy().to_string());
        }
    }
    if add.is_none() {
        if let Ok(h) = std::env::var("VERDI_HOME") {
            let d = format!("{}/share/NPI/lib/linux64", h);
            if Path::new(&d).join("etc").exists() {
                add = Some(d);
            }
        }
    }
    let Some(dir) = add else { return };
    let old = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
    if old.split(':').any(|p| p == dir) {
        return;
    }
    let joined = if old.is_empty() { dir } else { format!("{}:{}", dir, old) };
    // SAFETY: 只在 load() 的启动路径上、npi_init 之前调用(此时还没有跨线程使用)。
    unsafe { std::env::set_var("LD_LIBRARY_PATH", joined) };
}

impl Npi {
    fn load() -> Result<Npi, String> {
        let candidates = lib_candidates();
        if candidates.is_empty() {
            return Err(
                "未找到 libNPI.so: 请设置 $VERDI_HOME(Verdi 安装根)或 $WAL_NPI_LIB".to_string(),
            );
        }
        unsafe {
            let mut handle = ptr::null_mut();
            let mut derr = String::new();
            let mut hit_path = None;
            let t_dl = std::time::Instant::now();
            for p in &candidates {
                let c = CString::new(p.as_str()).map_err(|_| format!("库路径含 NUL: {}", p))?;
                // RTLD_LAZY: 123MB 的 libNPI.so 用 RTLD_NOW 会把所有 PLT 重定位在
                // 加载时做完(实测 ~1s), 而我们用到的符号都是 dlsym 显式取的 →
                // 懒绑定既省这一秒, 也不影响"符号缺失即报错"。
                handle = libc::dlopen(c.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
                if !handle.is_null() {
                    hit_path = Some(p.clone());
                    break;
                }
                derr = CStr::from_ptr(libc::dlerror()).to_string_lossy().to_string();
            }
            if std::env::var("WAL_DEBUG_FSDB").is_ok() {
                eprintln!("[fsdb] dlopen {}ms ({})", t_dl.elapsed().as_millis(), candidates.join(", "));
            }
            let Some(hit_path) = hit_path else {
                return Err(format!(
                    "dlopen libNPI.so 失败(试过 {:?}): {}",
                    candidates, derr
                ));
            };
            ensure_ld_library_path(Path::new(&hit_path));

            macro_rules! sym {
                ($name:expr, $ty:ty) => {{
                    let cs = CString::new($name).unwrap();
                    let p = libc::dlsym(handle, cs.as_ptr());
                    if p.is_null() {
                        return Err(format!(
                            "libNPI.so({}) 缺必需符号 {}: 这个 Verdi 版本的 NPI 可能太老",
                            hit_path, $name
                        ));
                    }
                    std::mem::transmute::<*mut c_void, $ty>(p)
                }};
            }
            macro_rules! sym_opt {
                ($name:expr, $ty:ty) => {{
                    let cs = CString::new($name).unwrap();
                    let p = libc::dlsym(handle, cs.as_ptr());
                    if p.is_null() { None::<$ty> } else { Some(std::mem::transmute::<*mut c_void, $ty>(p)) }
                }};
            }

            let iter = TimeIterSyms {
                set_max_session_load: sym_opt!("_ZN22npiFsdbTimeBasedVcIter20set_max_session_loadEj", unsafe extern "C" fn(*mut c_void, u32)),
                ctor: sym!("_ZN22npiFsdbTimeBasedVcIterC1Ev", unsafe extern "C" fn(*mut c_void)),
                dtor: sym!("_ZN22npiFsdbTimeBasedVcIterD1Ev", unsafe extern "C" fn(*mut c_void)),
                add: sym!("_ZN22npiFsdbTimeBasedVcIter3addEPv", unsafe extern "C" fn(*mut c_void, *mut c_void) -> i64),
                start: sym!("_ZN22npiFsdbTimeBasedVcIter10iter_startEyy", unsafe extern "C" fn(*mut c_void, NpiTime, NpiTime)),
                next: sym!("_ZN22npiFsdbTimeBasedVcIter9iter_nextERyRPv", unsafe extern "C" fn(*mut c_void, *mut NpiTime, *mut *mut c_void) -> i64),
                get_value: sym!("_ZN22npiFsdbTimeBasedVcIter9get_valueER12npiFsdbValue", unsafe extern "C" fn(*mut c_void, *mut NpiFsdbValue) -> c_int),
                stop: sym!("_ZN22npiFsdbTimeBasedVcIter9iter_stopEv", unsafe extern "C" fn(*mut c_void)),
            };

            let npi = Npi {
                handle,
                open: sym!("_Z13npi_fsdb_openPKc", unsafe extern "C" fn(*const c_char) -> *mut c_void),
                close: sym!("_Z14npi_fsdb_closePv", unsafe extern "C" fn(*mut c_void) -> c_int),
                is_fsdb: sym_opt!("_Z16npi_fsdb_is_fsdbPKc", unsafe extern "C" fn(*const c_char) -> c_int),
                min_time: sym!("_Z17npi_fsdb_min_timePvPy", unsafe extern "C" fn(*mut c_void, *mut NpiTime) -> c_int),
                max_time: sym!("_Z17npi_fsdb_max_timePvPy", unsafe extern "C" fn(*mut c_void, *mut NpiTime) -> c_int),
                file_property_str: sym!("_Z26npi_fsdb_file_property_str23npiFsdbFilePropertyTypePv", unsafe extern "C" fn(c_int, *mut c_void) -> *const c_char),
                unload_vc: sym_opt!("_Z20npi_fsdb_unload_vcPv", unsafe extern "C" fn(*mut c_void) -> c_int),
                sig_property: sym!("_Z21npi_fsdb_sig_property22npiFsdbSigPropertyTypePvPi", unsafe extern "C" fn(c_int, *mut c_void, *mut c_int) -> c_int),
                sig_property_str: sym!("_Z25npi_fsdb_sig_property_str22npiFsdbSigPropertyTypePv", unsafe extern "C" fn(c_int, *mut c_void) -> *const c_char),
                sig_by_name: sym!("_Z20npi_fsdb_sig_by_namePvPKcS_", unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void) -> *mut c_void),
                iter_top_scope: sym!("_Z23npi_fsdb_iter_top_scopePv", unsafe extern "C" fn(*mut c_void) -> *mut c_void),
                iter_child_scope: sym!("_Z25npi_fsdb_iter_child_scopePv", unsafe extern "C" fn(*mut c_void) -> *mut c_void),
                iter_scope_next: sym!("_Z24npi_fsdb_iter_scope_nextPv", unsafe extern "C" fn(*mut c_void) -> *mut c_void),
                iter_scope_stop: sym!("_Z24npi_fsdb_iter_scope_stopPv", unsafe extern "C" fn(*mut c_void) -> c_int),
                scope_property_str: sym!("_Z27npi_fsdb_scope_property_str24npiFsdbScopePropertyTypePv", unsafe extern "C" fn(c_int, *mut c_void) -> *const c_char),
                iter_sig: sym!("_Z17npi_fsdb_iter_sigPv", unsafe extern "C" fn(*mut c_void) -> *mut c_void),
                iter_sig_next: sym!("_Z22npi_fsdb_iter_sig_nextPv", unsafe extern "C" fn(*mut c_void) -> *mut c_void),
                iter_sig_stop: sym!("_Z22npi_fsdb_iter_sig_stopPv", unsafe extern "C" fn(*mut c_void) -> c_int),
                iter,
            };

            // npi_init(int&, char**&) 必须先调: 否则后续 API 报
            // "[NPI ERROR] Please call npi_init() before npi_fsdb_open."
            let init: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char) -> c_int =
                sym!("_Z8npi_initRiRPPc", unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char) -> c_int);
            // 只把 argv[0] 交给 NPI: 我们的 CLI 参数(`-l x.fsdb`、表达式…)对它是噪音,
            // 不同版本的 NPI 还可能去解析它们。
            let prog = std::env::args().next().unwrap_or_else(|| "wal-rust".to_string());
            let mut owned: Vec<CString> = vec![
                CString::new(prog).unwrap_or_else(|_| CString::new("wal-rust").unwrap())
            ];
            let mut argv: Vec<*mut c_char> =
                owned.iter_mut().map(|c| c.as_ptr() as *mut c_char).collect();
            argv.push(ptr::null_mut());
            let mut argc = (argv.len() - 1) as c_int;
            let mut argv_p = argv.as_mut_ptr();
            let t_init = std::time::Instant::now();
            let _box = NpiSandbox::enter(true); // init 的 banner 两个流都要静音
            let rc = init(&mut argc, &mut argv_p);
            if std::env::var("WAL_DEBUG_FSDB").is_ok() {
                eprintln!("[fsdb] npi_init {}ms (rc={})", t_init.elapsed().as_millis(), rc);
            }
            if rc == 0 {
                return Err(format!(
                    "npi_init 失败(libNPI.so 已加载: {})。常见原因: Verdi 许可不可用、\
                     $VERDI_HOME 指向的安装不完整",
                    hit_path
                ));
            }
            Ok(npi)
        }
    }

    fn cstr(p: *const c_char) -> String {
        if p.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(p) }.to_string_lossy().to_string()
        }
    }
}

// ============================ 时间线落盘缓存 ============================
//
// 索引空间(全局时间线)对 FSDB 后端就是"把整个 FSDB 的每条变更都过一遍",
// 而它只跟**波形文件内容**有关 —— 与查询无关。所以第一次算完就落盘:
// 之后任何进程的 `find`/电平条件查询直接读缓存。
//
// 文件布局(全部小端):
//   magic  8B  "WALFTL01"
//   fp     8B  波形指纹(首尾 64KB 的 FNV, 与 VCD 旁挂缓存同口径)
//   first  9B  1B 标志 + 8B "全局最早变更时间"(0 / 1 + 值)
//   n      8B  时间点个数
//   body   n 个 delta-varint(时间点升序, 首条为绝对值)

const TL_MAGIC: &[u8; 8] = b"WALFTL01";
const TREE_MAGIC: &[u8; 8] = b"WALFNM01";

/// 名字树缓存的内容: 全名 + 位宽 + scope 列表。
/// 句柄**不进缓存**(跨进程无效) —— 恢复后第一次用到再按名字问 NPI。
pub(crate) struct TreeSnapshot {
    pub names: Vec<String>,
    pub widths: Vec<usize>,
    pub scopes: Vec<String>,
}

pub(crate) fn encode_tree(t: &TreeSnapshot) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + t.names.len() * 24);
    out.extend_from_slice(TREE_MAGIC);
    out.extend_from_slice(&[0u8; 8]); // 指纹占位
    put_varint(&mut out, t.names.len() as u64);
    for (n, w) in t.names.iter().zip(t.widths.iter()) {
        put_varint(&mut out, n.len() as u64);
        out.extend_from_slice(n.as_bytes());
        put_varint(&mut out, *w as u64);
    }
    put_varint(&mut out, t.scopes.len() as u64);
    for sc in &t.scopes {
        put_varint(&mut out, sc.len() as u64);
        out.extend_from_slice(sc.as_bytes());
    }
    out
}

pub(crate) fn decode_tree(buf: &[u8], expect_fp: u64) -> Option<TreeSnapshot> {
    if buf.len() < 24 || &buf[..8] != TREE_MAGIC {
        return None;
    }
    if u64::from_le_bytes(buf[8..16].try_into().ok()?) != expect_fp {
        return None;
    }
    let mut pos = 16usize;
    let n = get_varint(buf, &mut pos)? as usize;
    if n > buf.len() {
        return None;
    }
    let mut names = Vec::with_capacity(n);
    let mut widths = Vec::with_capacity(n);
    for _ in 0..n {
        let l = get_varint(buf, &mut pos)? as usize;
        let end = pos.checked_add(l)?;
        names.push(String::from_utf8(buf.get(pos..end)?.to_vec()).ok()?);
        pos = end;
        widths.push(get_varint(buf, &mut pos)? as usize);
    }
    let ns = get_varint(buf, &mut pos)? as usize;
    if ns > buf.len() {
        return None;
    }
    let mut scopes = Vec::with_capacity(ns);
    for _ in 0..ns {
        let l = get_varint(buf, &mut pos)? as usize;
        let end = pos.checked_add(l)?;
        scopes.push(String::from_utf8(buf.get(pos..end)?.to_vec()).ok()?);
        pos = end;
    }
    Some(TreeSnapshot { names, widths, scopes })
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn get_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v: u64 = 0;
    let mut shift = 0u32;
    loop {
        let b = *buf.get(*pos)?;
        *pos += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

/// 时间线 → 字节流(delta 编码: 时间戳单调, 差值多为小数字)
pub(crate) fn encode_timeline(times: &[u64], first_change: Option<u64>) -> Vec<u8> {
    let mut out = Vec::with_capacity(times.len() + 32);
    out.extend_from_slice(TL_MAGIC);
    // 指纹由调用方写(第 9..17 字节占位), 这里先留 8 字节
    out.extend_from_slice(&[0u8; 8]);
    match first_change {
        Some(t) => {
            out.push(1);
            out.extend_from_slice(&t.to_le_bytes());
        }
        None => {
            out.push(0);
            out.extend_from_slice(&0u64.to_le_bytes());
        }
    }
    out.extend_from_slice(&(times.len() as u64).to_le_bytes());
    let mut prev = 0u64;
    for &t in times {
        put_varint(&mut out, t - prev);
        prev = t;
    }
    out
}

pub(crate) fn decode_timeline(buf: &[u8], expect_fp: u64) -> Option<(Vec<u64>, Option<u64>)> {
    if buf.len() < 8 + 8 + 9 + 8 || &buf[..8] != TL_MAGIC {
        return None;
    }
    let fp = u64::from_le_bytes(buf[8..16].try_into().ok()?);
    if fp != expect_fp {
        return None;
    }
    let mut pos = 16usize;
    let has_first = buf[pos] != 0;
    pos += 1;
    let first_val = u64::from_le_bytes(buf[pos..pos + 8].try_into().ok()?);
    pos += 8;
    let n = u64::from_le_bytes(buf[pos..pos + 8].try_into().ok()?) as usize;
    pos += 8;
    // 防呆: 每条 varint 至少 1 字节
    if n > buf.len() - pos {
        return None;
    }
    let mut times = Vec::with_capacity(n);
    let mut prev = 0u64;
    for _ in 0..n {
        let d = get_varint(buf, &mut pos)?;
        prev = prev.checked_add(d)?;
        times.push(prev);
    }
    Some((times, if has_first { Some(first_val) } else { None }))
}

/// 缓存根目录的**绝对**路径。
///
/// 必须绝对: NPI 沙箱会把 CWD 切到 `./.wal-rust-cache/npi/`, 期间用相对路径
/// 写缓存会落进沙箱目录里(实测踩过: 文件名对、文件却"不见了")。
fn abs_cache_root() -> std::path::PathBuf {
    let d = crate::trace::vcd::cache_dir();
    if d.is_absolute() {
        return d;
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(d),
        Err(_) => d,
    }
}

/// 通用缓存路径: `<cache>/<basename>-<size>-<mtime>-v1<ext>`
fn cache_path(cache_root: &std::path::Path, filename: &str, ext: &str) -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(filename);
    let meta = std::fs::metadata(p).ok()?;
    let mtime = meta.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())?;
    let base = p.file_name()?.to_string_lossy().replace('/', "_");
    Some(cache_root.join(format!("{}-{}-{}-v1{}", base, meta.len(), mtime, ext)))
}

fn try_load_tree_cache(cache_root: &std::path::Path, filename: &str) -> Option<TreeSnapshot> {
    if crate::trace::vcd::cache_mode() == crate::trace::vcd::CacheMode::Off {
        return None;
    }
    let f = cache_path(cache_root, filename, ".fnames")?;
    let buf = std::fs::read(&f).ok()?;
    let fp = crate::trace::vcd::wave_fingerprint(std::path::Path::new(filename));
    let snap = decode_tree(&buf, fp);
    if snap.is_none() && std::env::var("WAL_DEBUG_FSDB").is_ok() {
        eprintln!("[fsdb] 名字树缓存未命中/损坏: {}", f.display());
    }
    snap
}

fn save_tree_cache(
    cache_root: &std::path::Path,
    filename: &str,
    names: &[String],
    widths: &[usize],
    scopes: &[String],
) {
    use crate::trace::vcd::CacheMode;
    if crate::trace::vcd::cache_mode() == CacheMode::Read {
        return;
    }
    let Some(f) = cache_path(cache_root, filename, ".fnames") else { return };
    if let Some(dir) = f.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let fp = crate::trace::vcd::wave_fingerprint(std::path::Path::new(filename));
    let snap = TreeSnapshot {
        names: names.to_vec(),
        widths: widths.to_vec(),
        scopes: scopes.to_vec(),
    };
    let mut blob = encode_tree(&snap);
    blob[8..16].copy_from_slice(&fp.to_le_bytes());
    let tmp = f.with_extension("fnames.tmp");
    if std::fs::write(&tmp, &blob).is_ok() {
        let _ = std::fs::rename(&tmp, &f);
    }
    if std::env::var("WAL_DEBUG_FSDB").is_ok() {
        eprintln!("[fsdb] 名字树缓存写入 {} 信号 → {}", names.len(), f.display());
    }
}

/// 两个**已升序去重**的时间序列 → 归并后的升序去重序列。
/// 时间线可能有上千万个点, 逐块对全表 `sort_unstable` 是 O(块数 × n log n);
/// 增量线性归并是 O(n)。
fn merge_sorted_unique(a: &[u64], b: &[u64]) -> Vec<u64> {
    if a.is_empty() {
        return b.to_vec();
    }
    if b.is_empty() {
        return a.to_vec();
    }
    let mut out: Vec<u64> = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0usize, 0usize);
    let mut push = |v: u64, out: &mut Vec<u64>| {
        if out.last() != Some(&v) {
            out.push(v);
        }
    };
    while i < a.len() && j < b.len() {
        if a[i] <= b[j] {
            push(a[i], &mut out);
            i += 1;
        } else {
            push(b[j], &mut out);
            j += 1;
        }
    }
    while i < a.len() {
        push(a[i], &mut out);
        i += 1;
    }
    while j < b.len() {
        push(b[j], &mut out);
        j += 1;
    }
    out
}

/// `npiFsdbTimeBasedVcIter` 实例(RAII: drop 时 iter_stop + 析构)
struct IterObj {
    buf: Box<[u64; 8]>,
    syms: &'static TimeIterSyms,
}

impl IterObj {
    fn new(npi: &'static Npi) -> Self {
        let mut buf = Box::new([0u64; 8]);
        unsafe { (npi.iter.ctor)(buf.as_mut_ptr() as *mut c_void) };
        IterObj { buf, syms: &npi.iter }
    }
    fn this(&mut self) -> *mut c_void {
        self.buf.as_mut_ptr() as *mut c_void
    }
}

impl Drop for IterObj {
    fn drop(&mut self) {
        unsafe {
            (self.syms.stop)(self.this());
            (self.syms.dtor)(self.this());
        }
    }
}

/// NPI 调用期间的环境隔离(RAII, drop 时无条件恢复):
/// ① **静音 NPI 自带的输出** —— 版权 banner("NPI - Native Programming Interface,
///    Release …"/Synopsys 法律文本)在 `npi_init` 时既可能走 stdout 也可能走 stderr
///    (不同 Verdi 版本不一样;内网 S-2021.09 实测走 stderr)。批量脚本里这是纯噪音,
///    而且会破坏 `| grep` 这类管道。`WAL_DEBUG_FSDB=1` 时全部保留, 方便排障。
/// ② **CWD 切到 `./.wal-rust-cache/npi/`** —— NPI 会在 CWD 建 `<argv0>Log/` 日志
///    目录(`wal-rustLog/`), 不该落在用户目录里。切不进去(只读目录)就放弃隔离。
struct NpiSandbox {
    saved: [(c_int, c_int); 2],
    saved_cwd: Option<std::path::PathBuf>,
}

impl NpiSandbox {
    /// `silence_stderr` 只在 `npi_init` 时用: 打开文件阶段的版本警告(*WARN* …)
    /// 是有用信息, 留在 stderr 上。
    fn enter(silence_stderr: bool) -> Self {
        let quiet = std::env::var("WAL_DEBUG_FSDB").is_err();
        let mut saved = [(-1, -1); 2];
        if quiet {
            unsafe {
                let devnull = libc::open(b"/dev/null\0".as_ptr() as *const c_char, libc::O_WRONLY);
                if devnull >= 0 {
                    for (slot, fd) in [1, 2].into_iter().enumerate() {
                        if fd == 2 && !silence_stderr {
                            continue;
                        }
                        let dup = libc::dup(fd);
                        if dup >= 0 {
                            libc::dup2(devnull, fd);
                            saved[slot] = (fd, dup);
                        }
                    }
                    libc::close(devnull);
                }
            }
        }
        let mut saved_cwd = None;
        if let Ok(cwd) = std::env::current_dir() {
            let target = cwd.join(".wal-rust-cache").join("npi");
            if std::fs::create_dir_all(&target).is_ok() && std::env::set_current_dir(&target).is_ok()
            {
                saved_cwd = Some(cwd);
            }
        }
        NpiSandbox { saved, saved_cwd }
    }
}

impl Drop for NpiSandbox {
    fn drop(&mut self) {
        if let Some(cwd) = self.saved_cwd.take() {
            let _ = std::env::set_current_dir(cwd);
        }
        for (fd, dup) in self.saved {
            if dup >= 0 {
                unsafe {
                    libc::dup2(dup, fd);
                    libc::close(dup);
                }
            }
        }
    }
}

// ============================ 数值转换 ============================

fn sv_bits(v: &ScalarValue) -> Option<&[u8]> {
    match v {
        ScalarValue::Bit(b) => Some(std::slice::from_ref(b)),
        ScalarValue::Vector(x) => Some(x.as_slice()),
        ScalarValue::Real(_) => None,
    }
}

fn sv_as_bit(v: &ScalarValue) -> Option<u8> {
    let b = sv_bits(v)?;
    if b.len() == 1 { Some(b[0]) } else { None }
}

fn sv_is_defined(v: &ScalarValue) -> bool {
    match v {
        ScalarValue::Real(_) => true,
        _ => sv_bits(v).map(|b| !b.is_empty() && b.iter().all(|c| *c == b'0' || *c == b'1')).unwrap_or(false),
    }
}

/// 是否"确定的零": x/z 返回 None(调用方回退到位比较)。
fn sv_is_zero(v: &ScalarValue) -> Option<bool> {
    match v {
        ScalarValue::Real(_) => None,
        ScalarValue::Bit(b) => match *b {
            b'0' => Some(true),
            b'1' => Some(false),
            _ => None,
        },
        ScalarValue::Vector(x) => {
            if x.iter().all(|b| *b == b'0' || *b == b'1') {
                Some(x.iter().all(|b| *b == b'0'))
            } else {
                None
            }
        }
    }
}

/// 与 VCD 后端 `VcdValue::to_i64` 同口径: 1 bit x/z → None; >64 bit 高位必须为 0。
fn sv_to_i64(v: &ScalarValue) -> Option<i64> {
    let b = sv_bits(v)?;
    if b.is_empty() {
        return None;
    }
    if b.len() == 1 {
        return match b[0] {
            b'0' => Some(0),
            b'1' => Some(1),
            _ => None,
        };
    }
    if !b.iter().all(|c| *c == b'0' || *c == b'1') {
        return None;
    }
    if b.len() > 64 {
        if b[..b.len() - 64].iter().any(|c| *c == b'1') {
            return None;
        }
        let mut val: u64 = 0;
        for &c in &b[b.len() - 64..] {
            val = (val << 1) | u64::from(c == b'1');
        }
        return Some(val as i64);
    }
    let mut val: i64 = 0;
    for &c in b {
        val = (val << 1) | i64::from(c == b'1');
    }
    Some(val)
}

/// x/z 归一化: 全 x 的向量与单 bit x 视为同一状态(与 VCD 侧 Changed 口径一致)。
fn sv_norm(v: &ScalarValue) -> ScalarValue {
    match v {
        ScalarValue::Vector(x) if !x.is_empty() && x.iter().all(|b| *b == b'x' || *b == b'X') => {
            ScalarValue::Bit(b'x')
        }
        ScalarValue::Vector(x) if !x.is_empty() && x.iter().all(|b| *b == b'z' || *b == b'Z') => {
            ScalarValue::Bit(b'z')
        }
        other => other.clone(),
    }
}

fn sv_same(a: &ScalarValue, b: &ScalarValue) -> bool {
    sv_norm(a) == sv_norm(b)
}

fn find_cond_matches(val: &ScalarValue, prev_val: Option<&ScalarValue>, cond: &FindCondition) -> bool {
    let prev_bit = prev_val.and_then(sv_as_bit);
    match cond {
        FindCondition::Rising => {
            match (prev_val.and_then(sv_is_zero), sv_is_zero(val)) {
                (Some(true), Some(false)) => true,
                _ => prev_bit == Some(b'0') && sv_as_bit(val) == Some(b'1'),
            }
        }
        FindCondition::Falling => {
            match (prev_val.and_then(sv_is_zero), sv_is_zero(val)) {
                (Some(false), Some(true)) => true,
                _ => prev_bit == Some(b'1') && sv_as_bit(val) == Some(b'0'),
            }
        }
        FindCondition::High => sv_as_bit(val) == Some(b'1'),
        FindCondition::Low => sv_as_bit(val) == Some(b'0'),
        FindCondition::Value(v) => {
            let bit = sv_as_bit(val);
            if bit.is_some() {
                bit == Some(*v) || (bit == Some(b'1') && *v == 1) || (bit == Some(b'0') && *v == 0)
            } else {
                sv_to_i64(val) == Some(*v as i64)
            }
        }
        FindCondition::ValueI64(target) => sv_to_i64(val) == Some(*target),
        FindCondition::Neq(v) => {
            let bit = sv_as_bit(val);
            if bit.is_some() {
                !(bit == Some(*v) || (bit == Some(b'1') && *v == 1) || (bit == Some(b'0') && *v == 0))
            } else {
                sv_to_i64(val) != Some(*v as i64)
            }
        }
        FindCondition::NeqI64(target) => sv_to_i64(val) != Some(*target),
        FindCondition::IsX => sv_bits(val).map(|b| b.iter().any(|c| *c == b'x' || *c == b'X')).unwrap_or(false),
        FindCondition::IsZ => sv_bits(val).map(|b| b.iter().any(|c| *c == b'z' || *c == b'Z')).unwrap_or(false),
        FindCondition::Changed => match prev_val {
            Some(p) => !sv_same(p, val),
            None => false,
        },
    }
}

/// 逐索引条件求值 —— 与 `VcdTrace` 的 `eval_change_list` 同语义。
/// `points` 必须是按索引升序、同索引已折叠为最后写入的变更列(不含 t0 快照)。
fn eval_column(
    max_index: usize,
    points: &[(usize, ScalarValue)],
    initial: Option<&ScalarValue>,
    cond: &FindCondition,
) -> Vec<usize> {
    let is_edge = matches!(
        cond,
        FindCondition::Rising | FindCondition::Falling | FindCondition::Changed
    );
    let (edges, spans) = eval_column_spans(max_index, points, initial, cond);
    let mut indices = Vec::with_capacity(edges.len() + spans.iter().map(|(s, e)| e - s).sum::<usize>());
    if is_edge {
        indices = edges;
    } else {
        // 顺序与原实现一致: 先用初始段, 再按变更点顺序展开
        for (s, e) in spans {
            indices.extend(s..e);
        }
    }
    indices
}

/// 与 `eval_column` **同一套判定**, 但产出的是"边沿单点 + 电平区间"而不是逐个索引。
///
/// 为什么必须分开: 电平条件的匹配区间可能覆盖**上亿个索引**(比如整段都是 x 的信号
/// 上做 `(count (is-x s))`), 逐个 push 会直接吃光内存 —— 内网 174MB 波形实测
/// `is-x` 394s / RSS 峰值 4GB, 而它真正需要的信息只是"哪些区间成立"。
/// 计数走 `count_matches`(区间长度求和), 需要索引的 `find` 才展开。
fn eval_column_spans(
    max_index: usize,
    points: &[(usize, ScalarValue)],
    initial: Option<&ScalarValue>,
    cond: &FindCondition,
) -> (Vec<usize>, Vec<(usize, usize)>) {
    let is_edge = matches!(
        cond,
        FindCondition::Rising | FindCondition::Falling | FindCondition::Changed
    );
    let mut edges: Vec<usize> = Vec::new();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    // 与 VCD 后端逐条对齐: **首个变更恰好落在索引 0 且没有确定初值**时, 索引 0
    // 没有前驱(Changed 不成立); 其余情况前驱就是 t0 快照(没有条目 → x)。
    // `count_matches` 在时间基上必须复刻这一条, 见 `global_first_change_time`。
    let init_x = ScalarValue::Bit(b'x');
    let init_ref = initial.unwrap_or(&init_x);
    let first_is_zero = points.first().map(|(i, _)| *i) == Some(0);
    let mut prev_val: Option<ScalarValue> = if first_is_zero {
        if sv_is_defined(init_ref) { Some(init_ref.clone()) } else { None }
    } else {
        Some(init_ref.clone())
    };
    if !is_edge && find_cond_matches(init_ref, None, cond) {
        let first_idx = points.first().map(|(i, _)| *i).unwrap_or(max_index + 1);
        spans.push((0, first_idx.min(max_index + 1)));
    }
    for (k, (idx, v)) in points.iter().enumerate() {
        if *idx > max_index {
            break;
        }
        let matched = find_cond_matches(v, prev_val.as_ref(), cond);
        prev_val = Some(v.clone());
        if !matched {
            continue;
        }
        if is_edge {
            edges.push(*idx);
        } else {
            let end = points.get(k + 1).map(|(n, _)| *n).unwrap_or(max_index + 1);
            spans.push((*idx, end.min(max_index + 1)));
        }
    }
    (edges, spans)
}

// ============================ Trace 实现 ============================

struct Sig {
    /// 句柄。**可能为空** —— 从名字缓存恢复时不知道句柄, 第一次用到才
    /// `npi_fsdb_sig_by_name(全名)` 解析(实测 2000 次 114ms, 而整棵树遍历
    /// 60k 信号要 ~800ms)。
    handle: Cell<*mut c_void>,
    name: String,
    full: String,
    width: usize,
    /// 是否实数/字符串: 懒取(第一次读到这个信号的值时才问 NPI)
    real_str: Cell<Option<(bool, bool)>>,
}

/// 每信号变更列: **时间基**(原生时间升序; 不含 t0 初值快照)。
///
/// 为什么存时间而不是索引: 索引空间 = "所有信号变更时间的并集", 对流式后端
/// (FSDB 走 NPI)意味着一次全文件扫描。而 `(getwave s)`/`(at s T)`/边沿计数
/// 只要"信号自己的变更点 + 原生时间", 存时间就能完全不碰索引空间。
/// 需要索引时(`find`/逐拍取值)再用时间线换算, 见 `column_indexed`。
#[derive(Default)]
struct Column {
    points: Vec<(u64, ScalarValue)>,
}

/// 一次融合扫描的产物(建时间线时顺带取被查询信号的变更列)
#[derive(Default)]
struct ScanOut {
    /// (信号下标 → 原始 (时间, 值) 序列, t>0)
    raw: HashMap<usize, Vec<(u64, ScalarValue)>>,
    /// 信号下标 → t=0 快照值
    inits: HashMap<usize, ScalarValue>,
    /// 时间线(t>0 的去重升序时间)
    times: Vec<u64>,
    /// 本次扫描是否采了时间线(单信号扫描不能拿空 times 覆盖已有时间线)
    has_timeline: bool,
}

pub struct FsdbTrace {
    id: TraceId,
    filename: String,
    /// 缓存根目录的**绝对**路径(进 NPI 沙箱前算好: 沙箱会把 CWD 切走)
    cache_root: std::path::PathBuf,
    file: *mut c_void,
    sigs: Vec<Sig>,
    sig_names: Vec<String>,
    name_to_idx: HashMap<String, usize>,

    scopes: Vec<String>,
    ts_exp: Option<i8>,
    min_t: u64,
    max_t: u64,
    /// 首次按索引查询时构建(OnceCell 语义: 用 RefCell<Option<Rc<…>>>)
    timeline: RefCell<Option<Rc<Vec<u64>>>>,
    cols: RefCell<HashMap<usize, Rc<Column>>>,
    /// 索引基列缓存(懒换算; 只有需要索引空间的查询才会填)
    idx_cols: RefCell<HashMap<usize, Rc<Vec<(usize, ScalarValue)>>>>,
    initials: RefCell<HashMap<usize, Option<ScalarValue>>>,
    /// `Trace::prepare` 声明的信号: 建时间线时顺带取它们的变更列
    prepared: RefCell<Vec<usize>>,
    name_cache: RefCell<HashMap<String, Option<usize>>>,
    current_index: usize,
    /// 全局最早变更时间(时间线第一个时间点)的懒缓存:
    /// `Cell<Option<Option<u64>>>` = 外层"算过没", 内层"有没有变更"。
    /// VCD 的 Changed 在索引 0 有特例, 时间基计数要知道"这个信号的首个变更
    /// 是不是全局最早的那个"才能复刻它。
    first_change: Cell<Option<Option<u64>>>,
    /// 已知有效的最大索引: `set_index`/`step` 只要目标 ≤ 它就不必再问 `max_index()`
    /// —— 而 `max_index()` 会触发**全文件扫描**(物化全局时间线)。查询结束后恢复
    /// 游标的那次 `set_index` 就落在这一类, 否则每个查询都要白扫一遍整个 FSDB。
    highest_valid: Cell<usize>,
    fatal: RefCell<Option<String>>,
    /// 扫描进行中: 防止 `column()` 递归触发第二次扫描
    scanning: Cell<bool>,
}

/// 分块大小: 一次往归并迭代器里塞太多信号会占大量内存。
/// `WAL_FSDB_CHUNK` 可调(性能排查用; 分块越小 NPI 峰值内存越低, 但重复装载越多)。
fn chunk_size() -> usize {
    std::env::var("WAL_FSDB_CHUNK").ok().and_then(|v| v.parse().ok()).filter(|&n| n > 0).unwrap_or(4096)
}

/// 调完一块之后是否 `npi_fsdb_unload_vc`(丢掉 NPI 的文件级 VC 缓存)。
/// 默认开: 内网 174MB 波形实测 RSS 17.2GB, 就是这份缓存在涨。
fn unload_each_chunk() -> bool {
    std::env::var("WAL_FSDB_KEEP_VC").is_err()
}

impl FsdbTrace {
    pub fn load(path: &Path, id: TraceId) -> Result<Self, String> {
        let npi = npi()?;
        let filename = path.to_string_lossy().to_string();
        // 沙箱会把 CWD 切走, 所以必须给 NPI 绝对路径(用户可能传相对路径)
        // 缓存根必须在进 NPI 沙箱**之前**算成绝对路径(沙箱里 CWD 已经变了)
        let cache_root = abs_cache_root();
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let cpath = CString::new(abs.to_string_lossy().as_bytes())
            .map_err(|_| format!("路径含 NUL 字节: {}", filename))?;
        let _box = NpiSandbox::enter(false); // open 的版本警告留在 stderr
        unsafe {
            if let Some(f) = npi.is_fsdb {
                if f(cpath.as_ptr()) == 0 {
                    return Err(format!("{}: 不是 FSDB 文件(NPI 判定)", filename));
                }
            }
            let t_open = std::time::Instant::now();
            let file = (npi.open)(cpath.as_ptr());
            let ms_open = t_open.elapsed().as_millis();
            if file.is_null() {
                return Err(format!(
                    "{}: npi_fsdb_open 失败。常见原因: ①该 FSDB 版本比本机 Verdi 的 reader 新\
                     (NSIS: 用更老的 Verdi 打开更新的 FSDB); ②Verdi 许可不可用 \
                     (NPI 在 open 时 checkout); ③文件损坏或不是 FSDB。可设 WAL_DEBUG_FSDB=1 看细节",
                    filename
                ));
            }
            let mut min_t: NpiTime = 0;
            let mut max_t: NpiTime = 0;
            (npi.min_time)(file, &mut min_t);
            (npi.max_time)(file, &mut max_t);
            let scale = Npi::cstr((npi.file_property_str)(FILE_SCALE_UNIT, file));
            let version = Npi::cstr((npi.file_property_str)(FILE_VERSION, file));
            let ts_exp = crate::vcd::convert::parse_timescale(scale.as_bytes());
            let dbg = std::env::var("WAL_DEBUG_FSDB").is_ok();
            let t_walk = std::time::Instant::now();
            let mut sigs: Vec<Sig> = Vec::new();
            let mut scopes: Vec<String> = Vec::new();
            // 名字树缓存: 命中就**完全跳过树遍历**(60k 信号实测 ~800ms)。句柄不进
            // 缓存, 第一次用到某个信号时按名字解析(`npi_fsdb_sig_by_name`)。
            let mut quoted_names: Vec<String> = Vec::new();
            let mut quoted_widths: Vec<usize> = Vec::new();
            if let Some(snap) = try_load_tree_cache(&cache_root, &filename) {
                quoted_names = snap.names;
                quoted_widths = snap.widths;
                scopes = snap.scopes;
                for (i, full) in quoted_names.iter().enumerate() {
                    sigs.push(Sig {
                        handle: Cell::new(ptr::null_mut()),
                        name: full.rsplit('.').next().unwrap_or(full).to_string(),
                        full: full.clone(),
                        width: quoted_widths.get(i).copied().unwrap_or(1),
                        real_str: Cell::new(None),
                    });
                }
            } else {
                let top = (npi.iter_top_scope)(file);
                if !top.is_null() {
                    loop {
                        let s = (npi.iter_scope_next)(top);
                        if s.is_null() {
                            break;
                        }
                        walk_scope(npi, s, &mut sigs, &mut scopes);
                    }
                    (npi.iter_scope_stop)(top);
                }
            }
            if dbg {
                eprintln!(
                    "[fsdb] {}: open {}ms, 树遍历 {}ms ({} 信号 / {} scope), version={} scale={} ({:?}) min={} max={}",
                    filename,
                    ms_open,
                    t_walk.elapsed().as_millis(),
                    sigs.len(),
                    scopes.len(),
                    version,
                    scale,
                    ts_exp,
                    min_t,
                    max_t
                );
            }
            if quoted_names.is_empty() && !sigs.is_empty() {
                // 刚走完树 → 落盘, 之后的进程直接跳过遍历
                let names: Vec<String> = sigs.iter().map(|s| s.full.clone()).collect();
                let widths: Vec<usize> = sigs.iter().map(|s| s.width).collect();
                save_tree_cache(&cache_root, &filename, &names, &widths, &scopes);
            }
            if sigs.is_empty() {
                (npi.close)(file);
                return Err(format!("{}: FSDB 里没有任何信号", filename));
            }
            let mut name_to_idx = HashMap::with_capacity(sigs.len());
            let mut sig_names = Vec::with_capacity(sigs.len());
            for (i, s) in sigs.iter().enumerate() {
                name_to_idx.entry(s.full.clone()).or_insert(i);
                sig_names.push(s.full.clone());
            }
            Ok(FsdbTrace {
                id,
                filename,
                cache_root,
                file,
                sigs,
                sig_names,
                name_to_idx,
                scopes,
                ts_exp,
                min_t,
                max_t,
                timeline: RefCell::new(None),
                cols: RefCell::new(HashMap::new()),
                idx_cols: RefCell::new(HashMap::new()),
                initials: RefCell::new(HashMap::new()),
                prepared: RefCell::new(Vec::new()),
                name_cache: RefCell::new(HashMap::new()),
                first_change: Cell::new(None),
                current_index: 0,
                highest_valid: Cell::new(0),
                fatal: RefCell::new(None),
                scanning: Cell::new(false),
            })
        }
    }

    /// 名字解析: 精确 → 叶子名(短名/无点) → 子串(与 VCD/FST 同口径), 结果缓存。
    fn resolve_idx(&self, name: &str) -> Option<usize> {
        if let Some(i) = self.name_to_idx.get(name) {
            return Some(*i);
        }
        if let Some(c) = self.name_cache.borrow().get(name) {
            return *c;
        }
        fn leaf(s: &str) -> &str {
            s.rsplitn(2, '.').next().unwrap_or("")
        }
        let allow_leaf = name.len() <= 8 || !name.contains('.');
        let mut hit = None;
        let mut sub = None;
        for (i, s) in self.sig_names.iter().enumerate() {
            if allow_leaf && leaf(s) == name {
                hit = Some(i);
                break;
            }
            if sub.is_none() && s.contains(name) {
                sub = Some(i);
            }
        }
        let found = hit.or(sub);
        self.name_cache.borrow_mut().insert(name.to_string(), found);
        found
    }

    /// 读当前迭代位置的值(`get_value` 的字符串缓冲必须立刻拷走)。
    fn read_value(&self, npi: &Npi, it: &mut IterObj, idx: usize) -> ScalarValue {
        let (is_real, is_string) = self.real_str_of(idx);
        let wanted = if is_real {
            VAL_REAL
        } else if is_string {
            VAL_STRING
        } else {
            VAL_BINSTR
        };
        let mut v = NpiFsdbValue::new(wanted);
        let mut rc = unsafe { (npi.iter.get_value)(it.this(), &mut v) };
        if rc == 0 && wanted != VAL_UINT64 {
            // 位串失败: 试实数, 再试无符号整数(analog/枚举信号的兜底)
            v = NpiFsdbValue::new(VAL_REAL);
            rc = unsafe { (npi.iter.get_value)(it.this(), &mut v) };
            if rc == 0 {
                v = NpiFsdbValue::new(VAL_UINT64);
                rc = unsafe { (npi.iter.get_value)(it.this(), &mut v) };
            }
        }
        if rc == 0 {
            return ScalarValue::Bit(b'x');
        }
        unsafe {
            match v.format {
                VAL_REAL => ScalarValue::Real(v.value.real),
                VAL_STRING => {
                    let s = Npi::cstr(v.value.str_);
                    if s.as_bytes().len() == 1 {
                        ScalarValue::Bit(s.as_bytes()[0])
                    } else {
                        ScalarValue::Vector(s.into_bytes())
                    }
                }
                VAL_UINT64 => {
                    let bits = format!("{:b}", v.value.uint64);
                    ScalarValue::Vector(bits.into_bytes())
                }
                VAL_BINSTR => {
                    let s = Npi::cstr(v.value.str_);
                    let b = s.as_bytes();
                    if b.len() == 1 {
                        ScalarValue::Bit(b[0])
                    } else if b.is_empty() {
                        ScalarValue::Bit(b'x')
                    } else {
                        ScalarValue::Vector(b.to_vec())
                    }
                }
                _ => ScalarValue::Bit(b'x'),
            }
        }
    }

    /// 融合扫描: `want_timeline` 时遍历**所有**信号(算时间线), 同时为 `want`
    /// 里的信号取变更列; 否则只遍历 `want`。
    fn scan(&self, want: &HashSet<usize>, want_timeline: bool) -> Result<ScanOut, String> {
        let npi = npi()?;
        let n = self.sigs.len();
        let mut out = ScanOut::default();
        let targets: Vec<usize> = if want_timeline {
            (0..n).collect()
        } else {
            let mut v: Vec<usize> = want.iter().copied().collect();
            v.sort_unstable();
            v
        };
        if targets.is_empty() {
            return Ok(out);
        }
        let dbg = std::env::var("WAL_DEBUG_FSDB").is_ok();
        let t_begin = std::time::Instant::now();
        let chunk = chunk_size();
        let session_load: Option<u32> = std::env::var("WAL_FSDB_SESSION_LOAD").ok().and_then(|v| v.parse().ok());
        let mut entries = 0usize;
        let mut chunk_start = 0usize;
        while chunk_start < targets.len() {
            let chunk_end = (chunk_start + chunk).min(targets.len());
            let mut it = IterObj::new(npi);
            if let (Some(f), Some(n)) = (npi.iter.set_max_session_load, session_load) {
                unsafe { f(it.this(), n) };
            }
            let t_add = std::time::Instant::now();
            // 局部 handle→下标: 句柄可能刚从名字缓存里懒解析出来, 不能依赖 load
            // 时建的那张表。每块的信号数有上限(默认 4096), 建表很便宜。
            let mut local: HashMap<usize, usize> = HashMap::with_capacity(chunk_end - chunk_start);
            for &i in &targets[chunk_start..chunk_end] {
                match self.handle_of(i) {
                    Ok(h) => {
                        local.insert(h as usize, i);
                        unsafe { (npi.iter.add)(it.this(), h) };
                    }
                    Err(e) => {
                        let mut f = self.fatal.borrow_mut();
                        if f.is_none() {
                            *f = Some(e);
                        }
                    }
                }
            }
            let ms_add = t_add.elapsed().as_millis();
            let t_start = std::time::Instant::now();
            unsafe { (npi.iter.start)(it.this(), self.min_t, self.max_t) };
            let ms_start = t_start.elapsed().as_millis();
            // 块内时间戳是单调的(时间优先归并), 顺手去重; 块间再归并一次。
            let mut chunk_times: Vec<u64> = Vec::new();
            // 纯时间线扫描(没要任何信号的值): 不做 handle→信号 映射, 每条省一次
            // 哈希查找 —— 大波形上这个循环要跑几百万次。
            if want_timeline && want.is_empty() {
                let mut last = u64::MAX;
                loop {
                    let mut t: NpiTime = 0;
                    let mut sig: *mut c_void = ptr::null_mut();
                    let rc = unsafe { (npi.iter.next)(it.this(), &mut t, &mut sig) };
                    if rc <= 0 || sig.is_null() {
                        break;
                    }
                    entries += 1;
                    if t > 0 && t != last {
                        chunk_times.push(t);
                        last = t;
                    }
                }
                out.times = merge_sorted_unique(&out.times, &chunk_times);
                drop(it);
                if unload_each_chunk() {
                    if let Some(f) = npi.unload_vc {
                        unsafe { f(self.file) };
                    }
                }
                if dbg {
                    eprintln!(
                        "[fsdb] 时间线 chunk {}/{} (+{} sigs): {} ms, 时间点 {} 条",
                        chunk_start / chunk + 1,
                        targets.len().div_ceil(chunk),
                        chunk_end - chunk_start,
                        t_begin.elapsed().as_millis(),
                        out.times.len()
                    );
                }
                chunk_start = chunk_end;
                continue;
            }
            loop {
                let mut t: NpiTime = 0;
                let mut sig: *mut c_void = ptr::null_mut();
                let rc = unsafe { (npi.iter.next)(it.this(), &mut t, &mut sig) };
                if rc <= 0 || sig.is_null() {
                    break;
                }
                let Some(&idx) = local.get(&(sig as usize)) else {
                    continue;
                };
                entries += 1;
                if t == 0 {
                    if want.contains(&idx) {
                        let v = self.read_value(npi, &mut it, idx);
                        out.inits.entry(idx).or_insert(v);
                    }
                    continue;
                }
                if want_timeline && chunk_times.last() != Some(&t) {
                    chunk_times.push(t);
                }
                if want.contains(&idx) {
                    let v = self.read_value(npi, &mut it, idx);
                    out.raw.entry(idx).or_default().push((t, v));
                }
            }
            if want_timeline {
                // 增量归并: 每块结束就把时间去重进主表 → 主表长度 = 去重后的时间点数,
                // 不随"块数 × 块内时间点数"膨胀。
                out.times = merge_sorted_unique(&out.times, &chunk_times);
            }
            drop(it);
            if unload_each_chunk() {
                if let Some(f) = npi.unload_vc {
                    unsafe { f(self.file) };
                }
            }
            if dbg {
                eprintln!(
                    "[fsdb] scan chunk {}/{} (+{} sigs): add {}ms start {}ms iter {}ms → 时间点 {} 条, 累计 {}ms",
                    chunk_start / chunk + 1,
                    targets.len().div_ceil(chunk),
                    chunk_end - chunk_start,
                    ms_add,
                    ms_start,
                    t_begin.elapsed().as_millis() as u128 - ms_add as u128 - ms_start as u128,
                    out.times.len(),
                    t_begin.elapsed().as_millis()
                );
            }
            chunk_start = chunk_end;
        }
        if want_timeline {
            out.times.sort_unstable();
            out.times.dedup();
            out.has_timeline = true;
        }
        if dbg {
            eprintln!(
                "[fsdb] scan done: 目标 {} 信号, 归并流 {} 条, 时间点 {} 条, {} ms",
                targets.len(),
                entries,
                out.times.len(),
                t_begin.elapsed().as_millis()
            );
        }
        Ok(out)
    }

    /// 时间线缓存文件: 与 VCD 旁挂缓存同一套 key(路径 basename+size+mtime)。
    fn timeline_cache_file(&self) -> Option<std::path::PathBuf> {
        cache_path(&self.cache_root, &self.filename, ".ftl")
    }

    /// 读时间线缓存(命中 → 直接返回, 不再碰 FSDB 数据)
    fn try_load_timeline_cache(&self) -> Option<(Vec<u64>, Option<u64>)> {
        if crate::trace::vcd::cache_mode() == crate::trace::vcd::CacheMode::Off {
            return None;
        }
        let f = self.timeline_cache_file()?;
        let buf = std::fs::read(&f).ok()?;
        let fp = crate::trace::vcd::wave_fingerprint(std::path::Path::new(&self.filename));
        let got = decode_timeline(&buf, fp);
        if got.is_none() && std::env::var("WAL_DEBUG_FSDB").is_ok() {
            eprintln!("[fsdb] 时间线缓存未命中/损坏, 忽略: {}", f.display());
        }
        got
    }

    /// 写时间线缓存(原子: 先写 .tmp 再 rename, 避免别的进程读到半个文件)
    fn save_timeline_cache(&self, times: &[u64], first_change: Option<u64>) {
        use crate::trace::vcd::CacheMode;
        if crate::trace::vcd::cache_mode() == CacheMode::Read {
            return;
        }
        let Some(f) = self.timeline_cache_file() else { return };
        if let Some(dir) = f.parent() {
            if std::fs::create_dir_all(dir).is_err() {
                return;
            }
        }
        let fp = crate::trace::vcd::wave_fingerprint(std::path::Path::new(&self.filename));
        let mut blob = encode_timeline(times, first_change);
        blob[8..16].copy_from_slice(&fp.to_le_bytes());
        let tmp = f.with_extension("ftl.tmp");
        if std::fs::write(&tmp, &blob).is_ok() {
            let _ = std::fs::rename(&tmp, &f);
        }
        if std::env::var("WAL_DEBUG_FSDB").is_ok() {
            eprintln!("[fsdb] 时间线缓存写入 {} 个时间点 → {}", times.len(), f.display());
        }
    }

    /// 时间线(索引 → 原生时间)。规则 A: 不含 t=0 初值快照。
    fn timeline(&self) -> Result<Rc<Vec<u64>>, String> {
        if let Some(t) = self.timeline.borrow().as_ref() {
            return Ok(t.clone());
        }
        let cached = self.try_load_timeline_cache();
        if let Some((times, first)) = cached {
            if std::env::var("WAL_DEBUG_FSDB").is_ok() {
                eprintln!("[fsdb] 时间线缓存命中: {} 个时间点", times.len());
            }
            self.first_change.set(Some(first));
            let rc = Rc::new(times);
            *self.timeline.borrow_mut() = Some(rc.clone());
            return Ok(rc);
        }
        let t_begin = std::time::Instant::now();
        let out = self.scan(&HashSet::new(), true)?;
        let times = out.times.clone();
        self.apply_scan(out);
        let t = self.timeline.borrow().clone();
        let t = t.ok_or_else(|| "FSDB 时间线构建失败".to_string())?;
        let elapsed = t_begin.elapsed();
        // 只缓存"值得缓存"的: 小时间线(<1k 点)重扫也比读文件快, 而且不污染目录。
        // `WAL_CACHE=build` 时无条件写(测试/预热用)。
        // 判据: 时间点够多, 或者这次构建确实慢(平台/文件差异都能覆盖到)。
        if times.len() >= 256
            || elapsed >= std::time::Duration::from_millis(150)
            || crate::trace::vcd::cache_mode() == crate::trace::vcd::CacheMode::Build
        {
            self.save_timeline_cache(&times, self.first_change.get().flatten());
        }
        Ok(t)
    }

    /// 把一次扫描结果落进缓存。
    fn apply_scan(&self, out: ScanOut) {
        let ScanOut { raw, inits, times, has_timeline } = out;
        if has_timeline {
            *self.timeline.borrow_mut() = Some(Rc::new(times));
        }
        for (idx, v) in inits {
            self.initials.borrow_mut().entry(idx).or_insert(Some(v));
        }
        for (idx, list) in raw {
            // 同一时间戳的多次写入(delta/glitch) → 最后一次
            let mut collapsed: Vec<(u64, ScalarValue)> = Vec::with_capacity(list.len());
            for (t, v) in list {
                match collapsed.last_mut() {
                    Some((pt, pv)) if *pt == t => *pv = v,
                    _ => collapsed.push((t, v)),
                }
            }
            // 值没变(毛刺回到原值)不算变更点
            let init = self.initials.borrow().get(&idx).cloned().flatten();
            let mut out_pts: Vec<(u64, ScalarValue)> = Vec::with_capacity(collapsed.len());
            for (t, v) in collapsed {
                let prev = out_pts.last().map(|(_, p)| p).or(init.as_ref());
                let same = prev.map(|p| sv_same(p, &v)).unwrap_or(false);
                if !same {
                    out_pts.push((t, v));
                }
            }
            self.cols.borrow_mut().entry(idx).or_insert_with(|| {
                Rc::new(Column { points: out_pts })
            });
        }
    }

    /// 取**时间基**变更列(不含 t0 快照)。**不建时间线** —— 这是
    /// `(getwave s)`/`(at s T)`/边沿计数不做全文件扫描的关键。
    fn column_time(&self, idx: usize) -> Result<Rc<Column>, String> {
        if let Some(c) = self.cols.borrow().get(&idx) {
            return Ok(c.clone());
        }
        let mut want = HashSet::new();
        want.insert(idx);
        let out = self.scan(&want, false)?;
        self.apply_scan(out);
        Ok(self
            .cols
            .borrow()
            .get(&idx)
            .cloned()
            .unwrap_or_else(|| Rc::new(Column::default())))
    }

    /// 取**索引基**变更列(把时间映射到索引空间; 会建时间线)。
    /// 只有 `find` / 逐拍取值 / 电平条件才需要。
    fn column_indexed(&self, idx: usize) -> Result<Rc<Vec<(usize, ScalarValue)>>, String> {
        if let Some(c) = self.idx_cols.borrow().get(&idx) {
            return Ok(c.clone());
        }
        let tl = self.timeline()?;
        let col = self.column_time(idx)?;
        let mut pts: Vec<(usize, ScalarValue)> = Vec::with_capacity(col.points.len());
        for (t, v) in &col.points {
            if let Ok(i) = tl.binary_search(t) {
                match pts.last_mut() {
                    Some((pi, pv)) if *pi == i => *pv = v.clone(),
                    _ => pts.push((i, v.clone())),
                }
            }
        }
        let rc = Rc::new(pts);
        self.idx_cols.borrow_mut().insert(idx, rc.clone());
        Ok(rc)
    }

    /// 信号初值(t0 快照; 没有条目 → None)    /// 信号初值(t0 快照; 没有条目 → None)
    fn initial_of(&self, idx: usize) -> Option<ScalarValue> {
        if let Some(v) = self.initials.borrow().get(&idx) {
            return v.clone();
        }
        let mut want = HashSet::new();
        want.insert(idx);
        if let Ok(out) = self.scan(&want, false) {
            let mut inits = out.inits;
            let got = inits.remove(&idx);
            self.initials.borrow_mut().insert(idx, got.clone());
            return got;
        }
        None
    }

    /// 取(必要时解析)信号句柄。
    fn handle_of(&self, idx: usize) -> Result<*mut c_void, String> {
        let h = self.sigs[idx].handle.get();
        if !h.is_null() {
            return Ok(h);
        }
        let npi = npi()?;
        let c = CString::new(self.sigs[idx].full.as_str())
            .map_err(|_| format!("信号名含 NUL: {}", self.sigs[idx].full))?;
        let h = unsafe { (npi.sig_by_name)(self.file, c.as_ptr(), ptr::null_mut()) };
        if h.is_null() {
            return Err(format!(
                "npi_fsdb_sig_by_name 取不到信号 '{}'(缓存里的名字与文件不匹配?)",
                self.sigs[idx].full
            ));
        }
        self.sigs[idx].handle.set(h);
        Ok(h)
    }

    /// 是否实数/字符串(懒取一次)
    fn real_str_of(&self, idx: usize) -> (bool, bool) {
        if let Some(v) = self.sigs[idx].real_str.get() {
            return v;
        }
        let mut v = (false, false);
        if let Ok(h) = self.handle_of(idx) {
            if let Ok(npi) = npi() {
                unsafe {
                    let mut is_real: c_int = 0;
                    let mut is_string: c_int = 0;
                    (npi.sig_property)(SIG_IS_REAL, h, &mut is_real);
                    (npi.sig_property)(SIG_IS_STRING, h, &mut is_string);
                    v = (is_real != 0, is_string != 0);
                }
            }
        }
        self.sigs[idx].real_str.set(Some(v));
        v
    }

    fn width_of(&self, idx: usize) -> usize {
        self.sigs[idx].width
    }

    fn normalize(&self, idx: usize, v: ScalarValue) -> ScalarValue {
        let w = self.width_of(idx);
        match v {
            ScalarValue::Bit(b) if (b == b'x' || b == b'z') && w > 1 => {
                ScalarValue::Vector(vec![b; w])
            }
            other => other,
        }
    }

    /// 全局最早变更时间 = 时间线的第一个时间点。
    ///
    /// 只用于一件事: `Changed` 在"首个变更恰好落在索引 0"时要与 VCD 一致。
    /// 代价刻意压到最低 —— 每块只读**第一条** `t>0` 的条目(块的 add/start 是
    /// 固定开销, 不必把整块拉完); 时间线已经建好时直接取首元素, 零成本。
    fn global_first_change_time(&self) -> Result<Option<u64>, String> {
        if let Some(v) = self.first_change.get() {
            return Ok(v);
        }
        if let Some(tl) = self.timeline.borrow().as_ref() {
            let v = tl.first().copied();
            self.first_change.set(Some(v));
            return Ok(v);
        }
        // 落盘缓存里有时间线, 顺带就知道最早变更时间 —— 不必再摸 FSDB
        if let Some((times, first)) = self.try_load_timeline_cache() {
            let v = times.first().copied().or(first);
            self.first_change.set(Some(v));
            *self.timeline.borrow_mut() = Some(Rc::new(times));
            return Ok(v);
        }
        let npi = npi()?;
        let chunk = chunk_size();
        let mut best: Option<u64> = None;
        let mut lo = 0usize;
        while lo < self.sigs.len() {
            let hi = (lo + chunk).min(self.sigs.len());
            let mut it = IterObj::new(npi);
            for i in lo..hi {
                if let Ok(h) = self.handle_of(i) {
                    unsafe { (npi.iter.add)(it.this(), h) };
                }
            }
            unsafe { (npi.iter.start)(it.this(), self.min_t, self.max_t) };
            loop {
                let mut t: NpiTime = 0;
                let mut sig: *mut c_void = ptr::null_mut();
                let rc = unsafe { (npi.iter.next)(it.this(), &mut t, &mut sig) };
                if rc <= 0 || sig.is_null() {
                    break;
                }
                if t > 0 {
                    best = Some(best.map_or(t, |b: u64| b.min(t)));
                    break;
                }
            }
            drop(it);
            if unload_each_chunk() {
                if let Some(f) = npi.unload_vc {
                    unsafe { f(self.file) };
                }
            }
            lo = hi;
        }
        if std::env::var("WAL_DEBUG_FSDB").is_ok() {
            eprintln!("[fsdb] 全局最早变更时间 = {:?}", best);
        }
        self.first_change.set(Some(best));
        Ok(best)
    }

}

unsafe fn walk_scope(npi: &'static Npi, scope: *mut c_void, sigs: &mut Vec<Sig>, scopes: &mut Vec<String>) {
    unsafe {
        let full = Npi::cstr((npi.scope_property_str)(SCOPE_FULLNAME, scope));
        let name = Npi::cstr((npi.scope_property_str)(SCOPE_NAME, scope));
        if !full.is_empty() {
            scopes.push(full.clone());
        }
        let sit = (npi.iter_sig)(scope);
        if !sit.is_null() {
            loop {
                let s = (npi.iter_sig_next)(sit);
                if s.is_null() {
                    break;
                }
                let sname = Npi::cstr((npi.sig_property_str)(SIG_NAME, s));
                let sfull = if full.is_empty() {
                    sname.clone()
                } else {
                    format!("{}.{}", full, sname)
                };
                let mut size: c_int = 0;
                (npi.sig_property)(SIG_SIZE, s, &mut size);
                sigs.push(Sig {
                    handle: Cell::new(s),
                    name: sname,
                    full: sfull,
                    width: if size > 0 { size as usize } else { 1 },
                    real_str: Cell::new(None),
                });
            }
            (npi.iter_sig_stop)(sit);
        }
        let cit = (npi.iter_child_scope)(scope);
        if !cit.is_null() {
            loop {
                let child = (npi.iter_scope_next)(cit);
                if child.is_null() {
                    break;
                }
                walk_scope(npi, child, sigs, scopes);
            }
            (npi.iter_scope_stop)(cit);
        }
    }
}

impl Drop for FsdbTrace {
    fn drop(&mut self) {
        if let Ok(npi) = npi() {
            unsafe { (npi.close)(self.file) };
        }
    }
}

impl Trace for FsdbTrace {
    fn id(&self) -> &TraceId {
        &self.id
    }

    fn filename(&self) -> &str {
        &self.filename
    }

    fn fatal_error(&self) -> Option<String> {
        self.fatal.borrow().clone()
    }

    /// 查询前置声明: 记录信号; 首次调用就把时间线和这些信号的变更列一起算出来
    /// (冷启动只遍历一次 FSDB)。
    fn prepare(&self, names: &[String]) {
        // 只把被查询信号的**时间基变更列**取回来。**不建时间线** —— 索引空间是
        // "所有信号变更时间的并集", 对流式后端就是一次全文件扫描, 只有真正需要
        // 索引的查询(find/逐拍/电平)才付。
        //
        // 关键: 一次性把这批信号**合并到一个扫描**里(而不是每个信号各开一次
        // 归并迭代器)。每次迭代器的 add/start 是固定开销(实测单信号 ~100ms),
        // 复合条件 `(&& (rising a) (rising b) …)` 少扫 N-1 次。
        let mut want: HashSet<usize> = HashSet::new();
        for n in names {
            if let Some(i) = self.resolve_idx(n) {
                want.insert(i);
            }
        }
        if want.is_empty() {
            return;
        }
        let missing: HashSet<usize> = want
            .into_iter()
            .filter(|i| !self.cols.borrow().contains_key(i))
            .collect();
        if missing.is_empty() {
            return;
        }
        if let Ok(out) = self.scan(&missing, false) {
            self.apply_scan(out);
        }
    }

    fn step(&mut self, steps: usize) -> Result<(), String> {
        let new_index = self.current_index.saturating_add(steps);
        if new_index > self.highest_valid.get() {
            let max = self.max_index();
            if new_index > max {
                return Err(format!("Step {} would exceed max index {}", steps, max));
            }
            self.highest_valid.set(new_index);
        }
        self.current_index = new_index;
        Ok(())
    }

    fn signal_value(&self, name: &str, offset: usize) -> Result<ScalarValue, String> {
        let idx = self
            .resolve_idx(name)
            .ok_or_else(|| format!("Unknown signal: {}", name))?;
        let max = self.max_index();
        if offset > max {
            return Err(format!(
                "signal_value: offset {} out of range (max {}) for signal '{}'",
                offset, max, name
            ));
        }
        let col = self.column_indexed(idx)?;
        match col.binary_search_by(|(i, _)| i.cmp(&offset)) {
            Ok(k) => Ok(col[k].1.clone()),
            Err(0) => {
                let v = match self.initial_of(idx) {
                    Some(v) => v,
                    None => ScalarValue::Bit(b'x'),
                };
                Ok(self.normalize(idx, v))
            }
            Err(k) => Ok(col[k - 1].1.clone()),
        }
    }

    fn signal_width(&self, name: &str) -> Result<usize, String> {
        let idx = self
            .resolve_idx(name)
            .ok_or_else(|| format!("Unknown signal: {}", name))?;
        Ok(self.width_of(idx))
    }

    fn resolve_name(&self, name: &str) -> Option<String> {
        self.resolve_idx(name).map(|i| self.sig_names[i].clone())
    }

    fn resolve_name_strict(&self, name: &str) -> Result<String, String> {
        if let Some(i) = self.name_to_idx.get(name) {
            return Ok(self.sig_names[*i].clone());
        }
        fn leaf(s: &str) -> &str {
            s.rsplitn(2, '.').next().unwrap_or("")
        }
        let allow_leaf = name.len() <= 8 || !name.contains('.');
        let mut hits: Vec<&String> = Vec::new();
        for s in &self.sig_names {
            if (allow_leaf && leaf(s) == name) || s.contains(name) {
                if !hits.is_empty() {
                    hits.push(s);
                    return Err(format!(
                        "signal '{}' is ambiguous ({} candidates: {:?}) — 请用完整名字",
                        name,
                        hits.len(),
                        &hits[..hits.len().min(5)]
                    ));
                }
                hits.push(s);
            }
        }
        match hits.len() {
            1 => Ok(hits.pop().unwrap().clone()),
            _ => Err(format!("signal '{}' not found in any loaded trace.", name)),
        }
    }

    fn signals(&self) -> Vec<String> {
        self.sig_names.clone()
    }

    /// `$dumpvars` 快照(t0 写入的原值; 含 x/z)。FSDB 写者在 t=0 落一条初值条目,
    /// 语义与 VCD 的 `$dumpvars` 条目一一对应 → **有 t0 条目就返回原值**。
    fn initial_value(&self, name: &str) -> Option<ScalarValue> {
        let idx = self.resolve_idx(name)?;
        self.initial_of(idx)
    }

    /// 只给"确定"初值(过滤 x/z): 用于索引 0 的边沿判定。
    fn defined_initial_value(&self, name: &str) -> Option<ScalarValue> {
        let v = self.initial_value(name)?;
        if sv_is_defined(&v) { Some(v) } else { None }
    }

    fn scopes(&self) -> Vec<String> {
        self.scopes.clone()
    }

    fn max_index(&self) -> usize {
        match self.timeline() {
            Ok(t) => t.len().saturating_sub(1),
            Err(e) => {
                let mut f = self.fatal.borrow_mut();
                if f.is_none() {
                    *f = Some(e);
                }
                0
            }
        }
    }

    fn set_index(&mut self, index: usize) -> Result<(), String> {
        // 见 `highest_valid`: 恢复游标这类"已知有效"的写入不能去问 max_index()
        if index > self.highest_valid.get() {
            let max = self.max_index();
            if index > max {
                return Err(format!("Index {} exceeds max {}", index, max));
            }
            self.highest_valid.set(index);
        }
        self.current_index = index;
        Ok(())
    }

    fn index(&self) -> usize {
        self.current_index
    }

    fn find_indices(&self, name: &str, cond: FindCondition) -> Result<Vec<usize>, String> {
        let idx = self
            .resolve_idx(name)
            .ok_or_else(|| format!("Unknown signal: {}", name))?;
        let tl = self.timeline()?;
        if tl.is_empty() {
            return Ok(Vec::new()); // 空时间线(只有头/没有 dump 段)
        }
        let max = tl.len() - 1;
        let col = self.column_indexed(idx)?;
        Ok(eval_column(max, &col, self.initial_of(idx).as_ref(), &cond))
    }

    fn find_indices_batch(&self, entries: &[BatchEntry]) -> Result<Vec<(String, Vec<usize>)>, String> {
        // 先把所有要碰的信号声明出去 → 一次遍历同时取到时间线 + 全部变更列
        let mut names: Vec<String> = Vec::new();
        for e in entries {
            match e {
                BatchEntry::Simple(n, _) => names.push(n.clone()),
                BatchEntry::And(subs) => names.extend(subs.iter().map(|(n, _)| n.clone())),
            }
        }
        self.prepare(&names);
        let mut results = Vec::new();
        for entry in entries {
            match entry {
                BatchEntry::Simple(name, cond) => {
                    let indices = self.find_indices(name, cond.clone()).unwrap_or_default();
                    results.push((name.clone(), indices));
                }
                BatchEntry::And(subs) => {
                    let mut sets: Vec<Vec<usize>> = Vec::new();
                    for (name, cond) in subs {
                        if let Ok(idxs) = self.find_indices(name, cond.clone()) {
                            sets.push(idxs);
                        }
                    }
                    if sets.is_empty() {
                        results.push((format!("__and_{}", results.len()), vec![]));
                    } else {
                        sets.sort_by_key(|s| s.len());
                        let mut base = sets[0].clone();
                        for other in &sets[1..] {
                            let set: HashSet<usize> = other.iter().copied().collect();
                            base.retain(|i| set.contains(i));
                        }
                        results.push((format!("__and_{}", results.len() - 1), base));
                    }
                }
            }
        }
        Ok(results)
    }

    fn timestamp_at(&self, index: usize) -> Option<u64> {
        self.timeline().ok().and_then(|t| t.get(index).copied())
    }

    fn timescale_exp(&self) -> Option<i8> {
        self.ts_exp
    }

    /// 原生时间基变更点: 直接用时间基列, **不建时间线**(getwave/at/wave 走这里)。
    fn change_points_time(&self, name: &str) -> Result<Vec<(u64, ScalarValue)>, String> {
        let idx = self
            .resolve_idx(name)
            .ok_or_else(|| format!("Unknown signal: {}", name))?;
        Ok(self.column_time(idx)?.points.clone())
    }

    /// 只数个数: 边沿类条件(Rising/Falling/Changed)与全局时间线无关, 可以直接在
    /// 自己的变更列上数 → 省掉一次全文件扫描(索引空间 = 所有信号变更时间的并集)。
    /// 判定用同一个 `find_cond_matches`, 前驱口径与 `eval_column` 相同, 所以结果
    /// 与 `find_indices(..).len()` 逐条一致(差分门/矩阵都在盯这条)。
    fn count_matches(&self, name: &str, cond: FindCondition) -> Result<usize, String> {
        let is_edge = matches!(
            cond,
            FindCondition::Rising | FindCondition::Falling | FindCondition::Changed
        );
        let idx = self
            .resolve_idx(name)
            .ok_or_else(|| format!("Unknown signal: {}", name))?;
        if !is_edge {
            // 电平/取值条件: 仍然需要索引空间(按区间长度累加), 但**不展开索引** ——
            // 全程为 x 的信号上 `(count (is-x s))` 的匹配区间能覆盖上亿个索引,
            // 逐个 push 就是 4GB/394s(内网实测)。这里只在区间上求和。
            let tl = self.timeline()?;
            if tl.is_empty() {
                return Ok(0);
            }
            let col = self.column_indexed(idx)?;
            let (edges, spans) =
                eval_column_spans(tl.len() - 1, &col, self.initial_of(idx).as_ref(), &cond);
            return Ok(edges.len() + spans.iter().map(|(s, e)| e - s).sum::<usize>());
        }
        let col = self.column_time(idx)?;
        let init = self.initial_of(idx);
        let init_defined = init.as_ref().map(sv_is_defined).unwrap_or(false);
        let mut prev = Some(init.unwrap_or(ScalarValue::Bit(b'x')));
        // 复刻 `eval_column` 的"索引 0 没有前驱"特例: 首个变更恰好是全局最早的
        // 那次变更、且没有确定初值 → 这次写入不算 Changed(与 VCD 后端一致)。
        //
        // 只在**可能真的差**时才去问全局最早变更时间(那要额外扫一遍块头):
        //  · Rising/Falling 对 x 前驱恒为假 → 两种口径同答;
        //  · Changed 而首个变更写的是 x/z → x vs x 也不是变化, 同答。
        let need_index0_check = matches!(cond, FindCondition::Changed)
            && !init_defined
            && col.points.first().map(|(_, v)| sv_is_defined(v)).unwrap_or(false);
        if need_index0_check {
            if let Some((t_first, _)) = col.points.first() {
                if self.global_first_change_time()? == Some(*t_first) {
                    prev = None;
                }
            }
        }
        let mut n = 0usize;
        for (_t, v) in &col.points {
            if find_cond_matches(v, prev.as_ref(), &cond) {
                n += 1;
            }
            prev = Some(v.clone());
        }
        Ok(n)
    }

    fn change_points(&self, name: &str) -> Result<Vec<(usize, ScalarValue)>, String> {
        let idx = self
            .resolve_idx(name)
            .ok_or_else(|| format!("Unknown signal: {}", name))?;
        let col = self.column_indexed(idx)?;
        Ok(col.as_ref().clone())
    }

    /// Top-k 变更数: 一次遍历数完所有信号(默认实现会对每个信号各扫一遍)。
    fn signal_change_counts_top(&self, k: usize) -> Vec<(String, usize)> {
        let npi = match npi() {
            Ok(n) => n,
            Err(_) => return Vec::new(),
        };
        // 句柄可能还没解析(名字缓存路径) → 先解析再喂给迭代器
        let mut local: HashMap<usize, usize> = HashMap::with_capacity(self.sigs.len());
        let mut handles: Vec<*mut c_void> = Vec::with_capacity(self.sigs.len());
        for i in 0..self.sigs.len() {
            match self.handle_of(i) {
                Ok(h) => {
                    local.insert(h as usize, i);
                    handles.push(h);
                }
                Err(_) => handles.push(ptr::null_mut()),
            }
        }
        let mut counts = vec![0usize; self.sigs.len()];
        let chunk = chunk_size();
        let mut chunk_start = 0usize;
        while chunk_start < self.sigs.len() {
            let chunk_end = (chunk_start + chunk).min(self.sigs.len());
            let mut it = IterObj::new(npi);
            for i in chunk_start..chunk_end {
                if !handles[i].is_null() {
                    unsafe { (npi.iter.add)(it.this(), handles[i]) };
                }
            }
            unsafe { (npi.iter.start)(it.this(), self.min_t, self.max_t) };
            loop {
                let mut t: NpiTime = 0;
                let mut sig: *mut c_void = ptr::null_mut();
                let rc = unsafe { (npi.iter.next)(it.this(), &mut t, &mut sig) };
                if rc <= 0 || sig.is_null() {
                    break;
                }
                if t == 0 {
                    continue;
                }
                if let Some(&idx) = local.get(&(sig as usize)) {
                    counts[idx] += 1;
                }
            }
            drop(it);
            if unload_each_chunk() {
                if let Some(f) = npi.unload_vc {
                    unsafe { f(self.file) };
                }
            }
            chunk_start = chunk_end;
        }
        let mut v: Vec<(String, usize)> = self
            .sig_names
            .iter()
            .cloned()
            .zip(counts.into_iter())
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.truncate(k);
        v
    }
}
