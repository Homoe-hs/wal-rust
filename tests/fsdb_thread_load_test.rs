//! BUG-021 回归: NPI 是进程级全局状态, 之前**多线程同时 `FsdbTrace::load` 会 SIGSEGV**。
//! 修复: `FsdbTrace::load` 内部用进程级 mutex 串行化整段加载(init+open+遍历)。
//!
//! 需要 Verdi/NPI + 同源 FSDB(没设 `WAL_FSDB_TEST_FILE` 就跳过):
//! ```bash
//! WAL_FSDB_TEST_FILE=/path/mini.fsdb cargo test --release --test fsdb_thread_load_test -- --include-ignored
//! ```
use wal_rust::trace::{FsdbTrace, Trace};

#[test]
#[ignore = "需要 Verdi/NPI(libNPI.so)+ FSDB: 用 --include-ignored 跑(见 scripts/ci.sh gates)"]
fn concurrent_fsdb_loads_are_serialized() {
    let fsdb = match std::env::var("WAL_FSDB_TEST_FILE") {
        Ok(v) if !v.is_empty() => std::path::PathBuf::from(v),
        _ => {
            eprintln!("skip: 未设 WAL_FSDB_TEST_FILE");
            return;
        }
    };
    if !fsdb.exists() {
        eprintln!("skip: {} 不存在", fsdb.display());
        return;
    }
    // 先单线程量一次基准信号数, 供并发结果对齐
    let expected = {
        let tr = FsdbTrace::load(&fsdb, "warm".to_string()).expect("基准加载");
        tr.signal_count()
    };
    let mut handles = Vec::new();
    for i in 0..4 {
        let f = fsdb.clone();
        handles.push(std::thread::spawn(move || {
            let tr = FsdbTrace::load(&f, format!("t{i}")).expect("并发加载");
            let n = tr.signal_count();
            let mx = tr.max_index();
            (n, mx)
        }));
    }
    let results: Vec<(usize, usize)> = handles.into_iter().map(|h| h.join().expect("线程未崩溃")).collect();
    for (n, _mx) in &results {
        assert_eq!(*n, expected, "并发加载的信号数应与单线程一致");
    }
}
