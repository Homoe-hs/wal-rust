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
use std::path::{Path, PathBuf};
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

/// 值 → 字节(旁挂列缓存用; 4-state 原样保留: Bit 存原始字节, 0/1/x/z 不丢)
fn put_scalar(out: &mut Vec<u8>, v: &ScalarValue) {
    match v {
        ScalarValue::Bit(b) => {
            out.push(0);
            out.push(*b);
        }
        ScalarValue::Vector(bytes) => {
            out.push(1);
            put_varint(out, bytes.len() as u64);
            out.extend_from_slice(bytes);
        }
        ScalarValue::Real(x) => {
            out.push(2);
            out.extend_from_slice(&x.to_le_bytes());
        }
    }
}

fn get_scalar(buf: &[u8], pos: &mut usize) -> Option<ScalarValue> {
    let tag = *buf.get(*pos)?;
    *pos += 1;
    match tag {
        0 => {
            let b = *buf.get(*pos)?;
            *pos += 1;
            Some(ScalarValue::Bit(b))
        }
        1 => {
            let n = get_varint(buf, pos)? as usize;
            if *pos + n > buf.len() {
                return None;
            }
            let v = buf[*pos..*pos + n].to_vec();
            *pos += n;
            Some(ScalarValue::Vector(v))
        }
        2 => {
            if *pos + 8 > buf.len() {
                return None;
            }
            let mut a = [0u8; 8];
            a.copy_from_slice(&buf[*pos..*pos + 8]);
            *pos += 8;
            Some(ScalarValue::Real(f64::from_le_bytes(a)))
        }
        _ => None,
    }
}

/// 旁挂列缓存的编码(纯函数, 便于单元测试往返): `WALFCOL2` + 指纹 + 位宽 +
/// init 编码 + 条数 + [delta 时间 varint, 值]*。
/// init 放在头部, `initial_of()` 读前 64KB 就能拿到(不必解整列)。
fn encode_col_cache(fp: u64, width: usize, init: Option<&ScalarValue>, pts: &[(u64, ScalarValue)]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(32 + pts.len() * 6);
    out.extend_from_slice(b"WALFCOL2");
    out.extend_from_slice(&fp.to_le_bytes());
    out.extend_from_slice(&(width as u32).to_le_bytes());
    match init {
        None => out.push(0),
        Some(ScalarValue::Bit(b)) => {
            out.push(1);
            out.push(*b);
        }
        Some(ScalarValue::Vector(v)) => {
            out.push(2);
            put_varint(&mut out, v.len() as u64);
            out.extend_from_slice(v);
        }
        Some(ScalarValue::Real(x)) => {
            out.push(3);
            out.extend_from_slice(&x.to_le_bytes());
        }
    }
    out.extend_from_slice(&(pts.len() as u32).to_le_bytes());
    let mut prev = 0u64;
    for (t, v) in pts {
        put_varint(&mut out, t.wrapping_sub(prev));
        prev = *t;
        put_scalar(&mut out, v);
    }
    out
}

/// 解析旁挂列缓存头部 → (位宽, t0 初值, 变更点起始偏移)。指纹不符/损坏 → None。
fn parse_col_header(buf: &[u8], fp: u64) -> Option<(usize, Option<ScalarValue>, usize)> {
    if buf.len() < 25 || &buf[0..8] != b"WALFCOL2" {
        return None;
    }
    if u64::from_le_bytes(buf[8..16].try_into().ok()?) != fp {
        return None;
    }
    let width = u32::from_le_bytes(buf[16..20].try_into().ok()?) as usize;
    let mut pos = 20usize;
    let kind = *buf.get(pos)?;
    pos += 1;
    let init: Option<ScalarValue> = match kind {
        0 => None,
        1 => {
            let b = *buf.get(pos)?;
            pos += 1;
            Some(ScalarValue::Bit(b))
        }
        2 => {
            let n = get_varint(buf, &mut pos)? as usize;
            if pos + n > buf.len() {
                return None;
            }
            let v = buf[pos..pos + n].to_vec();
            pos += n;
            Some(ScalarValue::Vector(v))
        }
        3 => {
            if pos + 8 > buf.len() {
                return None;
            }
            let mut a = [0u8; 8];
            a.copy_from_slice(&buf[pos..pos + 8]);
            pos += 8;
            Some(ScalarValue::Real(f64::from_le_bytes(a)))
        }
        _ => return None,
    };
    Some((width, init, pos))
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
    // key 用 `file_identity`(含 ctime/inode): 只按 size+mtime 会漏掉"同秒等长改写",
    // 而那正是波形脚本反复重生成的常态(内网 #8)。
    let id = crate::trace::vcd::file_identity(std::path::Path::new(filename))?;
    Some(cache_root.join(format!("{}-v1{}", id, ext)))
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
    let tmp = tmp_path(&f);
    match std::fs::write(&tmp, &blob) {
        Ok(()) => {
            if let Err(e) = std::fs::rename(&tmp, &f) {
                crate::trace::warn_cache_write(&f, &e);
            }
        }
        Err(e) => crate::trace::warn_cache_write(&tmp, &e),
    }
    if std::env::var("WAL_DEBUG_FSDB").is_ok() {
        eprintln!("[fsdb] 名字树缓存写入 {} 信号 → {}", names.len(), f.display());
    }
}

