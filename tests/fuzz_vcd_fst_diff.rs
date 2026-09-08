//! 随机波形 VCD ↔ FST 差分对拍。
//!
//! 目的: 内网报过 "NE/changes 计数漂移"(本地不可复现)。矩阵里的固定夹具覆盖面
//! 有限,这里用**确定性伪随机**生成大量小波形(含 x/z、delta 周期、毛刺、宽向量),
//! 同一波形分别写成 VCD 与 FST,再对一批表达式逐值比对。任何不一致都打印种子与
//! 波形原文,便于内网复现。
//!
//! 运行: cargo test --test fuzz_vcd_fst_diff -- --nocapture
//! 环境变量:
//!   WAL_FUZZ_N   波形个数(默认 40)
//!   WAL_FUZZ_SEED 起始种子(默认 0x9E3779B97F4A7C15)

use wal_rust::fst::{FstOptions, FstWriter, ScopeType, VarType};
use wal_rust::trace::{ScalarValue, Trace, VcdTrace};
use wal_rust::wal::ast::Value;
use wal_rust::wal::eval::Evaluator;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
}

struct Sig {
    name: String,
    width: usize,
    /// 每个索引的最终值(0/1/x/z 字符序列,长度 = width)
    values: Vec<Vec<u8>>,
    /// 每个索引是否有一次"同索引二次写入"(delta 周期)
    deltas: Vec<bool>,
}

fn rand_char(rng: &mut Rng) -> u8 {
    match rng.below(10) {
        0 => b'x',
        1 => b'z',
        2..=5 => b'1',
        _ => b'0',
    }
}

fn rand_value(rng: &mut Rng, width: usize) -> Vec<u8> {
    let mut v: Vec<u8> = (0..width).map(|_| rand_char(rng)).collect();
    // 大部分时候是全 0/全 1 的规整值,避免全是 x/z 导致退化
    if rng.below(3) == 0 {
        let fill = if rng.below(2) == 0 { b'0' } else { b'1' };
        v = vec![fill; width];
    }
    v
}

fn gen_wave(rng: &mut Rng) -> (Vec<Sig>, Vec<u64>) {
    let n_sig = 1 + rng.below(4) as usize;
    let n_ts = 2 + rng.below(24) as usize;
    let times: Vec<u64> = (0..n_ts as u64).map(|i| i * 10).collect();

    let mut sigs = Vec::new();
    for si in 0..n_sig {
        let width = match rng.below(5) {
            0 => 1,
            1 => 4,
            2 => 8,
            3 => 45,
            _ => 128,
        };
        let mut values = Vec::with_capacity(n_ts);
        let mut deltas = Vec::with_capacity(n_ts);
        let mut cur = rand_value(rng, width);
        for t in 0..n_ts {
            if t > 0 {
                match rng.below(10) {
                    0 | 1 => {}                      // 保持不变
                    2 => cur = rand_value(rng, width), // 新值
                    3 => {
                        // 毛刺: 变一下再变回来(同索引两次写入 → 最后写入胜出)
                        cur = rand_value(rng, width);
                    }
                    _ => cur = rand_value(rng, width),
                }
            }
            values.push(cur.clone());
            // 约 1/5 的时间点制造 delta 周期(同索引两次写入,最终值 = values[t])
            deltas.push(rng.below(5) == 0);
        }
        sigs.push(Sig { name: format!("s{}", si), width, values, deltas });
    }
    (sigs, times)
}

fn to_vcd(sigs: &[Sig], times: &[u64]) -> String {
    let mut s = String::from("$timescale 1ns $end\n$scope module t $end\n");
    for (i, sig) in sigs.iter().enumerate() {
        // id 用可打印字符
        s.push_str(&format!("$var wire {} {} {} $end\n", sig.width, (b'!' + i as u8) as char, sig.name));
    }
    s.push_str("$upscope $end\n$enddefinitions $end\n");
    for (ti, t) in times.iter().enumerate() {
        s.push_str(&format!("#{}\n", t));
        for (i, sig) in sigs.iter().enumerate() {
            let id = (b'!' + i as u8) as char;
            let v = &sig.values[ti];
            if sig.deltas[ti] && ti > 0 {
                // 同索引先写一个不同的值(毛刺),再写最终值
                let mut glitch = v.clone();
                if glitch[0] == b'0' { glitch[0] = b'1'; } else { glitch[0] = b'0'; }
                emit_vcd_value(&mut s, &glitch, id);
            }
            emit_vcd_value(&mut s, v, id);
        }
    }
    s
}

