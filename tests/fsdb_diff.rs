//! FSDB(NPI) ↔ VCD **同源差分门**: 同一个设计的两份波形, 逐索引比模型。
//!
//! 需要内网/本地有 Verdi(含 NPI 读库 + 许可)。没设环境变量就自动跳过,
//! 所以 CI/普通 `cargo test` 不受影响:
//!
//! ```bash
//! export VERDI_HOME=/path/to/verdi            # 或 WAL_NPI_LIB=/path/to/libNPI.so
//! export SNPSLMD_LICENSE_FILE=...
//! WAL_FSDB_TEST_FILE=/path/mini.fsdb WAL_FSDB_TEST_VCD=/path/mini.vcd \
//!   cargo test --release --test fsdb_diff -- --nocapture
//! ```
//!
//! 比对项: 时间线(每个索引的原生时间) / 信号集 / 位宽 / 初值(t0 快照) /
//! 每个索引的取值 / 变更点。任一项不同都说明 FSDB 后端与 VCD 后端语义漂移。

use wal_rust::trace::{Trace, TraceContainer};

fn env_or_skip(k: &str) -> Option<String> {
    match std::env::var(k) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => {
            eprintln!(
                "ignored: 未设置 {} —— 本测试需要 Verdi/NPI + 同源波形对;\n  \
                 要真正跑它: WAL_FSDB_TEST_FILE=x.fsdb WAL_FSDB_TEST_VCD=x.vcd \\\n  \
                 cargo test --release --test fsdb_diff -- --include-ignored",
                k
            );
            None
        }
    }
}

/// VCD 里向量信号名带范围后缀(`tb.data [7:0]`), FSDB 不带 —— 配对时统一剥掉。
/// 注意: 必须按**全路径**配而不是叶子名, 否则 `system.i_cpu.CH` 会被
/// VCD 的"子串解析"错配到别的信号上(叶子名带后缀时 leaf 匹配会失效)。
fn norm(name: &str) -> String {
    name.split(" [").next().unwrap_or(name).to_string()
}

/// **相对路径**加载 FSDB 也必须落缓存。
///
/// NPI 沙箱会把 CWD 切到缓存目录下的 `npi/`, 而缓存 key 要 stat 波形文件 ——
/// 若 trace 里存的是用户给的相对路径, 沙箱里 stat 不到 → `cache_path()` 返回
/// None → **一个缓存都不写**: 用户用 `-l design.fsdb`(最常见的写法)时, 每次查询
/// 都要重付一遍全文件扫描(时间线)。本测试在临时目录里 chdir + 相对路径加载,
/// 断言缓存目录非空。(VM 实测: 修复前 files= 空, 修复后 .fnames 落盘。)
#[test]
#[ignore = "需要 Verdi/NPI(libNPI.so)+ 同源 FSDB: 用 --include-ignored 跑(见 scripts/ci.sh gates)"]
fn fsdb_cache_written_for_relative_path() {
    use std::sync::Mutex;
    static CWD_LOCK: Mutex<()> = Mutex::new(());
    let fsdb = match env_or_skip("WAL_FSDB_TEST_FILE") {
        Some(v) => v,
        None => return,
    };
    if !std::path::Path::new(&fsdb).exists() {
        eprintln!("skip: {} 不存在", fsdb);
        return;
    }
    let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("wal_fsdb_rel_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cache = dir.join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::copy(&fsdb, dir.join("rel.fsdb")).unwrap();

    let prev_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&dir).unwrap();
    std::env::set_var("WAL_CACHE", "build");
    std::env::set_var("WAL_CACHE_DIR", &cache);
    std::env::set_var("WAL_CACHE_MIN_MB", "0");
    let res = wal_rust::trace::FsdbTrace::load(std::path::Path::new("rel.fsdb"), "rel".to_string());
    let n = match res {
        Ok(tr) => {
            let _ = tr.max_index(); // 触发全文件扫描(时间线)
            std::fs::read_dir(&cache).map(|d| d.count()).unwrap_or(0)
        }
        Err(e) => {
            std::env::set_current_dir(prev_cwd).unwrap();
            panic!("相对路径加载 FSDB 失败: {}", e);
        }
    };
    std::env::set_current_dir(prev_cwd).unwrap();
    assert!(n > 0, "相对路径加载 FSDB 也必须落缓存(否则每次查询重扫全文件): {:?} 为空", cache);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "需要 Verdi/NPI + 同源 FSDB/VCD 对: 用 --include-ignored 跑(见 scripts/ci.sh gates)"]