/// 轮转分片: worker `offset`(0..stride) 负责的信号下标。
/// 纯函数, 有单测盯着"无重叠、无遗漏"(并行构建的底座)。
pub(crate) fn round_robin_slice(n: usize, offset: usize, stride: usize) -> Vec<usize> {
    if stride == 0 {
        return (0..n).collect();
    }
    (offset..n).step_by(stride).collect()
}

/// 时间线"值得落盘缓存"的判据(点数够多, 或这次构建确实慢 —— 由调用方补时长判断)
fn times_len_ok(n: usize) -> bool {
    n >= 256
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
    let push = |v: u64, out: &mut Vec<u64>| {
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

// ======================= 时间线并行构建(fan-out) =======================
//
// 冷启动构建全局时间线要"把整个 FSDB 的每条变更过一遍", 这是内网 174MB 波形
// 上唯一还在几百秒量级的操作。**它是纯并集, 天然可并行** —— 实测同一文件两个
// 进程同时冷建: 串行 20.6s+23.2s, 并行墙钟 23.8s(≈2x 吞吐)。
//
// 并行方式刻意选**多进程**(而不是 fork/线程): NPI 的线程安全性/fork 安全性没有
// 保证, exec 出来的 worker 是全新的进程, 每个都走正常的 npi_init/open 路径。
// 代价是每个 worker 付一次库加载(~1s 量级), 相对几百秒的构建可以忽略。
//
// 开关: `WAL_FSDB_TL_JOBS=N`(默认 1 = 关闭, 保持旧行为)。分片按名字树顺序切成
// N 段连续区间, 每段一个 worker, 各自输出"升序去重的变更时间", 父进程线性归并。

/// 同目录临时文件名带 PID 后缀。
///
/// 并行/集群场景里 **N 个进程可能同时写同一份缓存**(比如 N 个 worker 各自走
/// `FsdbTrace::load` 写 `.fnames`)。若临时文件名固定, 两个进程会写同一个文件,
/// 一个进程 rename 走的可能是另一个进程刚写了一半的内容。带上 pid 后各写各的,
/// `rename` 本身仍是原子的。(读侧本来就校验 magic/指纹/长度, 坏文件只会被当缓存未命中。)
fn tmp_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(format!(".{}.tmp", std::process::id()));
    PathBuf::from(s)
}

/// 该区间内所有变更时间(升序去重) —— worker 的输出格式: delta-varint
pub(crate) fn write_times(path: &std::path::Path, times: &[u64]) -> std::io::Result<()> {
    let mut out = Vec::with_capacity(times.len() * 2 + 8);
    let mut prev = 0u64;
    for &t in times {
        put_varint(&mut out, t - prev);
        prev = t;
    }
    let tmp = tmp_path(path);
    std::fs::write(&tmp, &out)?;
    std::fs::rename(&tmp, path)
}

fn read_times(path: &std::path::Path) -> Option<Vec<u64>> {
    let buf = std::fs::read(path).ok()?;
    let mut pos = 0usize;
    let mut prev = 0u64;
    let mut out = Vec::new();
    while pos < buf.len() {
        let d = get_varint(&buf, &mut pos)?;
        prev = prev.checked_add(d)?;
        out.push(prev);
    }
    Some(out)
}

/// 本进程"可用的并行额度": 优先看批处理系统实际分配了什么, 再看机器核数。
///
/// * LSF: `$LSB_DJOB_NUMPROC`(= `bsub -n N`), 或 `$LSB_MCPU_HOSTS` 里各主机 slot 数之和;
/// * 其它: `available_parallelism()`。
///
/// 为什么要看 LSF 而不是核数: `bsub -n 8` 只给这个 job 8 个 slot, 而节点可能有 64 核 ——
/// 按核数开 worker 会跟同节点的别人抢 CPU, 也远超用户申请的资源。
pub(crate) fn parallel_budget() -> usize {
    if let Ok(v) = std::env::var("LSB_DJOB_NUMPROC") {
        if let Ok(n) = v.trim().parse::<usize>() {
            if n > 0 {
                return n;
            }
        }
    }
    // LSB_MCPU_HOSTS 形如 "hostA 4 hostB 4"(主机名与 slot 数交替)
    if let Ok(v) = std::env::var("LSB_MCPU_HOSTS") {
        let slots: usize = v
            .split_whitespace()
            .skip(1)
            .step_by(2)
            .filter_map(|s| s.parse::<usize>().ok())
            .sum();
        if slots > 0 {
            return slots;
        }
    }
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

/// 分片数策略(纯函数, 便于测试):
/// * 显式给了 `WAL_FSDB_TL_JOBS` → 听它的(`auto` = min(额度, 8), 数字 = 原样);
/// * 没给 + 在批处理里(LSF 有 slot) → min(额度, 8): 用户用 `-n 8` 明确要了 8 个 slot,
///   就该按 8 路并行;默认 1 是为了"登录节点上别偷偷吃 8 个许可", 不是"永远单进程";
/// * 没给 + 不在批处理里 → 1。
pub(crate) fn resolve_timeline_jobs(explicit: Option<&str>, budget: usize, in_batch: bool) -> usize {
    match explicit.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(raw) => {
            let v = raw.to_ascii_lowercase();
            if v == "auto" {
                budget.clamp(1, 8)
            } else {
                v.parse::<usize>().ok().unwrap_or(1)
            }
        }
        None if in_batch => budget.clamp(1, 8),
        None => 1,
    }
}

/// `WAL_FSDB_TL_JOBS` 的解析(向后兼容的入口): 只按核数算额度
pub(crate) fn parse_timeline_jobs(raw: &str, cores: usize) -> usize {
    resolve_timeline_jobs(Some(raw), cores, false)
}

/// 是否在批处理系统里(LSF 会导出这些变量)
pub(crate) fn in_batch_system() -> bool {
    std::env::var_os("LSB_DJOB_NUMPROC").is_some()
        || std::env::var_os("LSB_MCPU_HOSTS").is_some()
        || std::env::var_os("LSB_JOBID").is_some()
}

/// 时间线分片数: `WAL_FSDB_TL_JOBS` 见 `parse_timeline_jobs`。
pub fn timeline_jobs() -> usize {
    let explicit = std::env::var("WAL_FSDB_TL_JOBS").ok();
    resolve_timeline_jobs(
        explicit.as_deref(),
        parallel_budget(),
        in_batch_system(),
    )
}

/// 写 `<cache>/<file_identity>-v1.ftl`(原子: 先写 .tmp 再 rename)。
/// 单进程路径与 `fsdb-timeline-merge` **共用这一份实现** —— 保证两条路径产出的
/// 缓存逐字节一致(已有 A/B 校验: 串行与并行产物相同)。
pub(crate) fn write_timeline_cache(
    cache_root: &Path,
    filename: &str,
    times: &[u64],
    first_change: Option<u64>,
) -> Option<PathBuf> {
    let f = cache_path(cache_root, filename, ".ftl")?;
    if let Some(dir) = f.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return None;
        }
    }
    let fp = crate::trace::vcd::wave_fingerprint(Path::new(filename));
    let mut blob = encode_timeline(times, first_change);
    blob[8..16].copy_from_slice(&fp.to_le_bytes());
    let tmp = tmp_path(&f);
    match std::fs::write(&tmp, &blob) {
        Ok(()) => {
            if let Err(e) = std::fs::rename(&tmp, &f) {
                crate::trace::warn_cache_write(&f, &e);
                return None;
            }
        }
        Err(e) => {
            crate::trace::warn_cache_write(&tmp, &e);
            return None;
        }
    }
    if std::env::var("WAL_DEBUG_FSDB").is_ok() {
        eprintln!("[fsdb] 时间线缓存写入 {} 个时间点 → {}", times.len(), f.display());
    }
    Some(f)
}

