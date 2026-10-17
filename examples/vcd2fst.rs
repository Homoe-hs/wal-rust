//! 开发工具: VCD → FST(复用 vcd_to_fst_streaming), 给 bug-hunt 的跨格式差分用。
use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("用法: vcd2fst <in.vcd> <out.fst>");
        std::process::exit(2);
    }
    let vcd = Path::new(&args[1]);
    let fst = Path::new(&args[2]);
    let resume = std::path::PathBuf::from(format!("{}.resume", args[2]));
    match wal_rust::vcd::convert::vcd_to_fst_streaming(vcd, fst, &resume, Arc::new(AtomicU64::new(0))) {
        Ok(()) => println!("ok {} -> {}", vcd.display(), fst.display()),
        Err(e) => {
            eprintln!("转换失败: {}", e);
            std::process::exit(1);
        }
    }
}