fn fsdb_matches_vcd_when_available() {
    let fsdb = match env_or_skip("WAL_FSDB_TEST_FILE") {
        Some(v) => v,
        None => return,
    };
    let vcd = match env_or_skip("WAL_FSDB_TEST_VCD") {
        Some(v) => v,
        None => return,
    };

    let mut cv = TraceContainer::new();
    cv.load(std::path::Path::new(&vcd), "v".to_string())
        .unwrap_or_else(|e| panic!("加载 VCD 失败 {}: {}", vcd, e));
    let mut cf = TraceContainer::new();
    cf.load(std::path::Path::new(&fsdb), "f".to_string())
        .unwrap_or_else(|e| panic!("加载 FSDB 失败 {}: {}", fsdb, e));
    let vid = "v".to_string();
    let fid = "f".to_string();
    let tv = cv.get(&vid).expect("vcd trace");
    let tf = cf.get(&fid).expect("fsdb trace");

    // ---- 时间线 ----
    assert_eq!(
        tv.max_index(),
        tf.max_index(),
        "max_index 不同(VCD {} / FSDB {})",
        tv.max_index(),
        tf.max_index()
    );
    for i in 0..=tv.max_index() {
        assert_eq!(
            tv.timestamp_at(i),
            tf.timestamp_at(i),
            "索引 {} 的原生时间不同",
            i
        );
    }
    assert_eq!(tv.timescale_exp(), tf.timescale_exp(), "timescale 不同");

    // ---- 信号集(剥掉范围后缀后必须一致) ----
    // 例外: **位炸开的向量**。VCS 写 VCD 时会把某些总线拆成 `CH [4] CH [3] …`
    // 这样的单 bit `$var`, 而 NPI 把它们归成一个 `CH [4:0]` 的 5 bit 信号。
    // 两边对同一批信号用不同表示, 无法逐信号对齐 → 这类键单独统计并跳过,
    // 但**必须打印出来**, 不能悄悄掩盖。
    let mut vgroups: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for s in tv.signals() {
        vgroups.entry(norm(&s)).or_default().push(s);
    }
    let bitblasted: std::collections::HashSet<String> = vgroups
        .iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|(k, _)| k.clone())
        .collect();
    let vmap: std::collections::HashMap<String, String> = vgroups
        .iter()
        .filter(|(k, v)| v.len() == 1 && !bitblasted.contains(*k))
        .map(|(k, v)| (k.clone(), v[0].clone()))
        .collect();
    let mut vs: Vec<String> = vgroups
        .keys()
        .filter(|k| !bitblasted.contains(*k))
        .cloned()
        .collect();
    let mut fs: Vec<String> = tf
        .signals()
        .iter()
        .map(|s| norm(s))
        .filter(|k| !bitblasted.contains(k))
        .collect();
    vs.sort();
    fs.sort();
    assert_eq!(vs, fs, "信号集不同(VCD {} 个 / FSDB {} 个, 位炸开跳过 {} 组)",
               vs.len(), fs.len(), bitblasted.len());
    if !bitblasted.is_empty() {
        let mut names: Vec<&String> = bitblasted.iter().collect();
        names.sort();
        println!(
            "跳过 {} 组位炸开向量(VCD 拆 bit / NPI 归总线): {:?}",
            names.len(),
            names.iter().take(8).collect::<Vec<_>>()
        );
    }

    // ---- 逐信号: 位宽 / 初值 / 每个索引的取值 / 变更点 ----
    // 收集**全部**差异再一次报出来(只看第一条会掩盖"到底是哪一类漂移")。
    let mut diffs: Vec<String> = Vec::new();
    let mut checked = 0usize;
    let mut with_init = 0usize;
    for fname in tf.signals() {
        let key = norm(&fname);
        if bitblasted.contains(&key) {
            continue; // 位炸开向量: 两边表示不同, 上面已单独统计
        }
        let Some(vname) = vmap.get(&key).cloned() else {
            diffs.push(format!("{}: 在 VCD 里找不到对应信号", fname));
            continue;
        };
        if tv.signal_width(&vname).unwrap() != tf.signal_width(&fname).unwrap() {
            diffs.push(format!(
                "{}: 位宽 {} vs {}",
                fname,
                tv.signal_width(&vname).unwrap(),
                tf.signal_width(&fname).unwrap()
            ));
        }
        // 比的是"**确定**初值"而不是原始快照: VCD 只给 `$dumpvars` 列出的信号写
        // 条目, FSDB 写者给每个信号都写一条 t=0(没初始化的是全 x)。原始快照
        // 因此会差在"x 有没有条目"上, 而这对三种读路径(initial / at t0 / 索引 0
        // 前驱)都是同一个 x, 所以用 defined_initial_value 作语义判据。
        let (iv, if_) = (tv.defined_initial_value(&vname), tf.defined_initial_value(&fname));
        if iv.is_some() {
            with_init += 1;
        }
        // 注: 曾经这里允许一类"VCD 无初值 / FSDB 有初值"的差异, 理由是"两代写者采样
        // 时机不同"。差分证明那是**VCD 后端的别名 bug**(同一 idcode 在多个 scope 被
        // 引用时, dumpprobes 初值只写在代表信号上) —— 修掉之后两边初值全等, 这条
        // 容忍分支随之删除, 恢复严格判据。
        if iv != if_ {
            diffs.push(format!("{}: 确定初值 {:?} vs {:?}", fname, iv, if_));
        }
        for i in 0..=tv.max_index() {
            let (a, b) = (tv.signal_value(&vname, i), tf.signal_value(&fname, i));
            if a != b {
                diffs.push(format!("{}: 索引 {} 取值 {:?} vs {:?}", fname, i, a, b));
                break; // 一个信号只报第一处取值差, 免得刷屏
            }
        }
        if tv.change_points(&vname).unwrap() != tf.change_points(&fname).unwrap() {
            diffs.push(format!("{}: 变更点不同", fname));
        }
        checked += 1;
    }
    if !diffs.is_empty() {
        eprintln!("共 {} 类差异 (检查 {} 个信号, VCD 侧 {} 个有初值):", diffs.len(), checked, with_init);
        for d in diffs.iter().take(20) {
            eprintln!("  {}", d);
        }
    }
    assert!(diffs.is_empty(), "FSDB 与 VCD 模型不一致: {} 处", diffs.len());
    println!(
        "FSDB↔VCD 差分通过: {} 个信号 × {} 个索引, 其中 {} 个有确定初值",
        checked,
        tv.max_index() + 1,
        with_init
    );
}