/// 把若干"升序去重的时间序列"归并成一个(平衡两两归并, 避免反复拷贝大表)
pub(crate) fn merge_time_sets(mut parts: Vec<Vec<u64>>) -> Vec<u64> {
    if parts.is_empty() {
        return Vec::new();
    }
    while parts.len() > 1 {
        let mut next: Vec<Vec<u64>> = Vec::with_capacity(parts.len().div_ceil(2));
        let mut it = parts.into_iter();
        while let Some(a) = it.next() {
            match it.next() {
                Some(b) => next.push(merge_sorted_unique(&a, &b)),
                None => next.push(a),
            }
        }
        parts = next;
    }
    parts.pop().unwrap_or_default()
}

/// `fsdb-timeline-merge`: 归并分片(每个都是 delta-varint 的升序去重时间序列)
/// 并安装 `.ftl` 缓存 —— 集群(map/reduce)与单机多进程共用同一条收尾路径。
///
/// `cache_root`: None → 用默认缓存目录(`WAL_CACHE_DIR` / CWD 下 `.wal-rust-cache`);
/// 显式传入便于测试与集群里统一指定共享目录。
pub fn merge_timeline_parts(
    file: &Path,
    parts: &[PathBuf],
    cache_root: Option<&Path>,
) -> Result<(usize, PathBuf), String> {
    if parts.is_empty() {
        return Err("没有输入分片(用法: wal-rust fsdb-timeline-merge <file.fsdb> <part>...)".to_string());
    }
    let mut sets: Vec<Vec<u64>> = Vec::with_capacity(parts.len());
    for p in parts {
        let t = read_times(p).ok_or_else(|| format!("分片读不出(截断/格式不对): {}", p.display()))?;
        sets.push(t);
    }
    let times = merge_time_sets(sets);
    if times.is_empty() {
        return Err("归并结果为空: 分片里一个时间点都没有(波形没有 dump 段?)".to_string());
    }
    // 与 `FsdbTrace::load` 同一套文件名规范化(相对路径在 NPI 沙箱里 stat 不到)
    let filename = std::fs::canonicalize(file)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| file.to_string_lossy().to_string());
    let root = cache_root.map(|p| p.to_path_buf()).unwrap_or_else(abs_cache_root);
    let first = times.first().copied();
    let out = write_timeline_cache(&root, &filename, &times, first)
        .ok_or_else(|| format!("写时间线缓存失败: {} (检查目录权限/WAL_CACHE_DIR)", root.display()))?;
    Ok((times.len(), out))
}

