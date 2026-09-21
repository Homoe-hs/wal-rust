# tree-sitter-wal — WAL 语法

`grammar.js` 是语法的**唯一来源**;`src/parser.c`(及 `src/tree_sitter/*.h`)是**生成的**,
但它们**必须入库** —— 仓库根的 `Cargo.toml` 依赖本 crate, 而 `build.rs` 只做
`cc` 编译, 不会自己生成语法。少了 `parser.c`, 干净 clone 直接编译失败。

## 改语法

```bash
# 1) 装 CLI(版本要与本仓库生成物一致;当前基线 0.22.6)
curl -sSL -o ts.gz https://github.com/tree-sitter/tree-sitter/releases/download/v0.22.6/tree-sitter-linux-x64.gz
gunzip ts.gz && chmod +x ts
# 2) 改 grammar.js
# 3) 重新生成(会覆盖 src/parser.c、grammar.json、node-types.json、tree_sitter/*.h)
./ts generate
# 4) 提交生成物 + grammar.js(同一提交), 然后跑 make gates
cd .. && cargo build --release && make gates
```

命令细节:

* CLI 需要可写的 `$HOME`/配置目录。受限环境里可以:
  `HOME=$PWD/.tools/home XDG_CONFIG_HOME=$PWD/.tools/config <ts> generate`
* `package.json` 的 `tree-sitter` 字段必须是**数组**形式(`[{"scope": "source.wal", ...}]`),
  写成对象会让 CLI 直接报 "invalid type: map, expected a sequence"。
* `ts generate` 还会顺手生成 `binding.gyp`/`bindings/`/`Makefile`/`.gitignore` 等 Node 包脚手架 ——
  本仓库是"内嵌 grammar 的 Rust crate", 这些都不需要, 生成后删掉即可(根 `.gitignore` 已忽略 `node_modules/`)。
* `token(...)` 里**不能引用规则**(如 `$.base_symbol`), 只能内联正则 —— 所以
  `base_symbol` 的字符类提取成了顶部的 `BASE_SYMBOL_RE` 常量, 两处共用。

## 两个已经踩过的坑(别改回去)

1. **`#name` 必须是单个 token**。写成 `seq("#", $.base_symbol)`(两个 token)时, 词法器在
   `#timeout` 上先匹配到 `#`, 于是输给关键字 `#t` → `#timeout` 被解析成 `#t` + `imeout`
   (`Undefined symbol: imeout`)。单 token 后按"最长匹配"赢过 `#t`, 而单独的 `#t` 仍是布尔。
2. **浮点要带科学计数法**: `float` 现在匹配 `1.5e3` / `1e3` / `1.5E-3`。以前只匹配
   `[0-9]+\.[0-9]+`, 于是 `1e3` 被拆成 int `1` + 符号 `e3`(报 "Undefined symbol: e3")。

改完语法务必跑 `tests/regression_matrix.rs`(里面有专门盯这两个坑的用例)与
`scripts/diff_find.sh` 语义对拍。
