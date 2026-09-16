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
    sig_property: unsafe extern "C" fn(c_int, *mut c_void, *mut c_int) -> c_int,
    sig_property_str: unsafe extern "C" fn(c_int, *mut c_void) -> *const c_char,
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
            for p in &candidates {
                let c = CString::new(p.as_str()).map_err(|_| format!("库路径含 NUL: {}", p))?;
                handle = libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
                if !handle.is_null() {
                    hit_path = Some(p.clone());
                    break;
                }
                derr = CStr::from_ptr(libc::dlerror()).to_string_lossy().to_string();
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
                sig_property: sym!("_Z21npi_fsdb_sig_property22npiFsdbSigPropertyTypePvPi", unsafe extern "C" fn(c_int, *mut c_void, *mut c_int) -> c_int),
                sig_property_str: sym!("_Z25npi_fsdb_sig_property_str22npiFsdbSigPropertyTypePv", unsafe extern "C" fn(c_int, *mut c_void) -> *const c_char),
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
            let mut owned: Vec<CString> = std::env::args()
                .map(|a| CString::new(a).unwrap_or_else(|_| CString::new("wal-rust").unwrap()))
                .collect();
            let mut argv: Vec<*mut c_char> =
                owned.iter_mut().map(|c| c.as_ptr() as *mut c_char).collect();
            argv.push(ptr::null_mut());
            let mut argc = (argv.len() - 1) as c_int;
            let mut argv_p = argv.as_mut_ptr();
            let _box = NpiSandbox::enter();
            let rc = init(&mut argc, &mut argv_p);
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
/// ① **静音 stdout** —— NPI 的版权 banner / 加载警告走 stdout, 会污染 CLI 输出
///    (管道/脚本里直接坏掉)。`WAL_DEBUG_FSDB=1` 时不静音, 方便看 NPI 自己的报错。
/// ② **CWD 切到 `./.wal-rust-cache/npi/`** —— NPI 会在 CWD 建 `<argv0>Log/` 日志
///    目录(`wal-rustLog/`), 不该落在用户目录里。切不进去(只读目录)就放弃隔离。
struct NpiSandbox {
    saved_stdout: c_int,
    saved_cwd: Option<std::path::PathBuf>,
    quiet: bool,
}

impl NpiSandbox {
    fn enter() -> Self {
        let quiet = std::env::var("WAL_DEBUG_FSDB").is_err();
        let mut saved_stdout = -1;
        if quiet {
            unsafe {
                let devnull = libc::open(b"/dev/null\0".as_ptr() as *const c_char, libc::O_WRONLY);
                if devnull >= 0 {
                    saved_stdout = libc::dup(1);
                    if saved_stdout >= 0 {
                        libc::dup2(devnull, 1);
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
        NpiSandbox { saved_stdout, saved_cwd, quiet }
    }
}

impl Drop for NpiSandbox {
    fn drop(&mut self) {
        if let Some(cwd) = self.saved_cwd.take() {
            let _ = std::env::set_current_dir(cwd);
        }
        if self.quiet && self.saved_stdout >= 0 {
            unsafe {
                libc::dup2(self.saved_stdout, 1);
                libc::close(self.saved_stdout);
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
    let mut indices = Vec::new();
    let first_is_zero = points.first().map(|(i, _)| *i) == Some(0);
    let init_x = ScalarValue::Bit(b'x');
    let init_ref = initial.unwrap_or(&init_x);
    let mut prev_val: Option<ScalarValue> = if first_is_zero {
        if sv_is_defined(init_ref) { Some(init_ref.clone()) } else { None }
    } else {
        Some(init_ref.clone())
    };
    if !is_edge && find_cond_matches(init_ref, None, cond) {
        let first_idx = points.first().map(|(i, _)| *i).unwrap_or(max_index + 1);
        indices.extend(0..first_idx.min(max_index + 1));
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
            indices.push(*idx);
        } else {
            let end = points.get(k + 1).map(|(n, _)| *n).unwrap_or(max_index + 1);
            indices.extend(*idx..end.min(max_index + 1));
        }
    }
    indices
}

// ============================ Trace 实现 ============================

struct Sig {
    handle: *mut c_void,
    name: String,
    full: String,
    width: usize,
    is_real: bool,
    is_string: bool,
}

/// 每信号变更列(按索引升序; 不含 t0 初值快照)。
#[derive(Default)]
struct Column {
    points: Vec<(usize, ScalarValue)>,
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
    file: *mut c_void,
    sigs: Vec<Sig>,
    sig_names: Vec<String>,
    name_to_idx: HashMap<String, usize>,
    handle_to_idx: HashMap<usize, usize>,
    scopes: Vec<String>,
    ts_exp: Option<i8>,
    min_t: u64,
    max_t: u64,
    /// 首次按索引查询时构建(OnceCell 语义: 用 RefCell<Option<Rc<…>>>)
    timeline: RefCell<Option<Rc<Vec<u64>>>>,
    cols: RefCell<HashMap<usize, Rc<Column>>>,
    initials: RefCell<HashMap<usize, Option<ScalarValue>>>,
    /// `Trace::prepare` 声明的信号: 建时间线时顺带取它们的变更列
    prepared: RefCell<Vec<usize>>,
    name_cache: RefCell<HashMap<String, Option<usize>>>,
    current_index: usize,
    fatal: RefCell<Option<String>>,
    /// 扫描进行中: 防止 `column()` 递归触发第二次扫描
    scanning: Cell<bool>,
}

/// 分块大小: 一次往归并迭代器里塞太多信号会占大量内存(实测 90k 级信号要分块)。
const CHUNK: usize = 4096;

impl FsdbTrace {
    pub fn load(path: &Path, id: TraceId) -> Result<Self, String> {
        let npi = npi()?;
        let filename = path.to_string_lossy().to_string();
        // 沙箱会把 CWD 切走, 所以必须给 NPI 绝对路径(用户可能传相对路径)
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let cpath = CString::new(abs.to_string_lossy().as_bytes())
            .map_err(|_| format!("路径含 NUL 字节: {}", filename))?;
        let _box = NpiSandbox::enter();
        unsafe {
            if let Some(f) = npi.is_fsdb {
                if f(cpath.as_ptr()) == 0 {
                    return Err(format!("{}: 不是 FSDB 文件(NPI 判定)", filename));
                }
            }
            let file = (npi.open)(cpath.as_ptr());
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
            if std::env::var("WAL_DEBUG_FSDB").is_ok() {
                eprintln!(
                    "[fsdb] {} version={} scale={} ({:?}) min={} max={}",
                    filename, version, scale, ts_exp, min_t, max_t
                );
            }

            let mut sigs: Vec<Sig> = Vec::new();
            let mut scopes: Vec<String> = Vec::new();
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
            if sigs.is_empty() {
                (npi.close)(file);
                return Err(format!("{}: FSDB 里没有任何信号", filename));
            }
            let mut name_to_idx = HashMap::with_capacity(sigs.len());
            let mut handle_to_idx = HashMap::with_capacity(sigs.len());
            let mut sig_names = Vec::with_capacity(sigs.len());
            for (i, s) in sigs.iter().enumerate() {
                name_to_idx.entry(s.full.clone()).or_insert(i);
                handle_to_idx.insert(s.handle as usize, i);
                sig_names.push(s.full.clone());
            }
            Ok(FsdbTrace {
                id,
                filename,
                file,
                sigs,
                sig_names,
                name_to_idx,
                handle_to_idx,
                scopes,
                ts_exp,
                min_t,
                max_t,
                timeline: RefCell::new(None),
                cols: RefCell::new(HashMap::new()),
                initials: RefCell::new(HashMap::new()),
                prepared: RefCell::new(Vec::new()),
                name_cache: RefCell::new(HashMap::new()),
                current_index: 0,
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
    fn read_value(&self, npi: &Npi, it: &mut IterObj, sig: &Sig) -> ScalarValue {
        let wanted = if sig.is_real {
            VAL_REAL
        } else if sig.is_string {
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
        let mut chunk_start = 0usize;
        while chunk_start < targets.len() {
            let chunk_end = (chunk_start + CHUNK).min(targets.len());
            let mut it = IterObj::new(npi);
            for &i in &targets[chunk_start..chunk_end] {
                unsafe { (npi.iter.add)(it.this(), self.sigs[i].handle) };
            }
            unsafe { (npi.iter.start)(it.this(), self.min_t, self.max_t) };
            loop {
                let mut t: NpiTime = 0;
                let mut sig: *mut c_void = ptr::null_mut();
                let rc = unsafe { (npi.iter.next)(it.this(), &mut t, &mut sig) };
                if rc <= 0 || sig.is_null() {
                    break;
                }
                let Some(&idx) = self.handle_to_idx.get(&(sig as usize)) else {
                    continue;
                };
                if t == 0 {
                    if want.contains(&idx) {
                        let v = self.read_value(npi, &mut it, &self.sigs[idx]);
                        out.inits.entry(idx).or_insert(v);
                    }
                    continue;
                }
                if want_timeline {
                    out.times.push(t);
                }
                if want.contains(&idx) {
                    let v = self.read_value(npi, &mut it, &self.sigs[idx]);
                    out.raw.entry(idx).or_default().push((t, v));
                }
            }
            drop(it);
            chunk_start = chunk_end;
        }
        if want_timeline {
            out.times.sort_unstable();
            out.times.dedup();
            out.has_timeline = true;
        }
        Ok(out)
    }

    /// 时间线(索引 → 原生时间)。规则 A: 不含 t=0 初值快照。
    fn timeline(&self) -> Result<Rc<Vec<u64>>, String> {
        if let Some(t) = self.timeline.borrow().as_ref() {
            return Ok(t.clone());
        }
        let prepared: HashSet<usize> = self.prepared.borrow().iter().copied().collect();
        let out = self.scan(&prepared, true)?;
        self.apply_scan(out);
        let t = self.timeline.borrow().clone();
        t.ok_or_else(|| "FSDB 时间线构建失败".to_string())
    }

    /// 把一次扫描结果落进缓存。
    fn apply_scan(&self, out: ScanOut) {
        let ScanOut { raw, inits, times, has_timeline } = out;
        if has_timeline {
            *self.timeline.borrow_mut() = Some(Rc::new(times));
        }
        let tl = self.timeline.borrow().clone().unwrap_or_else(|| Rc::new(Vec::new()));
        for (idx, v) in inits {
            self.initials.borrow_mut().entry(idx).or_insert(Some(v));
        }
        for (idx, list) in raw {
            let mut points: Vec<(usize, ScalarValue)> = Vec::with_capacity(list.len());
            for (t, v) in list {
                if let Ok(i) = tl.binary_search(&t) {
                    match points.last_mut() {
                        // 同一索引的多次写入(delta/glitch) → 最后一次
                        Some((pi, pv)) if *pi == i => *pv = v,
                        _ => points.push((i, v)),
                    }
                }
            }
            // 值没变(毛刺回到原值)不算变更点
            let init = self.initials.borrow().get(&idx).cloned().flatten();
            let mut collapsed: Vec<(usize, ScalarValue)> = Vec::with_capacity(points.len());
            for (i, v) in points {
                let prev = collapsed.last().map(|(_, p)| p).or(init.as_ref());
                let same = prev.map(|p| sv_same(p, &v)).unwrap_or(false);
                if !same {
                    collapsed.push((i, v));
                }
            }
            self.cols.borrow_mut().entry(idx).or_insert_with(|| {
                Rc::new(Column { points: collapsed })
            });
        }
    }

    /// 取信号变更列(按索引升序, 不含 t0 快照); 必要时触发扫描。
    fn column(&self, idx: usize) -> Result<Rc<Column>, String> {
        if let Some(c) = self.cols.borrow().get(&idx) {
            return Ok(c.clone());
        }
        if !self.scanning.get() {
            self.scanning.set(true);
            let r = self.timeline();
            self.scanning.set(false);
            r?;
        }
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

    /// 信号初值(t0 快照; 没有条目 → None)
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
                let mut size: c_int = 0;
                let mut is_real: c_int = 0;
                let mut is_string: c_int = 0;
                (npi.sig_property)(SIG_SIZE, s, &mut size);
                (npi.sig_property)(SIG_IS_REAL, s, &mut is_real);
                (npi.sig_property)(SIG_IS_STRING, s, &mut is_string);
                let sfull = if full.is_empty() {
                    sname.clone()
                } else {
                    format!("{}.{}", full, sname)
                };
                sigs.push(Sig {
                    handle: s,
                    name: sname,
                    full: sfull,
                    width: if size > 0 { size as usize } else { 1 },
                    is_real: is_real != 0,
                    is_string: is_string != 0,
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
        {
            let mut p = self.prepared.borrow_mut();
            for n in names {
                if let Some(i) = self.resolve_idx(n) {
                    if !p.contains(&i) {
                        p.push(i);
                    }
                }
            }
        }
        if self.timeline.borrow().is_none() {
            if let Err(e) = self.timeline() {
                let mut f = self.fatal.borrow_mut();
                if f.is_none() {
                    *f = Some(e);
                }
            }
        }
    }

    fn step(&mut self, steps: usize) -> Result<(), String> {
        let new_index = self.current_index.saturating_add(steps);
        if new_index > self.max_index() {
            return Err(format!("Step {} would exceed max index {}", steps, self.max_index()));
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
        let col = self.column(idx)?;
        match col.points.binary_search_by(|(i, _)| i.cmp(&offset)) {
            Ok(k) => Ok(col.points[k].1.clone()),
            Err(0) => {
                let v = match self.initial_of(idx) {
                    Some(v) => v,
                    None => ScalarValue::Bit(b'x'),
                };
                Ok(self.normalize(idx, v))
            }
            Err(k) => Ok(col.points[k - 1].1.clone()),
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
        let idx = self
            .resolve_idx(name)
            .ok_or_else(|| format!("Unknown signal: {}", name))?;
        let tl = self.timeline()?;
        if tl.is_empty() {
            return Ok(Vec::new()); // 空时间线(只有头/没有 dump 段)
        }
        let max = tl.len() - 1;
        let col = self.column(idx)?;
        Ok(eval_column(max, &col.points, self.initial_of(idx).as_ref(), &cond))
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

    fn change_points(&self, name: &str) -> Result<Vec<(usize, ScalarValue)>, String> {
        let idx = self
            .resolve_idx(name)
            .ok_or_else(|| format!("Unknown signal: {}", name))?;
        let col = self.column(idx)?;
        let mut out: Vec<(usize, ScalarValue)> = Vec::with_capacity(col.points.len());
        for (i, v) in &col.points {
            out.push((*i, v.clone()));
        }
        Ok(out)
    }

    /// Top-k 变更数: 一次遍历数完所有信号(默认实现会对每个信号各扫一遍)。
    fn signal_change_counts_top(&self, k: usize) -> Vec<(String, usize)> {
        let npi = match npi() {
            Ok(n) => n,
            Err(_) => return Vec::new(),
        };
        let mut counts = vec![0usize; self.sigs.len()];
        let mut chunk_start = 0usize;
        while chunk_start < self.sigs.len() {
            let chunk_end = (chunk_start + CHUNK).min(self.sigs.len());
            let mut it = IterObj::new(npi);
            for i in chunk_start..chunk_end {
                unsafe { (npi.iter.add)(it.this(), self.sigs[i].handle) };
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
                if let Some(&idx) = self.handle_to_idx.get(&(sig as usize)) {
                    counts[idx] += 1;
                }
            }
            drop(it);
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
