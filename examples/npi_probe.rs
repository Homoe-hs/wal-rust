//! NPI(VC Apps Native Programming Interface)FSDB 读者的纯 Rust 探针。
//!
//! 目的: 在写 `src/trace/fsdb.rs` 之前, 用**不写一行 C++**的方式验证
//! Synopsys NPI 的 FSDB 读取能力够不够我们 `Trace` 用:
//!   树遍历 / 位宽 / 时间范围 / 逐点值变化(create_vct)/ 值格式(4 态位串/整数/实数)。
//!
//! 关键事实(实测自 Verdi X-2025.06-SP1):
//!   * 头文件是 C++(`#include <map>`、有引用参数), `libNPI.so` 里 123 个
//!     `npi_fsdb_*` 全是 **Itanium mangled** 符号(如 `_Z13npi_fsdb_openPKc`);
//!     函数本身只吃 C 类型, 所以 Rust 侧 `dlsym(mangled)` + `extern "C"` 调用是安全的。
//!   * 库路径: `$VERDI_HOME/share/vcst/linux64/libNPI.so`(或 `share/NPI/lib/linux64/`)。
//!
//! 运行:
//!   VERDI_HOME=/path/to/verdi cargo run --release --example npi_probe -- file.fsdb [sig]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;

// ---------------- NPI 类型(与 npi_fsdb.h 对齐) ----------------

type NpiTime = u64;

#[repr(C)]
#[derive(Clone, Copy)]
pub union NpiValueUnion {
    pub str_: *const c_char,
    pub sint: i32,
    pub uint: u32,
    pub sint64: i64,
    pub uint64: u64,
    pub real: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NpiFsdbValue {
    pub format: i32,
    pub value: NpiValueUnion,
}

#[repr(i32)]
#[derive(Clone, Copy, PartialEq)]
pub enum ValType {
    BinStr = 0,
    OctStr = 1,
    DecStr = 2,
    HexStr = 3,
    Sint = 4,
    Uint = 5,
    Real = 6,
    String = 7,
    EnumStr = 8,
    Sint64 = 9,
    Uint64 = 10,
    ObjType = 11,
}

#[repr(i32)]
#[derive(Clone, Copy)]
pub enum ScopeProp {
    Name = 0,
    FullName = 1,
    DefName = 2,
    Type = 3,
}

#[repr(i32)]
#[derive(Clone, Copy)]
pub enum SigProp {
    Name = 0,
    FullName = 1,
    IsReal = 2,
    HasMember = 3,
    LeftRange = 4,
    RightRange = 5,
    RangeSize = 6,
    IsString = 7,
}

#[repr(i32)]
#[derive(Clone, Copy)]
pub enum FileProp {
    FileName = 0,
    ScaleUnit = 1,
    DumpOffRange = 2,
    HasSeqNum = 3,
    IsCompleted = 4,
    HasGlitch = 5,
    FileVersion = 10,
    SimDate = 11,
    SessionCount = 13,
}

// ---------------- 动态符号解析(Itanium mangled 名) ----------------

struct Npi {
    handle: *mut c_void,
    // 全部按 extern "C" 函数指针调用(签名见下)
    is_fsdb: unsafe extern "C" fn(*const c_char) -> c_int,
    open: unsafe extern "C" fn(*const c_char) -> *mut c_void,
    close: unsafe extern "C" fn(*mut c_void) -> c_int,
    min_time: unsafe extern "C" fn(*mut c_void, *mut NpiTime) -> c_int,
    max_time: unsafe extern "C" fn(*mut c_void, *mut NpiTime) -> c_int,
    file_property_str: unsafe extern "C" fn(c_int, *mut c_void) -> *const c_char,
    file_property: unsafe extern "C" fn(c_int, *mut c_void, *mut c_int) -> c_int,
    iter_top_scope: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    iter_child_scope: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    iter_scope_next: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    iter_scope_stop: unsafe extern "C" fn(*mut c_void) -> c_int,
    iter_sig: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    iter_sig_next: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    iter_sig_stop: unsafe extern "C" fn(*mut c_void) -> c_int,
    scope_property_str: unsafe extern "C" fn(c_int, *mut c_void) -> *const c_char,
    sig_property_str: unsafe extern "C" fn(c_int, *mut c_void) -> *const c_char,
    sig_property: unsafe extern "C" fn(c_int, *mut c_void, *mut c_int) -> c_int,
    sig_by_name: unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void) -> *mut c_void,
    create_vct: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    release_vct: unsafe extern "C" fn(*mut c_void) -> c_int,
    goto_first: unsafe extern "C" fn(*mut c_void) -> c_int,
    goto_next: unsafe extern "C" fn(*mut c_void) -> c_int,
    vct_time: unsafe extern "C" fn(*mut c_void, *mut NpiTime) -> c_int,
    vct_value: unsafe extern "C" fn(*mut c_void, *mut NpiFsdbValue) -> c_int,
    support_version: Option<unsafe extern "C" fn() -> *const c_char>,
    // npi_init(int&, char**&) / npi_end(): C++ 引用在 ABI 上就是指针, 可直接调
    init: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char) -> c_int,
    end: unsafe extern "C" fn() -> c_int,
    /// `npiFsdbTimeBasedVcIter`: 多信号**归并**的"时间→信号"变更流迭代器。
    /// 是 C++ 类, 但布局只有一个 `Impl* m_impl`(8 字节), 纯 Rust 分配缓冲 +
    /// 调构造/析构符号即可, 仍然不需要 C++ 编译器。2018/25A 两代 libNPI.so 都导出。
    iter: Option<TimeIterSyms>,
}