/// worker 入口: 只算 [lo, hi) 这些信号的变更时间并落盘。
pub fn run_timeline_worker(
    file: &std::path::Path,
    offset: usize,
    stride: usize,
    out: &std::path::Path,
) -> Result<(), String> {
    let trace = FsdbTrace::load(file, "w".to_string())?;
    let n = trace.sigs.len();
    let stride = stride.max(1);
    // **轮转分片**: 热点信号的代价比冷信号高几个数量级, 连续切片会让一个 worker
    // 独自扛下整条时钟树; `idx % stride == offset` 把冷热混在一起, 且**不需要**
    // 增加 worker 数(每个 worker 一次 NPI 初始化就要 ~2s, 细分成池子反而更慢)。
    let mine: Vec<usize> = round_robin_slice(n, offset, stride);
    if mine.is_empty() {
        return write_times(out, &[]).map_err(|e| e.to_string());
    }
    if std::env::var("WAL_DEBUG_FSDB").is_ok() {
        eprintln!("[fsdb] worker offset={} stride={}: {} 个信号", offset, stride, mine.len());
    }
    let times = trace.scan_times(&mine)?;
    trace.close_now();
    write_times(out, &times).map_err(|e| e.to_string())
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
    /// 短名(诊断/调试用; 查询走 `full`)
    #[allow(dead_code)]
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
    #[allow(dead_code)]
    prepared: RefCell<Vec<usize>>,
    name_cache: RefCell<HashMap<String, Option<usize>>>,
    /// 叶子名(短名)索引: 按叶子名排序的 `sig_names` 下标, 懒建一次。
    ///
    /// 为什么需要: 短名(`clk`)不是 `name_to_idx` 的键, 旧实现在这里**线性扫 188 万个名字**
    /// (首个不同拼写各付一次)。188 万信号的 FSDB 上, 一次解析要几十~几百毫秒;
    /// 而查询里每次 `get` 解析一次 → 直接卡死。排序索引只存 u32(约 7.5MB @188万),
    /// 查找 O(log N), 且不复制名字字符串。
    leaf_order: std::cell::OnceCell<Vec<u32>>,
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
    /// 扫描重入保护(当前扫描路径已串行化; 保留字段以便将来做并发扫描)
    #[allow(dead_code)]
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
        // 用户可能传**相对路径**, 而 NPI 沙箱会把 CWD 切到缓存目录下 npi/ 里:
        // ① NPI open 必须给绝对路径; ② 之后每次算缓存 key 都要 stat 这个文件 ——
        // 相对路径在沙箱里 stat 不到 → cache_path() 返回 None → **缓存永远写不出来**,
        // 每次都重付一遍全文件扫描(实测 VM 上相对路径一个缓存文件都不落)。
        // 所以这里统一记成绝对路径(解析失败才退回原样)。
        let filename = std::fs::canonicalize(path)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string_lossy().to_string());
        // 缓存根必须在进 NPI 沙箱**之前**算成绝对路径(沙箱里 CWD 已经变了)
        let cache_root = abs_cache_root();
        let abs = std::path::PathBuf::from(&filename);
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
            #[allow(unused_assignments)]
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
                leaf_order: std::cell::OnceCell::new(),
                first_change: Cell::new(None),
                current_index: 0,
                highest_valid: Cell::new(0),
                fatal: RefCell::new(None),
                scanning: Cell::new(false),
            })
        }
    }

    /// 名字解析: 精确 → 叶子名(短名/无点) → 子串(与 VCD/FST 同口径), 结果缓存。
    /// 叶子名(最后一个 `.` 之后)在 `sig_names` 里的下标, 按 (叶子名, 下标) 排序。
    /// 相同叶子名时取**下标最小**的, 与旧的线性扫描语义一致。
    fn leaf_order_index(&self) -> &Vec<u32> {
        self.leaf_order.get_or_init(|| {
            fn leaf(s: &str) -> &str {
                s.rsplitn(2, '.').next().unwrap_or("")
            }
            let mut v: Vec<u32> = (0..self.sig_names.len() as u32).collect();
            v.sort_unstable_by(|a, b| {
                let (sa, sb) = (&self.sig_names[*a as usize], &self.sig_names[*b as usize]);
                leaf(sa).cmp(leaf(sb)).then(a.cmp(b))
            });
            v
        })
    }

    /// 按叶子名查(O(log N));重复叶子名返回下标最小的那个。
    fn lookup_leaf(&self, want: &str) -> Option<usize> {
        fn leaf(s: &str) -> &str {
            s.rsplitn(2, '.').next().unwrap_or("")
        }
        let order = self.leaf_order_index();
        let lo = order.partition_point(|&i| leaf(&self.sig_names[i as usize]) < want);
        let first = *order.get(lo)?;
        if leaf(&self.sig_names[first as usize]) == want {
            Some(first as usize)
        } else {
            None
        }
    }

    /// 叶子名是否**唯一**(用于严格解析的歧义判定)。
    fn leaf_is_unique(&self, want: &str) -> Option<bool> {
        fn leaf(s: &str) -> &str {
            s.rsplitn(2, '.').next().unwrap_or("")
        }
        let order = self.leaf_order_index();
        let lo = order.partition_point(|&i| leaf(&self.sig_names[i as usize]) < want);
        let first = *order.get(lo)?;
        if leaf(&self.sig_names[first as usize]) != want {
            return None;
        }
        let hi = lo + order[lo..].iter().take_while(|&&i| leaf(&self.sig_names[i as usize]) == want).count();
        Some(hi - lo == 1)
    }

    fn resolve_idx(&self, name: &str) -> Option<usize> {
        if let Some(i) = self.name_to_idx.get(name) {
            return Some(*i);
        }
        if let Some(c) = self.name_cache.borrow().get(name) {
            return *c;
        }
        let allow_leaf = name.len() <= 8 || !name.contains('.');
        // 短名走懒建的排序索引(O(log N)), 不再线性扫全表
        let hit = if allow_leaf { self.lookup_leaf(name) } else { None };
        if hit.is_none() {
            // 子串匹配仍然只能线性扫(这类写法本身代价高);结果同样进 name_cache。
            for (i, s) in self.sig_names.iter().enumerate() {
                if s.contains(name) {
                    let found = Some(i);
                    self.name_cache.borrow_mut().insert(name.to_string(), found);
                    return found;
                }
            }
        }
        let found = hit;
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

    /// 多进程并行构建全局时间线: 把信号按名字树顺序切成 N 段, 每段一个 worker
    /// 子进程(exec 自身, 走正常的 npi_init/open 路径, 不 fork), 再线性归并。
    /// 只在 `WAL_FSDB_TL_JOBS>1` 时调用; 任何一步失败都返回 Err 让调用方回退。
    ///
    /// 注意: 每个 worker 都是一次独立的 NPI 会话 —— 会各占**一个 Verdi 许可**,
    /// 所以默认关闭。
    fn parallel_timeline(&self, jobs: usize) -> Result<Vec<u64>, String> {
        let exe = std::env::current_exe().map_err(|e| format!("current_exe: {}", e))?;
        let jobs = jobs.clamp(1, self.sigs.len());
        // 只开 `jobs` 个 worker(每个都是一次 NPI 会话, 初始化 ~2s + 一个 Verdi 许可),
        // 负载均衡靠 worker 内部的**轮转分片**而不是加进程。
        let tag = std::process::id();
        let mut running: Vec<(std::process::Child, std::path::PathBuf)> = Vec::new();
        for k in 0..jobs {
            let out = self.cache_root.join(format!(".tl-{}-{}.part", tag, k));
            let _ = std::fs::remove_file(&out);
            match std::process::Command::new(&exe)
                .arg("fsdb-timeline-map")
                .arg(&self.filename)
                .arg(k.to_string())
                .arg(jobs.to_string())
                .arg(&out)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(c) => running.push((c, out)),
                Err(e) => {
                    for (mut c, o) in running {
                        let _ = c.kill();
                        let _ = c.wait();
                        let _ = std::fs::remove_file(&o);
                    }
                    return Err(format!("spawn worker: {}", e));
                }
            }
        }
        let mut parts: Vec<Vec<u64>> = Vec::new();
        let mut failed: Option<String> = None;
        for (mut c, out) in running {
            match c.wait() {
                Ok(st) if st.success() => match read_times(&out) {
                    Some(t) => parts.push(t),
                    None => failed = Some(format!("worker 输出无法读取: {}", out.display())),
                },
                Ok(st) => failed = Some(format!("worker 退出码 {:?}", st.code())),
                Err(e) => failed = Some(format!("wait worker: {}", e)),
            }
            let _ = std::fs::remove_file(&out);
            if failed.is_some() {
                break;
            }
        }
        if let Some(e) = failed {
            return Err(e);
        }
        let mut all: Vec<u64> = merge_time_sets(parts);
        all.sort_unstable();
        all.dedup();
        Ok(all)
    }

    /// 只算"这些信号的变更时间并集"(升序去重) —— 并行构建时间线的 worker 用。
    fn scan_times(&self, targets: &[usize]) -> Result<Vec<u64>, String> {
        let npi = npi()?;
        let chunk = chunk_size();
        let mut all: Vec<u64> = Vec::new();
        let mut lo = 0usize;
        while lo < targets.len() {
            let hi = (lo + chunk).min(targets.len());
            let mut it = IterObj::new(npi);
            for &i in &targets[lo..hi] {
                if let Ok(h) = self.handle_of(i) {
                    unsafe { (npi.iter.add)(it.this(), h) };
                }
            }
            unsafe { (npi.iter.start)(it.this(), self.min_t, self.max_t) };
            let mut chunk_times: Vec<u64> = Vec::new();
            let mut last = u64::MAX;
            loop {
                let mut t: NpiTime = 0;
                let mut sig: *mut c_void = ptr::null_mut();
                let rc = unsafe { (npi.iter.next)(it.this(), &mut t, &mut sig) };
                if rc <= 0 || sig.is_null() {
                    break;
                }
                if t > 0 && t != last {
                    chunk_times.push(t);
                    last = t;
                }
            }
            drop(it);
            if unload_each_chunk() {
                if let Some(f) = npi.unload_vc {
                    unsafe { f(self.file) };
                }
            }
            all = merge_sorted_unique(&all, &chunk_times);
            lo = hi;
        }
        Ok(all)
    }

    /// 立刻关闭 FSDB(worker 写完就走, 不等 Drop)
    pub fn close_now(&self) {
        if let Ok(npi) = npi() {
            unsafe { (npi.close)(self.file) };
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
        // 每块的"块内已去重升序时间表"。收齐后一次性归并(见函数末尾), 不边扫边并。
        let mut tl_parts: Vec<Vec<u64>> = Vec::new();
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
                tl_parts.push(chunk_times);
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
                // 块内已升序去重 → 收起来最后统一归并(不逐块并进主表)
                tl_parts.push(chunk_times);
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
            // 收齐所有分片再一次归并。**不能边扫边并进主表**: 每块都会把已累积的
            // 主表整份拷贝一遍, 总拷贝量 = O(块数 × 主表长) —— 1.88M 信号(4096/块
            // = 459 块)× 上千万时间点 = 几十 GB memcpy, 纯浪费。
            let mut times = merge_time_sets(tl_parts);
            times.sort_unstable();
            times.dedup();
            out.times = times;
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
        write_timeline_cache(&self.cache_root, &self.filename, times, first_change);
    }

    // ============ 逐信号变更列的旁挂缓存(跨进程) ============
    //
    // 为什么需要: FSDB 每个进程都要重走一遍 NPI 变更流才能拿到某信号的列
    // (`scan(&{idx}, false)` → `npiFsdbTimeBasedVcIter`), 而 `(get s)`/
    // `(getwave s)`/`(at s T)`/`(count (rising s))` **只要这一个信号自己的变更列**。
    // 实测(2M 时间戳夹具, TCG 客机): 只加载 3.3s, 而"取一个 2M 变更信号的列"要
    // +4.9s —— 每次查询都重扫一遍纯属浪费。VCD 侧早有同构的旁挂列缓存(`.cols`),
    // FSDB 一直没有。
    //
    // 格式: `WALFCOL1` + 指纹(u64) + 位宽(u32) + 条数(u32) + [delta 时间, 值]*
    // 路径: `<cache>/<文件身份>-v1.fcol/<fnv1a(全名)>-v1.col`(与 `.ftl`/`.fnames` 同一身份 key)
    fn col_cache_dir(&self) -> Option<PathBuf> {
        cache_path(&self.cache_root, &self.filename, ".fcol")
    }

    fn col_cache_file(&self, idx: usize) -> Option<PathBuf> {
        let sig = self.sigs.get(idx)?;
        let dir = self.col_cache_dir()?;
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in sig.full.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        Some(dir.join(format!("{:016x}-v1.col", h)))
    }

    /// 单列点数上限: 几千万点的信号一个文件上百 MB, 不值当(`WAL_FSDB_COL_MAX` 可调)
    fn col_cache_max_points() -> usize {
        std::env::var("WAL_FSDB_COL_MAX")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(50_000_000)
    }

    /// 解析旁挂列缓存头部(指纹由本 trace 的波形算出)
    fn parse_col_header(&self, buf: &[u8]) -> Option<(usize, Option<ScalarValue>, usize)> {
        let fp = crate::trace::vcd::wave_fingerprint(Path::new(&self.filename));
        parse_col_header(buf, fp)
    }

    fn try_load_col_cache(&self, idx: usize) -> Option<(Option<ScalarValue>, Vec<(u64, ScalarValue)>)> {
        if crate::trace::vcd::cache_mode() == crate::trace::vcd::CacheMode::Off {
            return None;
        }
        let f = self.col_cache_file(idx)?;
        let buf = std::fs::read(&f).ok()?;
        let (width, init, mut pos) = self.parse_col_header(&buf)?;
        if width != self.sigs[idx].width {
            return None;
        }
        let n = u32::from_le_bytes(buf.get(pos..pos + 4)?.try_into().ok()?) as usize;
        pos += 4;
        let mut out = Vec::with_capacity(n);
        let mut t = 0u64;
        for _ in 0..n {
            t = t.wrapping_add(get_varint(&buf, &mut pos)?);
            out.push((t, get_scalar(&buf, &mut pos)?));
        }
        if std::env::var("WAL_DEBUG_FSDB").is_ok() {
            eprintln!("[fsdb] 列缓存命中: {} 点(初值 {:?}) ← {}", out.len(), init, f.display());
        }
        Some((init, out))
    }

    /// 只要 t0 初值: 读头部即可, 不解整列(宽信号超出缓冲区时退回整文件读)
    fn try_load_init_cache(&self, idx: usize) -> Option<Option<ScalarValue>> {
        if crate::trace::vcd::cache_mode() == crate::trace::vcd::CacheMode::Off {
            return None;
        }
        let f = self.col_cache_file(idx)?;
        use std::io::Read;
        let mut file = std::fs::File::open(&f).ok()?;
        let mut head = vec![0u8; 65536];
        let n = file.read(&mut head).ok()?;
        head.truncate(n);
        let parsed = self.parse_col_header(&head).or_else(|| {
            let all = std::fs::read(&f).ok()?;
            self.parse_col_header(&all)
        })?;
        if parsed.0 != self.sigs[idx].width {
            return None;
        }
        Some(parsed.1)
    }

    fn save_col_cache(&self, idx: usize, pts: &[(u64, ScalarValue)]) {
        use crate::trace::vcd::CacheMode;
        if !matches!(crate::trace::vcd::cache_mode(), CacheMode::Auto | CacheMode::Build) {
            return;
        }
        if pts.len() < 2 || pts.len() > Self::col_cache_max_points() {
            return;
        }
        let Ok(meta) = std::fs::metadata(&self.filename) else { return };
        if meta.len() < crate::trace::vcd::cache_min_bytes() {
            return;
        }
        let Some(f) = self.col_cache_file(idx) else { return };
        if let Some(dir) = f.parent() {
            if std::fs::create_dir_all(dir).is_err() {
                return;
            }
        }
        let init = self.initials.borrow().get(&idx).cloned().flatten();
        let fp = crate::trace::vcd::wave_fingerprint(Path::new(&self.filename));
        let out = encode_col_cache(fp, self.sigs[idx].width, init.as_ref(), pts);
        let tmp = tmp_path(&f);
        match std::fs::write(&tmp, &out) {
            Ok(()) => {
                if let Err(e) = std::fs::rename(&tmp, &f) {
                    crate::trace::warn_cache_write(&f, &e);
                }
            }
            Err(e) => crate::trace::warn_cache_write(&tmp, &e),
        }
        if std::env::var("WAL_DEBUG_FSDB").is_ok() {
            eprintln!("[fsdb] 列缓存写入 {} 点 → {}", pts.len(), f.display());
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
        // 并行构建: 冷启动的时间线是"把全文件每条变更过一遍", 与信号数无关、只与
        // 变更条数有关 —— 所以判据是"每个 worker 至少分到一个信号", 不是"信号数够多"。
        // 曾经写成 `sigs >= 2048`, 于是"信号少但时间戳几千万"的波形(一组计数器打满
        // 时间轴)永远走单进程, 明明可以并行。(轮转分片对任何 n 都成立。)
        let jobs = timeline_jobs();
        if jobs > 1 && self.sigs.len() >= jobs {
            // 没显式设 WAL_FSDB_TL_JOBS 而开了并行 → 只可能是"批处理分配了 slot",
            // 这件事必须说出来(每个 worker 各占一个 NPI 许可)。
            let explicit = std::env::var("WAL_FSDB_TL_JOBS").map(|v| !v.trim().is_empty()).unwrap_or(false);
            if !explicit && std::env::var("WAL_DEBUG_FSDB").is_err() {
                eprintln!(
                    "[fsdb] LSF 分配了 {} 个 slot → 时间线冷建用 {} 个 worker(每个各占一个 NPI 许可); 想单进程设 WAL_FSDB_TL_JOBS=1",
                    parallel_budget(),
                    jobs
                );
            }
            match self.parallel_timeline(jobs) {
                Ok(times) if !times.is_empty() => {
                    if std::env::var("WAL_DEBUG_FSDB").is_ok() {
                        eprintln!(
                            "[fsdb] 并行构建时间线: {} workers, {} 个时间点, {}ms",
                            jobs,
                            times.len(),
                            t_begin.elapsed().as_millis()
                        );
                    }
                    self.first_change.set(Some(times.first().copied()));
                    let rc = Rc::new(times);
                    *self.timeline.borrow_mut() = Some(rc.clone());
                    if times_len_ok(rc.len()) {
                        self.save_timeline_cache(&rc, self.first_change.get().flatten());
                    }
                    return Ok(rc);
                }
                Ok(_) => {}
                Err(e) => {
                    if std::env::var("WAL_DEBUG_FSDB").is_ok() {
                        eprintln!("[fsdb] 并行构建失败, 回退单进程: {}", e);
                    }
                }
            }
        }
        let out = self.scan(&HashSet::new(), true)?;
        let times = out.times.clone();
        self.apply_scan(out);
        let t = self.timeline.borrow().clone();
        let t = t.ok_or_else(|| "FSDB 时间线构建失败".to_string())?;
        // 顺手把"全局最早变更时间"填上(时间线的第一个点) —— 并行/串行两条路径
        // 产出的缓存因此逐字节一致, 也省掉后续 `global_first_change_time()` 的探测。
        if self.first_change.get().is_none() {
            self.first_change.set(Some(t.first().copied()));
        }
        let elapsed = t_begin.elapsed();
        // 冷建慢的时候给一条可执行的提示(时间线只能靠"把全文件变更过一遍", 能压的
        // 只有并行 —— 单机多进程或 LSF)。只在真的慢(≥30s)且没开并行时说一次。
        if jobs <= 1 && elapsed >= std::time::Duration::from_secs(30) {
            eprintln!(
                "[fsdb] 时间线冷建 {:.1}s({} 个时间点): 可并行加速 —— \
                 WAL_FSDB_TL_JOBS=auto(每 worker 各占一个 NPI 许可), \
                 集群上用 `wal-rust fsdb-timeline-map`/`fsdb-timeline-merge`(见 docs/fsdb-npi.md §6.6)",
                elapsed.as_secs_f64(),
                times.len()
            );
        }
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
        // 跨进程旁挂缓存: 命中就完全不用碰 NPI 的变更流(连 t0 初值一起拿回来)
        if let Some((init, pts)) = self.try_load_col_cache(idx) {
            self.initials.borrow_mut().entry(idx).or_insert(init);
            let rc = Rc::new(Column { points: pts });
            self.cols.borrow_mut().insert(idx, rc.clone());
            return Ok(rc);
        }
        let mut want = HashSet::new();
        want.insert(idx);
        let out = self.scan(&want, false)?;
        self.apply_scan(out);
        let got = self.cols.borrow().get(&idx).cloned();
        if let Some(c) = &got {
            self.save_col_cache(idx, &c.points);
        }
        Ok(got.unwrap_or_else(|| Rc::new(Column::default())))
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

    /// 信号初值(t0 快照; 没有条目 → None)
    fn initial_of(&self, idx: usize) -> Option<ScalarValue> {
        if let Some(v) = self.initials.borrow().get(&idx) {
            return v.clone();
        }
        // 旁挂缓存头部就带着初值 → 不必为一个初值扫完整条变更流
        if let Some(got) = self.try_load_init_cache(idx) {
            self.initials.borrow_mut().insert(idx, got.clone());
            return got;
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
        let _name = Npi::cstr((npi.scope_property_str)(SCOPE_NAME, scope));
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
        // 先吃跨进程旁挂缓存(命中就不必再走 NPI); 剩下的才合并成一次扫描。
        let mut missing: HashSet<usize> = HashSet::new();
        for i in want {
            if self.cols.borrow().contains_key(&i) {
                continue;
            }
            match self.try_load_col_cache(i) {
                Some((init, pts)) => {
                    self.initials.borrow_mut().entry(i).or_insert(init);
                    self.cols.borrow_mut().insert(i, Rc::new(Column { points: pts }));
                }
                None => {
                    missing.insert(i);
                }
            }
        }
        if missing.is_empty() {
            return;
        }
        if let Ok(out) = self.scan(&missing, false) {
            self.apply_scan(out);
        }
        // 扫完落盘: 下一个进程(同一条查询)直接命中 —— FSDB 拿列只能靠 NPI 全扫该信号
        let scanned: Vec<usize> = missing.into_iter().collect();
        for i in scanned {
            if let Some(c) = self.cols.borrow().get(&i).cloned() {
                self.save_col_cache(i, &c.points);
            }
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
        // 叶子名唯一 → 直接命中(免去 188 万次线性扫描);有重名则按原语义报歧义错误。
        if allow_leaf {
            match self.leaf_is_unique(name) {
                Some(true) => {
                    let i = self.lookup_leaf(name).ok_or_else(|| {
                        format!("signal '{}' not found in any loaded trace.", name)
                    })?;
                    return Ok(self.sig_names[i].clone());
                }
                Some(false) => {
                    return Err(format!(
                        "signal '{}' is ambiguous (多个同名叶子信号) — 请用完整名字",
                        name
                    ));
                }
                None => {}
            }
        }
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

#[cfg(test)]
mod col_cache_tests {
    use super::*;

    /// 从编码后的字节流解出变更点(与 `try_load_col_cache` 同一读法)
    fn decode_points(buf: &[u8], mut pos: usize) -> Option<Vec<(u64, ScalarValue)>> {
        let n = u32::from_le_bytes(buf.get(pos..pos + 4)?.try_into().ok()?) as usize;
        pos += 4;
        let mut out = Vec::with_capacity(n);
        let mut t = 0u64;
        for _ in 0..n {
            t = t.wrapping_add(get_varint(buf, &mut pos)?);
            out.push((t, get_scalar(buf, &mut pos)?));
        }
        Some(out)
    }

    /// 4-state 必须原样往返: 0/1/x/z 与矢量里的 x/z 都不能被"规范化"掉
    #[test]
    fn col_cache_round_trip_keeps_4state() {
        let fp = 0x1234_5678_9abc_def0u64;
        let init = ScalarValue::Bit(b'x');
        let pts = vec![
            (5u64, ScalarValue::Bit(b'0')),
            (9u64, ScalarValue::Bit(b'z')),
            (12u64, ScalarValue::Bit(b'1')),
            (40u64, ScalarValue::Vector(vec![b'1', b'0', b'x', b'z'])),
        ];
        let buf = encode_col_cache(fp, 4, Some(&init), &pts);
        let (width, got_init, off) = parse_col_header(&buf, fp).expect("header");
        assert_eq!(width, 4);
        assert_eq!(got_init, Some(ScalarValue::Bit(b'x')));
        assert_eq!(decode_points(&buf, off).unwrap(), pts);
    }

    /// "没有 t0 条目"(None)与"初值是某个值"必须能区分 —— 引擎的索引 0 特例靠它
    #[test]
    fn col_cache_distinguishes_absent_init() {
        let fp = 7u64;
        let buf = encode_col_cache(fp, 1, None, &[(3u64, ScalarValue::Bit(b'1'))]);
        let (_, init, _) = parse_col_header(&buf, fp).expect("header");
        assert_eq!(init, None);
    }

    #[test]
    fn col_cache_real_value_round_trip() {
        let fp = 42u64;
        let pts = vec![(11u64, ScalarValue::Real(1.5)), (19u64, ScalarValue::Real(-0.25))];
        let buf = encode_col_cache(fp, 64, Some(&ScalarValue::Real(3.0)), &pts);
        let (_, init, off) = parse_col_header(&buf, fp).expect("header");
        assert_eq!(init, Some(ScalarValue::Real(3.0)));
        assert_eq!(decode_points(&buf, off).unwrap(), pts);
    }

    /// 指纹不符(同一秒等长改写)必须视为未命中 —— 否则会拿旧列答"看似正常"的错值
    #[test]
    fn col_cache_rejects_stale_fingerprint() {
        let buf = encode_col_cache(1u64, 1, None, &[(3u64, ScalarValue::Bit(b'1'))]);
        assert!(parse_col_header(&buf, 2u64).is_none());
    }

    /// 损坏/截断的缓存不能 panic, 只能是未命中
    #[test]
    fn col_cache_rejects_corrupt() {
        let buf = encode_col_cache(1u64, 1, None, &[(3u64, ScalarValue::Bit(b'1'))]);
        assert!(parse_col_header(&buf[..10], 1u64).is_none());
        let mut bad = buf.clone();
        bad[20] = 9; // 未知 init 编码
        assert!(parse_col_header(&bad, 1u64).is_none());
        assert!(decode_points(&buf, buf.len() - 1).is_none());
    }
}
