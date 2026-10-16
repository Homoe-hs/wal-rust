//! 阶段墙钟计时(+ 可选采样剖析 / 火焰图)。
//!
//! * **默认零成本**: 只有 `WAL_PROF=1` 时才记录;退出前打印每个阶段的 墙钟/次数/占比。
//! * **`--features prof`**: 每个**顶层**阶段单独采样, 退出前写
//!   `<WAL_PROF_OUT|.wal-rust-prof>/flamegraph-<阶段>.svg`。
//!   采样器是 pprof-rs(用户态 SIGPROF + backtrace), **不需要 perf、不需要 root**。
//! * 用法:
//!   ```bash
//!   # 只看阶段账(用普通 release 二进制即可)
//!   WAL_PROF=1 ./target/release/wal-rust --stdin -l big.fsdb < probes.txt
//!   # 火焰图(需要带符号的二进制)
//!   CARGO_PROFILE_RELEASE_DEBUG=1 CARGO_PROFILE_RELEASE_STRIP=false \
//!     cargo build --release --features prof
//!   WAL_PROF=1 WAL_PROF_OUT=.tools/prof ./target/release/wal-rust ...
//!   ```

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

#[derive(Default, Clone, Copy)]
struct Rec {
    ns: u64,
    calls: u64,
}

struct Table {
    order: Vec<&'static str>,
    map: std::collections::HashMap<&'static str, Rec>,
}

fn table() -> &'static Mutex<Table> {
    static T: OnceLock<Mutex<Table>> = OnceLock::new();
    T.get_or_init(|| {
        Mutex::new(Table { order: Vec::new(), map: std::collections::HashMap::new() })
    })
}

/// 阶段嵌套深度。
static DEPTH: AtomicUsize = AtomicUsize::new(0);
/// 同一时刻只允许一个采样器(嵌套时只采被 `WAL_PROF_SAMPLE` 选中的那一层)。
static SAMPLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `WAL_PROF=1|on|yes|true` 打开。
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        matches!(
            std::env::var("WAL_PROF").as_deref(),
            Ok("1") | Ok("on") | Ok("yes") | Ok("true")
        ) || trace_on()   // 只想逐探针归因时, 不必再设 WAL_PROF
    })
}

/// `WAL_PROF_TRACE=1`: 每**次**阶段调用都打一行(会话模式下就能逐个探针归因)。
fn trace_on() -> bool {
    static T: OnceLock<bool> = OnceLock::new();
    *T.get_or_init(|| {
        matches!(
            std::env::var("WAL_PROF_TRACE").as_deref(),
            Ok("1") | Ok("on") | Ok("yes") | Ok("true")
        )
    })
}

fn out_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("WAL_PROF_OUT").unwrap_or_else(|_| ".wal-rust-prof".into()))
}

/// 上一次 mark 的时刻(阶段之间用 mark 划界, 免去给大块代码重新缩进)。
static LAST_MARK: Mutex<Option<Instant>> = Mutex::new(None);

fn mark_impl(name: &'static str) {
    let now = Instant::now();
    let Ok(mut last) = LAST_MARK.lock() else { return };
    let prev = last.replace(now);
    let Some(prev) = prev else { return };
    let dt = now.duration_since(prev).as_nanos() as u64;
    if let Ok(mut t) = table().lock() {
        if !t.map.contains_key(name) {
            t.order.push(name);
        }
        let e = t.map.entry(name).or_default();
        e.ns += dt;
        e.calls += 1;
    }
}

/// 在阶段边界打点: 把"距上一个 mark"的墙钟记到 **name** 名下(名字描述的是刚结束的那段)。
pub fn mark(name: &'static str) {
    if enabled() {
        mark_impl(name);
    }
}

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// `WAL_PROF_SAMPLE=load,session,...`: 只对这些**顶层**阶段开采样(默认一个都不采, 只计时)。
/// 不设这个变量时, 哪怕带了 `--features prof` 也不会生成任何火焰图。
#[cfg(feature = "prof")]
fn should_sample(name: &str) -> bool {
    static LIST: OnceLock<Vec<String>> = OnceLock::new();
    let list = LIST.get_or_init(|| {
        std::env::var("WAL_PROF_SAMPLE")
            .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default()
    });
    list.iter().any(|p| p == name || p == "*")
}