/// 时间线落盘缓存的编解码往返(纯函数, 不需要 Verdi)。
/// 缓存是"索引空间"的唯一跨进程载体, 解错一个字节 = 所有 find/电平查询错位。
#[test]
fn timeline_cache_codec_roundtrip() {
    let cases: Vec<Vec<u64>> = vec![
        vec![],
        vec![0],
        vec![5, 10, 15, 25, 35, 45, 55],
        (0..5000u64).map(|i| i * 1000).collect(),
        vec![u64::MAX - 1, u64::MAX],
    ];
    for times in cases {
        let blob = wal_rust::trace::fsdb_test_api::encode(&times, Some(7));
        let (got, first) = wal_rust::trace::fsdb_test_api::decode(&blob, fp_of(&blob))
            .expect("decode 应当成功");
        assert_eq!(got, times, "时间点往返不一致");
        assert_eq!(first, Some(7));
    }
    // 指纹不符 / 截断 / magic 错 → 必须拒绝(不能把别人的时间线读进来)
    let blob = wal_rust::trace::fsdb_test_api::encode(&[1, 2, 3], None);
    assert!(wal_rust::trace::fsdb_test_api::decode(&blob, 0xdead_beef).is_none());
    assert!(wal_rust::trace::fsdb_test_api::decode(&blob[..blob.len() - 1], fp_of(&blob)).is_none());
    assert!(wal_rust::trace::fsdb_test_api::decode(b"XXXXXXXX", fp_of(&blob)).is_none());
    let empty_first = wal_rust::trace::fsdb_test_api::encode(&[], None);
    assert_eq!(wal_rust::trace::fsdb_test_api::decode(&empty_first, fp_of(&empty_first)).unwrap().1, None);
}

/// 从 blob 里取指纹(测试辅助: 免得再算一遍文件指纹)
fn fp_of(blob: &[u8]) -> u64 {
    u64::from_le_bytes(blob[8..16].try_into().unwrap())
}

