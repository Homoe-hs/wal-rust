//! 临时诊断: FstWriter 写 4-bit x/z → FstTrace 读回(开发工具)
use wal_rust::fst::{FstOptions, FstWriter, ScopeType, VarType};
use wal_rust::trace::{FstTrace, Trace};
fn main() {
    let p = std::env::temp_dir().join("rt_xz.fst");
    {
        let mut w = FstWriter::create(&p, FstOptions::default()).unwrap();
        w.set_timescale(-9);
        w.push_scope("top", ScopeType::VcdModule);
        let q = w.create_var("q", 4, VarType::VcdWire);
        w.pop_scope();
        w.emit_time_change(0);
        w.emit_value_change(q, &[b'x', b'x', b'x', b'x']);
        w.emit_time_change(10);
        w.emit_value_change(q, &[b'x', b'0', b'0', b'z']);
        w.emit_time_change(20);
        w.emit_value_change(q, &[b'1', b'x', b'1', b'x']);
        w.close().unwrap();
    }
    let tr = FstTrace::load(&p, "t".to_string()).unwrap();
    for i in 0..3 {
        println!("idx{} = {:?}", i, tr.signal_value("top.q", i).unwrap());
    }
    let _ = std::fs::remove_file(p);
}