/// 包住一个阶段: 计时(总开);若开了 `prof` feature 且该阶段被 `WAL_PROF_SAMPLE`
/// 选中(同一时刻只允许一个采样器), 同时采样并写火焰图。
pub fn phase<T>(name: &'static str, f: impl FnOnce() -> T) -> T {
    if !enabled() {
        return f();
    }
    DEPTH.fetch_add(1, Ordering::Relaxed);
    #[cfg(feature = "prof")]
    let guard = if should_sample(name) && !SAMPLING.swap(true, Ordering::SeqCst) {
        match start_sampler() {
            Some(g) => Some(g),
            None => {
                SAMPLING.store(false, Ordering::SeqCst);
                None
            }
        }
    } else {
        None
    };
    let t0 = Instant::now();
    let out = f();
    let dt = t0.elapsed().as_nanos() as u64;
    DEPTH.fetch_sub(1, Ordering::Relaxed);
    let mut nth = 0u64;
    if let Ok(mut t) = table().lock() {
        if !t.map.contains_key(name) {
            t.order.push(name);
        }
        let e = t.map.entry(name).or_default();
        e.ns += dt;
        e.calls += 1;
        nth = e.calls;
    }
    if trace_on() {
        // 会话模式下按顺序逐个探针归因(与 probes.wal 的行序一一对应)
        eprintln!("[prof] trace {} #{} {:.3}ms", name, nth, dt as f64 / 1e6);
    }
    #[cfg(feature = "prof")]
    if let Some(g) = guard {
        dump_flamegraph(g, name);
    }
    out
}

#[cfg(feature = "prof")]
fn start_sampler() -> Option<pprof::ProfilerGuard<'static>> {
    match pprof::ProfilerGuardBuilder::default()
        .frequency(499)                     // ~0.5ms 一次, 够看分布又不至于太吵
        .blocklist(&["libc", "libgcc", "pthread", "vdso", "ld-linux"])
        .build()
    {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("[prof] 采样器启动失败(退回只计阶段): {e}");
            None
        }
    }
}

#[cfg(feature = "prof")]
fn dump_flamegraph(guard: pprof::ProfilerGuard<'static>, name: &str) {
    SAMPLING.store(false, Ordering::SeqCst);
    let report = match guard.report().build() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[prof] {name}: 生成报告失败: {e}");
            return;
        }
    };
    let dir = out_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("[prof] 建目录 {} 失败: {e}", dir.display());
        return;
    }
    let path = dir.join(format!("flamegraph-{}.svg", sanitize(name)));
    match std::fs::File::create(&path) {
        Ok(mut fh) => {
            let mut opts = pprof::flamegraph::Options::default();
            opts.image_width = Some(2400);
            opts.min_width = 0.4;
            match report.flamegraph_with_options(&mut fh, &mut opts) {
                Ok(()) => eprintln!("[prof] 火焰图 → {}", path.display()),
                Err(e) => eprintln!("[prof] 写 {} 失败: {e}", path.display()),
            }
        }
        Err(e) => eprintln!("[prof] 建 {} 失败: {e}", path.display()),
    }
}

/// 退出前打印阶段账(按累计墙钟降序)。
pub fn summary() {
    if !enabled() {
        return;
    }
    let Ok(t) = table().lock() else { return };
    if t.order.is_empty() {
        return;
    }
    let total: u64 = t.map.values().map(|r| r.ns).sum();
    let mut rows: Vec<(&str, Rec)> = t.order.iter().map(|n| (*n, t.map[n])).collect();
    rows.sort_by_key(|(_, r)| std::cmp::Reverse(r.ns));
    eprintln!("[prof] 阶段账(墙钟, 累计 {:.3}s):", total as f64 / 1e9);
    for (name, r) in rows {
        let share = if total > 0 { 100.0 * r.ns as f64 / total as f64 } else { 0.0 };
        eprintln!(
            "[prof]   {:<28} {:>9.3}s  ×{:<6}  {:>5.1}%",
            name,
            r.ns as f64 / 1e9,
            r.calls,
            share
        );
    }
    if cfg!(feature = "prof") {
        eprintln!("[prof] 火焰图目录: {}", out_dir().display());
    }
}