/// `npiFsdbTimeBasedVcIter` 的 C++ 成员函数符号(Ittanium mangled, 非虚函数, 可直接 dlsym)
struct TimeIterSyms {
    ctor: unsafe extern "C" fn(*mut c_void),
    dtor: unsafe extern "C" fn(*mut c_void),
    add: unsafe extern "C" fn(*mut c_void, *mut c_void) -> i64,
    start: unsafe extern "C" fn(*mut c_void, NpiTime, NpiTime),
    next: unsafe extern "C" fn(*mut c_void, *mut NpiTime, *mut *mut c_void) -> i64,
    get_value: unsafe extern "C" fn(*mut c_void, *mut NpiFsdbValue) -> c_int,
    stop: unsafe extern "C" fn(*mut c_void),
}

/// C++ 对象缓冲: 实际只有 8 字节, 给 64 字节余量 + 16 字节对齐。
#[repr(C, align(16))]
struct IterObj([u64; 8]);

impl Npi {
    fn load(lib_candidates: &[String]) -> Result<Npi, String> {
        unsafe {
            let mut handle = ptr::null_mut();
            let mut tried = Vec::new();
            for p in lib_candidates {
                let c = CString::new(p.as_str()).unwrap();
                handle = libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
                tried.push(p.clone());
                if !handle.is_null() {
                    break;
                }
            }
            if handle.is_null() {
                return Err(format!(
                    "dlopen 失败(试过: {:?}); 提示: 需要 LD_LIBRARY_PATH 含 $VERDI_HOME/share/NPI/lib/linux64\n  {}",
                    tried,
                    CStr::from_ptr(libc::dlerror()).to_string_lossy()
                ));
            }
            // 必需符号缺失 → 报错; 可选符号(版本号之类)→ None, 老版本 Verdi 缺少是正常的
            macro_rules! sym {
                ($name:expr, $ty:ty) => {{
                    let cs = CString::new($name).unwrap();
                    let p = libc::dlsym(handle, cs.as_ptr());
                    if p.is_null() {
                        return Err(format!("符号缺失(必需): {}", $name));
                    }
                    std::mem::transmute::<*mut c_void, $ty>(p)
                }};
            }
            macro_rules! sym_opt {
                ($name:expr, $ty:ty) => {{
                    let cs = CString::new($name).unwrap();
                    let p = libc::dlsym(handle, cs.as_ptr());
                    if p.is_null() {
                        None
                    } else {
                        Some(std::mem::transmute::<*mut c_void, $ty>(p))
                    }
                }};
            }
            Ok(Npi {
                handle,
                is_fsdb: sym!("_Z16npi_fsdb_is_fsdbPKc", unsafe extern "C" fn(*const c_char) -> c_int),
                open: sym!("_Z13npi_fsdb_openPKc", unsafe extern "C" fn(*const c_char) -> *mut c_void),
                close: sym!("_Z14npi_fsdb_closePv", unsafe extern "C" fn(*mut c_void) -> c_int),
                min_time: sym!("_Z17npi_fsdb_min_timePvPy", unsafe extern "C" fn(*mut c_void, *mut NpiTime) -> c_int),
                max_time: sym!("_Z17npi_fsdb_max_timePvPy", unsafe extern "C" fn(*mut c_void, *mut NpiTime) -> c_int),
                file_property_str: sym!("_Z26npi_fsdb_file_property_str23npiFsdbFilePropertyTypePv", unsafe extern "C" fn(c_int, *mut c_void) -> *const c_char),
                file_property: sym!("_Z22npi_fsdb_file_property23npiFsdbFilePropertyTypePvPi", unsafe extern "C" fn(c_int, *mut c_void, *mut c_int) -> c_int),
                iter_top_scope: sym!("_Z23npi_fsdb_iter_top_scopePv", unsafe extern "C" fn(*mut c_void) -> *mut c_void),
                iter_child_scope: sym!("_Z25npi_fsdb_iter_child_scopePv", unsafe extern "C" fn(*mut c_void) -> *mut c_void),
                iter_scope_next: sym!("_Z24npi_fsdb_iter_scope_nextPv", unsafe extern "C" fn(*mut c_void) -> *mut c_void),
                iter_scope_stop: sym!("_Z24npi_fsdb_iter_scope_stopPv", unsafe extern "C" fn(*mut c_void) -> c_int),
                iter_sig: sym!("_Z17npi_fsdb_iter_sigPv", unsafe extern "C" fn(*mut c_void) -> *mut c_void),
                iter_sig_next: sym!("_Z22npi_fsdb_iter_sig_nextPv", unsafe extern "C" fn(*mut c_void) -> *mut c_void),
                iter_sig_stop: sym!("_Z22npi_fsdb_iter_sig_stopPv", unsafe extern "C" fn(*mut c_void) -> c_int),
                scope_property_str: sym!("_Z27npi_fsdb_scope_property_str24npiFsdbScopePropertyTypePv", unsafe extern "C" fn(c_int, *mut c_void) -> *const c_char),
                sig_property_str: sym!("_Z25npi_fsdb_sig_property_str22npiFsdbSigPropertyTypePv", unsafe extern "C" fn(c_int, *mut c_void) -> *const c_char),
                sig_property: sym!("_Z21npi_fsdb_sig_property22npiFsdbSigPropertyTypePvPi", unsafe extern "C" fn(c_int, *mut c_void, *mut c_int) -> c_int),
                sig_by_name: sym!("_Z20npi_fsdb_sig_by_namePvPKcS_", unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void) -> *mut c_void),
                create_vct: sym!("_Z19npi_fsdb_create_vctPv", unsafe extern "C" fn(*mut c_void) -> *mut c_void),
                release_vct: sym!("_Z20npi_fsdb_release_vctPv", unsafe extern "C" fn(*mut c_void) -> c_int),
                goto_first: sym!("_Z19npi_fsdb_goto_firstPv", unsafe extern "C" fn(*mut c_void) -> c_int),
                goto_next: sym!("_Z18npi_fsdb_goto_nextPv", unsafe extern "C" fn(*mut c_void) -> c_int),
                vct_time: sym!("_Z17npi_fsdb_vct_timePvPy", unsafe extern "C" fn(*mut c_void, *mut NpiTime) -> c_int),
                vct_value: sym!("_Z18npi_fsdb_vct_valuePvP12npiFsdbValue", unsafe extern "C" fn(*mut c_void, *mut NpiFsdbValue) -> c_int),
                support_version: sym_opt!("_Z24npi_fsdb_support_versionv", unsafe extern "C" fn() -> *const c_char),
                init: sym!("_Z8npi_initRiRPPc", unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char) -> c_int),
                end: sym!("_Z7npi_endv", unsafe extern "C" fn() -> c_int),
                iter: load_time_iter(handle).ok(),
            })
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

/// `npiFsdbTimeBasedVcIter` 的符号(老版本缺任一符号 → None, 探针退化但不崩)
fn load_time_iter(handle: *mut c_void) -> Result<TimeIterSyms, String> {
    unsafe {
        macro_rules! s {
            ($name:expr, $ty:ty) => {{
                let cs = CString::new($name).unwrap();
                let p = libc::dlsym(handle, cs.as_ptr());
                if p.is_null() {
                    return Err(format!("符号缺失: {}", $name));
                }
                std::mem::transmute::<*mut c_void, $ty>(p)
            }};
        }
        Ok(TimeIterSyms {
            ctor: s!("_ZN22npiFsdbTimeBasedVcIterC1Ev", unsafe extern "C" fn(*mut c_void)),
            dtor: s!("_ZN22npiFsdbTimeBasedVcIterD1Ev", unsafe extern "C" fn(*mut c_void)),
            add: s!("_ZN22npiFsdbTimeBasedVcIter3addEPv", unsafe extern "C" fn(*mut c_void, *mut c_void) -> i64),
            start: s!("_ZN22npiFsdbTimeBasedVcIter10iter_startEyy", unsafe extern "C" fn(*mut c_void, NpiTime, NpiTime)),
            next: s!("_ZN22npiFsdbTimeBasedVcIter9iter_nextERyRPv", unsafe extern "C" fn(*mut c_void, *mut NpiTime, *mut *mut c_void) -> i64),
            get_value: s!("_ZN22npiFsdbTimeBasedVcIter9get_valueER12npiFsdbValue", unsafe extern "C" fn(*mut c_void, *mut NpiFsdbValue) -> c_int),
            stop: s!("_ZN22npiFsdbTimeBasedVcIter9iter_stopEv", unsafe extern "C" fn(*mut c_void)),
        })
    }
}

/// 值 → 4 态位串 / 数字的文本
fn fmt_value(v: &NpiFsdbValue) -> String {
    unsafe {
        match v.format {
            0 => Npi::cstr(v.value.str_),                    // BinStr: "1010" / "xx01"
            3 => Npi::cstr(v.value.str_),                    // HexStr
            6 => format!("{}", v.value.real),                // Real
            9 => format!("{}", v.value.sint64),
            10 => format!("{}", v.value.uint64),
            5 => format!("{}", v.value.uint),
            _ => {
                let s = Npi::cstr(v.value.str_);
                if s.is_empty() {
                    format!("<format={} sint64={}>", v.format, v.value.sint64)
                } else {
                    s
                }
            }
        }
    }
}

#[derive(Clone)]
struct SigInfo {
    sig: *mut c_void,
    full: String,
    width: i32,
}

/// 递归遍历作用域树(NPI 的 iterator 需要显式 stop, 别漏)
unsafe fn walk_scope(npi: &Npi, scope: *mut c_void, depth: usize, out: &mut Vec<SigInfo>) {
    let full = Npi::cstr((npi.scope_property_str)(ScopeProp::FullName as c_int, scope));
    let name = Npi::cstr((npi.scope_property_str)(ScopeProp::Name as c_int, scope));
    println!("{:indent$}SCOPE {} (full={})", "", name, full, indent = depth * 2);
    let it = (npi.iter_sig)(scope);
    if !it.is_null() {
        loop {
            let sig = (npi.iter_sig_next)(it);
            if sig.is_null() {
                break;
            }
            let sname = Npi::cstr((npi.sig_property_str)(SigProp::Name as c_int, sig));
            let (mut l, mut r, mut w) = (0, 0, 0);
            (npi.sig_property)(SigProp::LeftRange as c_int, sig, &mut l);
            (npi.sig_property)(SigProp::RightRange as c_int, sig, &mut r);
            (npi.sig_property)(SigProp::RangeSize as c_int, sig, &mut w);
            let sfull = if full.is_empty() { sname.clone() } else { format!("{}.{}", full, sname) };
            println!("{:indent$}  SIG {:<24} [{l}:{r}] size={w}", "", sfull, indent = depth * 2);
            out.push(SigInfo { sig, full: sfull, width: w });
        }
        (npi.iter_sig_stop)(it);
    }
    let cit = (npi.iter_child_scope)(scope);
    if !cit.is_null() {
        loop {
            let child = (npi.iter_scope_next)(cit);
            if child.is_null() {
                break;
            }
            walk_scope(npi, child, depth + 1, out);
        }
        (npi.iter_scope_stop)(cit);
    }
}

fn lib_candidates() -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(p) = std::env::var("WAL_NPI_LIB") {
        v.push(p);
    }
    // VERDI_HOME 优先; 若没设, 从 PATH 里的 verdi/fsdbdebug 反推(内网常见做法)
    let home = std::env::var("VERDI_HOME").ok().or_else(|| {
        std::env::var("PATH").ok().and_then(|p| {
            for d in p.split(':') {
                let p = std::path::Path::new(d);
                if let Some(parent) = p.parent() {
                    if parent.join("share/NPI/inc/npi_fsdb.h").exists() {
                        return Some(parent.to_string_lossy().to_string());
                    }
                }
            }
            None
        })
    });
    if let Some(h) = home {
        v.push(format!("{}/share/vcst/linux64/libNPI.so", h));
        v.push(format!("{}/share/NPI/lib/linux64/libNPI.so", h));
    }
    v
}

