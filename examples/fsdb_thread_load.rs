//! 诊断: 同一进程内**并发**加载两个 FSDB(验证 NPI 是否线程安全/是否需要进程级串行)。
use wal_rust::trace::{FsdbTrace, Trace};
fn main() {
    let fsdb = std::env::args().nth(1).expect("usage: fsdb_thread_load file.fsdb");
    let mut handles = Vec::new();
    for i in 0..4 {
        let f = fsdb.clone();
        handles.push(std::thread::spawn(move || {
            let tr = FsdbTrace::load(std::path::Path::new(&f), format!("t{i}")).expect("load");
            let n = tr.max_index();
            println!("thread {i}: signals={} max_index={}", tr.signal_count(), n);
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    println!("OK");
}