/// 名字树缓存的编解码往返(纯函数): 名字/位宽/scope 三者的**顺序与内容**都不能错,
/// 否则恢复出来的句柄会挂到别的信号上(查询结果整体错位)。
#[test]
fn tree_cache_codec_roundtrip() {
    let names: Vec<String> = vec![
        "tb.clk".into(),
        "tb.u_dcache.g_inst[0].u_leaf.cnt".into(),
        "中文/带空格 名字".into(),
        String::new(),
    ];
    let widths = vec![1usize, 16, 8, 0];
    let scopes = vec!["tb".into(), "tb.u_dcache".into()];
    let blob = wal_rust::trace::fsdb_test_api::encode_tree(&names, &widths, &scopes);
    let got = wal_rust::trace::fsdb_test_api::decode_tree(&blob, fp_of(&blob)).expect("decode");
    assert_eq!(got.0, names);
    assert_eq!(got.1, widths);
    assert_eq!(got.2, scopes);
    // 指纹不符 / 截断 → 拒绝
    assert!(wal_rust::trace::fsdb_test_api::decode_tree(&blob, 0x1234).is_none());
    assert!(wal_rust::trace::fsdb_test_api::decode_tree(&blob[..blob.len() - 1], fp_of(&blob)).is_none());
}

/// 并行构建时间线的**分片正确性**(纯函数, 不需要 Verdi):
/// 每个 worker 拿到的信号必须两两不重叠、合起来正好覆盖全部 —— 漏一个信号就会
/// 少一段时间点(索引空间整体错位), 重一个则白算。
#[test]
fn timeline_round_robin_partition_covers_all() {
    for n in [0usize, 1, 2, 3, 7, 8, 1000, 60005] {
        for k in [1usize, 2, 3, 4, 8, 16, 64] {
            let mut all: Vec<usize> = Vec::new();
            for off in 0..k {
                all.extend(wal_rust::trace::fsdb_test_api::round_robin_slice(n, off, k));
            }
            all.sort_unstable();
            let expect: Vec<usize> = (0..n).collect();
            assert_eq!(all, expect, "n={} k={} 分片不覆盖/有重复", n, k);
        }
    }
}