fn emit_vcd_value(s: &mut String, v: &[u8], id: char) {
    if v.len() == 1 {
        s.push_str(&format!("{}{}\n", v[0] as char, id));
    } else {
        s.push_str(&format!("b{} {}\n", String::from_utf8_lossy(v), id));
    }
}

fn write_fst(path: &std::path::Path, sigs: &[Sig], times: &[u64]) {
    let mut w = FstWriter::create(path, FstOptions::default()).unwrap();
    w.push_scope("t", ScopeType::VcdModule);
    let handles: Vec<u32> = sigs.iter()
        .map(|s| w.create_var(&s.name, s.width as u32, VarType::VcdWire))
        .collect();
    w.pop_scope();
    for (ti, t) in times.iter().enumerate() {
        w.emit_time_change(*t);
        for (i, sig) in sigs.iter().enumerate() {
            let v = &sig.values[ti];
            if sig.deltas[ti] && ti > 0 {
                let mut glitch = v.clone();
                if glitch[0] == b'0' { glitch[0] = b'1'; } else { glitch[0] = b'0'; }
                w.emit_value_change(handles[i], &glitch);
            }
            w.emit_value_change(handles[i], v);
        }
    }
    w.close().unwrap();
}

fn queries(sigs: &[Sig], times: &[u64]) -> Vec<String> {
    let mut qs: Vec<String> = Vec::new();
    for sig in sigs {
        let n = &sig.name;
        // 取该信号真实出现过的值作为查询常量(含 x/z 位串)
        let mut seen: Vec<String> = Vec::new();
        for v in &sig.values {
            let s = String::from_utf8_lossy(v).to_string();
            if !seen.contains(&s) { seen.push(s); }
        }
        seen.truncate(4);
        for s in &seen {
            if s.len() == 1 && !s.chars().all(|c| c == '0' || c == '1') {
                continue; // 1 位 x/z 的整数比较另测
            }
            if s.chars().all(|c| c == '0' || c == '1') {
                let dec = u128::from_str_radix(s, 2).unwrap_or(0);
                if dec > i64::MAX as u128 { continue; } // 字面量超出 i64: 解析器不接受
                qs.push(format!("(count (= (get \"{}\") {}))", n, dec));
                qs.push(format!("(count (!= (get \"{}\") {}))", n, dec));
                qs.push(format!("(count/step (= (get \"{}\") {}))", n, dec));
                qs.push(format!("(at \"{}\" {})", n, times[times.len() / 2]));
            }
        }
        qs.push(format!("(count (rising \"{}\"))", n));
        qs.push(format!("(count (falling \"{}\"))", n));
        qs.push(format!("(count (changes \"{}\"))", n));
        qs.push(format!("(count/step (changes \"{}\"))", n));
        qs.push(format!("(count (is-x \"{}\"))", n));
        qs.push(format!("(count (is-z \"{}\"))", n));
        qs.push(format!("(length (find (is-x \"{}\")))", n));
    }
    // 混合条件(引擎路径)
    if sigs.len() >= 2 {
        let (a, b) = (&sigs[0].name, &sigs[1].name);
        qs.push(format!("(count (&& (rising \"{}\") (= (get \"{}\") 0)))", a, b));
        qs.push(format!("(count (&& (changes \"{}\") (is-x \"{}\")))", a, b));
        qs.push(format!("(count (|| (rising \"{}\") (falling \"{}\")))", a, b));
        qs.push(format!("(count (&& (not (rising \"{}\")) (= (get \"{}\") 1)))", a, b));
        qs.push(format!("(find (&& (rising \"{}\") (= (get \"{}\") 1)))", a, b));
    }
    qs
}

