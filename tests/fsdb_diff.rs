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
            eprintln!("skip: 未设置 {} (本测试需要 Verdi/NPI + 同源波形对)", k);
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

#[test]
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