/// 旁挂列缓存(跨进程 `.fcol`)必须**命中 == 未命中**给出同一个答案。
///
/// 事故背景: FSDB 拿某信号的变更列只能重走一遍 NPI 变更流(`npiFsdbTimeBasedVcIter`),
/// 于是同一个查询每次进程启动都付一遍全量扫描 —— 188 万信号 / 几千万时间戳上就是
/// "每次都慢"。加了 `.fcol`(列 + t0 初值)之后, **第二次运行**必须与冷扫逐字节同答,
/// 且缓存文件真的要落在缓存目录里。
///
/// 需要:
/// ```bash
/// VERDI_HOME=… SNPSLMD_LICENSE_FILE=…
/// WAL_FSDB_TEST_FILE=x.fsdb WAL_FSDB_TEST_SIG=<信号全名> \
///   cargo test --release --test fsdb_diff -- --include-ignored fsdb_col_cache
/// ```
#[test]
#[ignore = "需要 Verdi/NPI + WAL_FSDB_TEST_SIG: 用 --include-ignored 跑"]
fn fsdb_col_cache_hit_matches_cold() {
    let fsdb = match env_or_skip("WAL_FSDB_TEST_FILE") {
        Some(v) => v,
        None => return,
    };
    let sig = match env_or_skip("WAL_FSDB_TEST_SIG") {
        Some(v) => v,
        None => return,
    };
    if !std::path::Path::new(&fsdb).exists() {
        eprintln!("skip: {} 不存在", fsdb);
        return;
    }
    let bin = env!("CARGO_BIN_EXE_wal-rust");
    // 缓存目录 = 执行目录(用户明确要求), 所以用两个临时 CWD 隔离"冷"与"暖"
    let base = std::env::temp_dir().join(format!("wal-fcol-{}", std::process::id()));
    let cold = base.join("cold");
    let warm = base.join("warm");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&cold).unwrap();
    std::fs::create_dir_all(&warm).unwrap();

    let queries = [
        format!("(count (= (get \"{}\") 1))", sig),
        format!("(count (rising \"{}\"))", sig),
        format!("(at \"{}\" 3)", sig),
    ];
    let run = |dir: &std::path::Path, cache: &str, q: &str| -> String {
        // `WAL_CACHE_DIR` 必须**显式**指到本测试的目录: 环境里若已有它(CI 会设),
        // 子进程会把缓存写到别处, 于是"缓存到底落在哪"就不是这个测试能控制的了。
        let out = std::process::Command::new(bin)
            .arg(q)
            .arg("-l")
            .arg(&fsdb)
            .current_dir(dir)
            .env("WAL_CACHE", cache)
            .env("WAL_CACHE_MIN_MB", "0")
            .env("WAL_CACHE_DIR", dir.join(".wal-rust-cache"))
            .output()
            .expect("spawn wal-rust");
        assert!(
            out.status.success(),
            "查询失败 [{} / {}]: {}",
            q,
            cache,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    for q in &queries {
        let a = run(&cold, "off", q); // 完全不用缓存
        let b = run(&warm, "build", q); // 冷建 + 落盘
        let c = run(&warm, "build", q); // 命中缓存
        assert_eq!(a, b, "冷扫与建缓存结果不一致: {}", q);
        assert_eq!(a, c, "缓存命中与冷扫结果不一致: {}", q);
    }

    // 缓存目录里必须有列文件(否则这个测试什么也没证明)。
    // 注意缓存落在 **CWD 下的 `.wal-rust-cache/`**(用户要求"缓存在执行命令的路径下"),
    // 不在工作目录顶层 —— 这里要往里看一层。
    let cache_root = warm.join(".wal-rust-cache");
    let mut n_col = 0usize;
    if let Ok(entries) = std::fs::read_dir(&cache_root) {
        for e in entries {
            let p = e.unwrap().path();
            if p.is_dir() && p.file_name().unwrap().to_string_lossy().ends_with(".fcol") {
                n_col += std::fs::read_dir(&p).unwrap().count();
            }
        }
    }
    let _ = std::fs::remove_dir_all(&base);
    assert!(n_col > 0, "没有写出任何 .fcol 列缓存 —— 缓存路径或写入门槛有问题");
}

/// map/reduce 预计算的时间线缓存必须与"单进程自己写出来的 `.ftl`"**逐字节一致**,
/// 且与分片顺序/切法无关 —— 这是集群(LSF)与多进程并行的正确性契约:
/// 分片阶段只产"升序去重的时间序列", 汇合阶段走与单进程完全相同的编码 + 指纹。
///
/// (不需要 Verdi: 这里只碰分片文件的编解码与缓存落盘。)
#[test]
fn timeline_map_reduce_matches_single_process_encoding() {
    use std::path::PathBuf;
    let base = std::env::temp_dir().join(format!("wal-tl-mr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    // 假波形: merge 只读它的身份(stat)与首尾 64KB 指纹, 不需要是真 FSDB
    let wave = base.join("fake.fsdb");
    std::fs::write(&wave, vec![7u8; 4096]).unwrap();

    // 三个分片: 片内升序去重, 片间有重叠(轮转分片的真实形态)
    let sets: [&[u64]; 3] = [&[3, 9, 40], &[9, 11], &[1, 40, 41]];
    let mut parts: Vec<PathBuf> = Vec::new();
    for (k, s) in sets.iter().enumerate() {
        let p = base.join(format!("tl.{}.part", k));
        wal_rust::trace::fsdb_test_api::write_times_file(&p, s).unwrap();
        parts.push(p);
    }
    let cache = base.join("cache");
    let (n, ftl) =
        wal_rust::trace::fsdb_test_api::merge_timeline_parts(&wave, &parts, Some(&cache)).unwrap();
    assert_eq!(n, 6, "1,3,9,11,40,41 去重后应有 6 个时间点");

    let want: Vec<u64> = vec![1, 3, 9, 11, 40, 41];
    let fp = wal_rust::trace::fsdb_test_api::wave_fingerprint(&wave);
    let bytes = std::fs::read(&ftl).unwrap();
    let (got, first) = wal_rust::trace::fsdb_test_api::decode(&bytes, fp).expect("缓存可解码");
    assert_eq!(got, want);
    assert_eq!(first, Some(1), "first_change = 时间线首点");
    // 与单进程编码逐字节一致(只有指纹不同来源, 值必须相同)
    let expect = wal_rust::trace::fsdb_test_api::encode(&want, Some(1));
    assert_eq!(bytes[0..8], expect[0..8], "magic 必须一致");
    assert_eq!(bytes[16..], expect[16..], "除指纹外整段字节必须一致");

    // 分片顺序无关: 倒序归并 → 同一份缓存内容
    let mut rev = parts.clone();
    rev.reverse();
    let cache2 = base.join("cache2");
    let (n2, ftl2) =
        wal_rust::trace::fsdb_test_api::merge_timeline_parts(&wave, &rev, Some(&cache2)).unwrap();
    assert_eq!(n2, 6);
    assert_eq!(std::fs::read(&ftl2).unwrap(), bytes, "归并顺序不得影响缓存字节");

    // 坏输入必须报错而不是写出半份缓存
    assert!(wal_rust::trace::fsdb_test_api::merge_timeline_parts(&wave, &[], Some(&cache)).is_err());
    let bad = base.join("tl.bad.part");
    std::fs::write(&bad, [0x80u8, 0x80]).unwrap(); // 截断的 varint
    assert!(wal_rust::trace::fsdb_test_api::merge_timeline_parts(&wave, &[bad], Some(&cache)).is_err());
    let empty = base.join("tl.empty.part");
    wal_rust::trace::fsdb_test_api::write_times_file(&empty, &[]).unwrap();
    assert!(wal_rust::trace::fsdb_test_api::merge_timeline_parts(&wave, &[empty], Some(&cache)).is_err());
    let _ = std::fs::remove_dir_all(&base);
}

/// `WAL_FSDB_TL_JOBS` 解析: 默认 1(每 worker 一个 Verdi 许可, 不能偷偷并行),
/// `auto` = min(核数, 8), 非法值退回 1。
#[test]
fn timeline_jobs_parsing_is_conservative() {
    use wal_rust::trace::fsdb_test_api::parse_timeline_jobs as p;
    assert_eq!(p("", 32), 1, "未设置 → 不并行");
    assert_eq!(p("1", 32), 1);
    assert_eq!(p("4", 32), 4);
    assert_eq!(p(" 4 ", 32), 4);
    assert_eq!(p("auto", 32), 8, "auto 上限 8(许可与内存都有限)");
    assert_eq!(p("auto", 4), 4);
    assert_eq!(p("auto", 1), 1);
    assert_eq!(p("nonsense", 32), 1, "非法值不能变成 0 或 panic");
    assert_eq!(p("0", 32), 0, "0 表示显式关闭并行(由调用方 clamp)");
}

/// 并行度策略: 默认 auto(`-j` 未给)但要跳过小波形, `-j 1` 强制单进程,
/// 显式数字一律照办;批处理(`bsub -n 8`)的 slot 数作为可用额度。
#[test]
fn timeline_jobs_decision_is_auto_but_size_gated() {
    use wal_rust::trace::fsdb_test_api::decide_jobs as d;
    const MB: u64 = 1024 * 1024;
    let big = 256 * MB;
    let small = 1 * MB;
    let min = 32 * MB;

    // 什么都没给 → 自动: 小波形 1 路, 大波形按额度(封顶 8)
    assert_eq!(d(None, 32, small, 100, min), (1, true));
    assert_eq!(d(None, 32, big, 100, min), (8, true));
    assert_eq!(d(None, 4, big, 100, min), (4, true));
    // 信号特别多(188 万那种)即使文件不大也值得并行
    assert_eq!(d(None, 4, small, 20000, min), (4, true));

    // `-j 1` 强制单进程(许可紧张时用);`-j 8` 一律照办(不看大小)
    assert_eq!(d(Some("1"), 32, big, 100, min), (1, false));
    assert_eq!(d(Some("8"), 32, small, 100, min), (8, false));
    // `-j auto` 仍跳过小波形
    assert_eq!(d(Some("auto"), 32, small, 100, min), (1, true));
    assert_eq!(d(Some("auto"), 32, big, 100, min), (8, true));
    assert_eq!(d(Some("  "), 32, big, 100, min), (8, true), "空串 = 没设置");
    assert_eq!(d(Some("junk"), 32, big, 100, min), (1, false), "非法值退化成 1");
    // 额度上限: 64 核的机器 auto 也只开 8(许可与收益的权衡)
    assert_eq!(d(Some("auto"), 64, big, 100, min), (8, true));
    assert_eq!(d(Some("16"), 64, big, 100, min), (16, false), "显式数字不封顶");
}
