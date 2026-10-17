//! 开发工具: 打印已注册算子的规范名字(给 bug-hunt 测试矩阵用; 不参与发布)。
fn main() {
    let mut d = wal_rust::wal::eval::Dispatcher::new();
    wal_rust::wal::builtins::register_all(&mut d);
    let mut names: Vec<String> = d.operators.keys().map(|o| o.as_str().to_string()).collect();
    names.sort();
    names.dedup();
    for n in &names {
        println!("{}", n);
    }
    eprintln!("total={}", names.len());
}