fn run_cli_env(bin: &str, query: &str, vcd: &std::path::Path, envs: &[(&str, &str)]) -> String {
    let mut cmd = std::process::Command::new(bin);
    cmd.arg(query).arg("-l").arg(vcd);
    for (k, v) in envs { cmd.env(k, v); }
    let out = cmd.output().expect("spawn wal-rust");
    assert!(out.status.success(), "cli 失败 {}: {}", query, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn eval_file(path: &std::path::Path, q: &str) -> Result<Value, String> {
    let mut e = Evaluator::new();
    e.load_trace(&path.to_string_lossy(), "t")?;
    e.eval(q)
}

#[test]
fn fuzz_vcd_fst_equivalence() {
    let n: u64 = std::env::var("WAL_FUZZ_N").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let seed0: u64 = std::env::var("WAL_FUZZ_SEED").ok().and_then(|v| v.parse().ok())
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    let dir = std::env::temp_dir().join(format!("wal_fuzz_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    for it in 0..n {
        let seed = seed0.wrapping_add(it.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut rng = Rng(seed | 1);
        let (sigs, times) = gen_wave(&mut rng);
        let vcd_text = to_vcd(&sigs, &times);
        let vcd_path = dir.join(format!("w{}.vcd", it));
        let fst_path = dir.join(format!("w{}.fst", it));
        std::fs::write(&vcd_path, &vcd_text).unwrap();
        write_fst(&fst_path, &sigs, &times);

        // 快速自检: 两个后端读到的时间轴/信号表一致
        let vt = VcdTrace::load(&vcd_path, "t".to_string()).unwrap();
        assert_eq!(vt.max_index() + 1, times.len(), "seed={:#x} VCD 索引数不符", seed);
        drop(vt);

        for q in queries(&sigs, &times) {
            let a = eval_file(&vcd_path, &q);
            let b = eval_file(&fst_path, &q);
            if a != b {
                if std::env::var_os("WAL_FUZZ_KEEP").is_some() {
                    let _ = std::fs::copy(&vcd_path, ".tools/fuzz_fail.vcd");
                    let _ = std::fs::copy(&fst_path, ".tools/fuzz_fail.fst");
                }
                panic!(
                    "VCD/FST 不一致 seed={:#x} query={}\n  VCD={:?}\n  FST={:?}\n波形:\n{}",
                    seed, q, a, b, vcd_text
                );
            }
        }
        if std::env::var_os("WAL_FUZZ_KEEP").is_none() {
            let _ = std::fs::remove_file(&vcd_path);
            let _ = std::fs::remove_file(&fst_path);
        } else {
            eprintln!("keep: {} / {}", vcd_path.display(), fst_path.display());
        }
    }
    if std::env::var_os("WAL_FUZZ_KEEP").is_none() {
        let _ = std::fs::remove_dir(&dir);
    }
    eprintln!("fuzz_vcd_fst_equivalence: {} 个随机波形 × {} 类查询 全部一致", n, "多");
}

/// 与逐拍 oracle 的一致性(引擎开关在子进程里切换,避免环境变量竞态)
#[test]
fn fuzz_engine_matches_step_oracle() {
    let n: u64 = std::env::var("WAL_FUZZ_N").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
    let seed0: u64 = std::env::var("WAL_FUZZ_SEED").ok().and_then(|v| v.parse().ok())
        .unwrap_or(0x1234_5678_9ABC_DEF1);
    let dir = std::env::temp_dir().join(format!("wal_fuzz_step_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bin = env!("CARGO_BIN_EXE_wal-rust");

    for it in 0..n {
        let seed = seed0.wrapping_add(it.wrapping_mul(0x2545_F491_4F6C_DD1D));
        let mut rng = Rng(seed | 1);
        let (sigs, times) = gen_wave(&mut rng);
        let vcd_path = dir.join(format!("s{}.vcd", it));
        std::fs::write(&vcd_path, to_vcd(&sigs, &times)).unwrap();

        for q in queries(&sigs, &times) {
            if !q.starts_with("(count") && !q.starts_with("(find") { continue; }
            let eng = run_cli_env(bin, &q, &vcd_path, &[]);
            let step = run_cli_env(bin, &q, &vcd_path, &[("WAL_NO_ENGINE", "1")]);
            if eng != step && std::env::var_os("WAL_FUZZ_KEEP").is_some() {
                let _ = std::fs::copy(&vcd_path, ".tools/fuzz_step_fail.vcd");
            }
            assert_eq!(eng, step, "引擎/逐拍不一致 seed={:#x} query={}\n波形:\n{}", seed, q,
                to_vcd(&sigs, &times));
        }
        let _ = std::fs::remove_file(&vcd_path);
    }
    let _ = std::fs::remove_dir(&dir);
}

/// 可选路径必须与基线逐值一致: 列缓存(WAL_COL_CACHE_MB)、跨进程缓存
/// (WAL_CACHE=build 写回 → auto/read 命中)。这些路径默认关闭,固定夹具覆盖
/// 有限,这里用随机波形加闸。
#[test]
fn fuzz_optin_paths_match_baseline() {
    let n: u64 = std::env::var("WAL_FUZZ_N").ok().and_then(|v| v.parse().ok()).unwrap_or(12);
    let seed0: u64 = std::env::var("WAL_FUZZ_SEED").ok().and_then(|v| v.parse().ok())
        .unwrap_or(0x51ED_270B_1111_2222);
    let dir = std::env::temp_dir().join(format!("wal_fuzz_opt_{}", std::process::id()));
    let cdir = dir.join("cache");
    std::fs::create_dir_all(&cdir).unwrap();
    let bin = env!("CARGO_BIN_EXE_wal-rust");
    let cdir_s = cdir.to_string_lossy().to_string();

    for it in 0..n {
        let seed = seed0.wrapping_add(it.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut rng = Rng(seed | 1);
        let (sigs, times) = gen_wave(&mut rng);
        let vcd_path = dir.join(format!("o{}.vcd", it));
        std::fs::write(&vcd_path, to_vcd(&sigs, &times)).unwrap();
        let qs = queries(&sigs, &times);

        // 1) 先用 build 构建跨进程缓存(任意查询都会触发加载+写回)
        let _ = run_cli_env(bin, &qs[0], &vcd_path, &[("WAL_CACHE", "build"), ("WAL_CACHE_DIR", &cdir_s)]);
        assert!(std::fs::read_dir(&cdir).unwrap().count() > 0,
            "build 模式未写出缓存文件 seed={:#x}", seed);

        for q in &qs {
            let base = run_cli_env(bin, q, &vcd_path, &[]);
            let col = run_cli_env(bin, q, &vcd_path, &[("WAL_COL_CACHE_MB", "8")]);
            assert_eq!(base, col, "列缓存路径不一致 seed={:#x} q={}", seed, q);
            let auto = run_cli_env(bin, q, &vcd_path, &[("WAL_CACHE", "auto"), ("WAL_CACHE_DIR", &cdir_s)]);
            let read = run_cli_env(bin, q, &vcd_path, &[("WAL_CACHE", "read"), ("WAL_CACHE_DIR", &cdir_s)]);
            assert_eq!(base, auto, "跨进程缓存(auto 命中)不一致 seed={:#x} q={}", seed, q);
            assert_eq!(base, read, "跨进程缓存(read 命中)不一致 seed={:#x} q={}", seed, q);
            let both = run_cli_env(bin, q, &vcd_path,
                &[("WAL_CACHE", "read"), ("WAL_COL_CACHE_MB", "8"), ("WAL_CACHE_DIR", &cdir_s)]);
            assert_eq!(base, both, "缓存组合路径不一致 seed={:#x} q={}", seed, q);
        }
        let _ = std::fs::remove_file(&vcd_path);
        let _ = std::fs::remove_dir_all(&cdir);
        std::fs::create_dir_all(&cdir).unwrap();
    }
    let _ = std::fs::remove_dir_all(&dir);
}