/// NPI 还要在 LD_LIBRARY_PATH 的某个目录下找到 `etc/`(Verdi 资源目录),
/// 否则 `[NPI ERROR] Failed to find Verdi resource directory (etc/)`。
/// 老实的做法是让用户自己设; 这里把它自动化: 只要发现某个目录下有 etc/ 就补进去。
fn ensure_ld_library_path(lib: &std::path::Path) {
    let mut dirs: Vec<String> = Vec::new();
    if let Some(d) = lib.parent() {
        // `share/NPI/lib/linux64/etc` 通常是指向 $VERDI_HOME/etc 的软链
        if d.join("etc").exists() {
            dirs.push(d.to_string_lossy().to_string());
        }
    }
    if let Ok(h) = std::env::var("VERDI_HOME") {
        let d = format!("{}/share/NPI/lib/linux64", h);
        if std::path::Path::new(&d).join("etc").exists() {
            dirs.push(d);
        }
        let d = format!("{}/etc", h);
        if std::path::Path::new(&d).exists() {
            dirs.push(format!("{}/share/NPI/lib/linux64", h));
        }
    }
    if std::env::var("WAL_NPI_NO_LDPATH").is_ok() {
        return;
    }
    if dirs.is_empty() {
        return;
    }
    let old = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
    let mut parts: Vec<String> = old.split(':').filter(|s| !s.is_empty()).map(String::from).collect();
    let mut changed = false;
    for d in dirs {
        if !parts.iter().any(|p| p == &d) {
            parts.insert(0, d);
            changed = true;
        }
    }
    if changed {
        // SAFETY: 单线程启动路径, npi_init 之前设置。
        unsafe { std::env::set_var("LD_LIBRARY_PATH", parts.join(":")) };
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: npi_probe <file.fsdb> [sig]");
        std::process::exit(2);
    }
    let npi = match Npi::load(&lib_candidates()) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    };
    if let Ok(p) = std::env::var("WAL_NPI_LIB") {
        ensure_ld_library_path(std::path::Path::new(&p));
    } else if let Ok(h) = std::env::var("VERDI_HOME") {
        for c in [format!("{}/share/vcst/linux64/libNPI.so", h), format!("{}/share/NPI/lib/linux64/libNPI.so", h)] {
            let p = std::path::Path::new(&c);
            if p.exists() {
                ensure_ld_library_path(p);
                break;
            }
        }
    }
    unsafe {
        // NPI 要求先 npi_init(argc, argv) 才能 open(实测: 否则
        // "[NPI ERROR] Please call npi_init() before npi_fsdb_open.")
        let mut owned: Vec<std::ffi::CString> = std::env::args()
            .map(|a| std::ffi::CString::new(a).unwrap())
            .collect();
        let mut argv: Vec<*mut c_char> = owned.iter_mut().map(|c| c.as_ptr() as *mut c_char).collect();
        argv.push(ptr::null_mut());
        let mut argc: c_int = (argv.len() - 1) as c_int;
        let mut argv_ptr = argv.as_mut_ptr();
        let rc = (npi.init)(&mut argc, &mut argv_ptr);
        println!("npi_init -> {}", rc);
        println!(
            "NPI support_version: {}",
            match npi.support_version { Some(f) => Npi::cstr(f()), None => "n/a(老版本库无此符号)".to_string() }
        );
        let path = CString::new(args[1].as_str()).unwrap();
        println!("is_fsdb: {}", (npi.is_fsdb)(path.as_ptr()));
        let file = (npi.open)(path.as_ptr());
        if file.is_null() {
            eprintln!("error: npi_fsdb_open 失败(库已 dlopen, 能力已解析)");
            std::process::exit(1);
        }
        let mut t0: NpiTime = 0;
        let mut t1: NpiTime = 0;
        (npi.min_time)(file, &mut t0);
        (npi.max_time)(file, &mut t1);
        println!(
            "file: version={} scale={} completed={} giitch={} min={} max={}",
            Npi::cstr((npi.file_property_str)(FileProp::FileVersion as c_int, file)),
            Npi::cstr((npi.file_property_str)(FileProp::ScaleUnit as c_int, file)),
            (npi.file_property)(FileProp::IsCompleted as c_int, file, &mut 0) as i32,
            (npi.file_property)(FileProp::HasGlitch as c_int, file, &mut 0) as i32,
            t0, t1
        );

        // ---- 树: 作用域 → 信号(递归) ----
        let mut sig_list: Vec<SigInfo> = Vec::new();
        let top = (npi.iter_top_scope)(file);
        if !top.is_null() {
            loop {
                let s = (npi.iter_scope_next)(top);
                if s.is_null() {
                    break;
                }
                walk_scope(&npi, s, 0, &mut sig_list);
            }
            (npi.iter_scope_stop)(top);
        }

        // ---- 归并变更流: npiFsdbTimeBasedVcIter ----
        // WAL_NPI_ITER=N: 把前 N 个信号加进一个归并迭代器, 打印 (t, sig, value) 流。
        // 这是统一引擎"一遍扫描出所有查询信号变更列"的原语; 全加进去还能算出
        // 全局时间线(所有信号变更时间的并集)。
        if let Some(ti) = &npi.iter {
            let k: usize = std::env::var("WAL_NPI_ITER").ok().and_then(|v| v.parse().ok())
                .unwrap_or(0);
            if k > 0 {
                let k = k.min(sig_list.len());
                let rounds: usize = std::env::var("WAL_NPI_ITER_ROUNDS").ok()
                    .and_then(|v| v.parse().ok()).unwrap_or(1);
                let mut obj = IterObj([0u64; 8]);
                let this = obj.0.as_mut_ptr() as *mut c_void;
                (ti.ctor)(this);
                for s in &sig_list[..k] {
                    (ti.add)(this, s.sig);
                }
                let by_handle: std::collections::HashMap<usize, &str> = sig_list[..k]
                    .iter()
                    .map(|s| (s.sig as usize, s.full.as_str()))
                    .collect();
                for r in 0..rounds {
                    (ti.start)(this, t0, t1);
                    let mut n = 0usize;
                    let mut last_t = u64::MAX;
                    let mut uniq_t = std::collections::BTreeSet::new();
                    println!("MERGE 迭代器 round {}: {} 个信号", r + 1, k);
                    loop {
                        let mut t: NpiTime = 0;
                        let mut sig: *mut c_void = ptr::null_mut();
                        let rc = (ti.next)(this, &mut t, &mut sig);
                        if rc <= 0 || sig.is_null() {
                            if n < 3 {
                                println!("  iter_next rc={} t={} sig=null → 结束", rc, t);
                            }
                            break;
                        }
                        if t > 0 {
                            uniq_t.insert(t);
                        }
                        if n < 6 {
                            let mut v = NpiFsdbValue { format: 0, value: NpiValueUnion { str_: ptr::null() } };
                            let ok = (ti.get_value)(this, &mut v) != 0;
                            println!(
                                "  t={:<9} sig={:<22} value={} (rc={})",
                                t,
                                by_handle.get(&(sig as usize)).copied().unwrap_or("?"),
                                if ok { fmt_value(&v) } else { "<get_value rc=0>".into() },
                                rc
                            );
                        }
                        n += 1;
                        last_t = t;
                    }
                    println!("  共 {} 条变更, 去重时间点(>0) {} 个, 最后 t={}", n, uniq_t.len(), last_t);
                }
                (ti.stop)(this);
                (ti.dtor)(this);
            }
        }

        // ---- 值变化: create_vct ----
        let want = args.get(2).cloned().unwrap_or_default();
        let target = sig_list.iter().find(|s| {
            want.is_empty() || s.full == want || s.full.ends_with(&format!(".{}", want)) || s.full.contains(&want)
        });
        if let Some(SigInfo { sig, full: name, .. }) = target {
            let vct = (npi.create_vct)(*sig);
            if vct.is_null() {
                println!("SIG {}: create_vct 返回空", name);
            } else {
                println!("SIG {} 值变化:", name);
                let mut n = 0;
                // 关键: `npiFsdbValue.format` 是**入参**(要什么格式), 不是出参!
                // 官方 example 先 `val.format = npiFsdbBinStrVal` 再调 vct_value,
                // 成功(返回非 0)后从 `val.value.str` 取 4 态位串。
                let mut read = |vct: *mut c_void, format: i32| -> Option<String> {
                    let mut v = NpiFsdbValue {
                        format,
                        value: NpiValueUnion { str_: ptr::null() },
                    };
                    if (npi.vct_value)(vct, &mut v) != 0 {
                        Some(fmt_value(&v))
                    } else {
                        None
                    }
                };
                if (npi.goto_first)(vct) != 0 {
                    loop {
                        let mut t: NpiTime = 0;
                        (npi.vct_time)(vct, &mut t);
                        if n < 8 {
                            let s = read(vct, 0) // BinStr(4 态位串)
                                .or_else(|| read(vct, 6)) // RealVal
                                .unwrap_or_else(|| "<vct_value rc=0>".to_string());
                            println!("  t={:<8} {}", t, s);
                        }
                        n += 1;
                        if (npi.goto_next)(vct) == 0 {
                            break;
                        }
                    }
                }
                println!("  ... 共 {} 个值变化", n);
                (npi.release_vct)(vct);
            }
        }
        (npi.close)(file);
        (npi.end)();
    }
}
