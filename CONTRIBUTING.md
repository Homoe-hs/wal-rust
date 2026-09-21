# 参与开发

> 面向**人**的流程说明。给 AI 编码代理的项目结构与约定在 [`AGENTS.md`](AGENTS.md),
> 两者互补: 这里讲"怎么做、怎么验、怎么发", AGENTS.md 讲"代码在哪、哪些约束不能破"。

## 0. 一分钟上手

```bash
git clone https://github.com/Homoe-hs/wal-rust && cd wal-rust
make ci-fast                 # 最快一圈: 环境自检 → 文档检查 → 构建 → 全部测试
cargo build --release
target/release/wal-rust '(count (rising "top.clk"))' -l design.vcd
```

工具链: `rust-toolchain.toml` 固定 stable(含 rustfmt/clippy); MSRV 见 `Cargo.toml` 的
`rust-version`(当前 1.90)。本仓库**自带**工具链缓存目录(`.cargo-home/`、`.tools/`),
`make` 目标会自动设置 `CARGO_HOME`/`XDG_CACHE_HOME`, 你的 `~/.cargo` 不会被污染。

## 1. 本地 CI(最重要的一条)

所有门都在 `scripts/ci.sh`, 本机与 GitHub Actions 跑的是**同一份**脚本:

```bash
make ci            # 全套必过阶段(提交前/发版前)
make ci-fast       # 最快一圈(改完随手跑)
make ci-full       # 追加: 大规模随机差分 + glibc2.17 打包冒烟
make ci --only test,gates        # 只跑某几个阶段
./scripts/ci.sh --list           # 看阶段清单
```

| 阶段 | 作用 | 失败意味着 |
|---|---|---|
| `preflight` | rustc/cargo/必备文件/仓库卫生(大文件、编译产物入库) | 仓库结构被破坏 |
| `docs` | 文档断链、`docs/README.md` 索引完整性、文档里声称的数字 | 文档与代码不一致 |
| `fmt` | `cargo fmt --check` | 格式漂移(见下"格式收敛") |
| `clippy` | `cargo clippy --all-targets` | 潜在 bug/坏味道 |
| `build` | `cargo build --release` | 编译失败 |
| `test` | `cargo test --release` | 单元/集成测试失败 |
| `gates` | 语义冻结闸(见下) | **语义漂移, 最高优先级** |
| `package` | glibc 2.17 交叉构建 + 冒烟查询 | 发布物坏了 |
| `perf` / `fuzz` | 性能冒烟(**名字解析基准**: 防"每次求值克隆整张名字表"回归)/ 大规模随机差分 | 需要人看(`--full` 才跑) |

日志在 `.tools/ci-logs/<阶段>.log`; 失败会打印尾部 25 行。

### 格式收敛(诚实说明)

仓库历史提交没有跑过 `cargo fmt`, 全仓约有 2000 处差异。所以 `fmt`/`clippy` 两个阶段
**目前是提示级**(`WAL_CI_FMT_STRICT=1` 可让它们变硬门):

* 你**新增/修改的文件必须** `cargo fmt`(评审时会被要求);
* 全仓一次性收敛安排在下一个大版本, 用**单独一个纯格式化 commit**(不要混在功能改动里);
* 收敛完成后把 CI 的严格开关打开(见 `.github/workflows/ci.yml` 顶部注释)。

### 语义冻结闸(`make gates`)

改任何解析/查询路径都必须过这几关, 它们是**独立对拍**而不是"自己验自己":

| 门 | 内容 |
|---|---|
| `tests/regression_matrix.rs` | 51 项历史 bug 与语义冻结用例; 子进程里用 `WAL_NO_ENGINE=1` 跑纯逐拍做 oracle |
| `tests/fuzz_vcd_fst_diff.rs` | 随机波形: VCD↔FST 等价 + 统一引擎↔逐拍等价 |
| `scripts/diff_find.sh` | 与上一版二进制 `.tools/wal-rust.old` 的大批量查询对拍 |
| `tests/fsdb_diff.rs` | FSDB↔VCD 同源差分(需 Verdi/NPI + 一对同源波形, 否则自动跳过) |

需要 Verdi 的两项这样跑:

```bash
export VERDI_HOME=/path/to/verdi            # 或 WAL_NPI_LIB=/path/to/libNPI.so
export SNPSLMD_LICENSE_FILE=27051@host
WAL_FSDB_TEST_FILE=design.fsdb WAL_FSDB_TEST_VCD=design.vcd make test-fsdb
```

## 2. 提交规范

* **Conventional Commits**: `type(scope): 摘要` —— 类型用
  `feat` / `fix` / `perf` / `docs` / `refactor` / `test` / `chore`;
  摘要写"用户能感知的变化", 不要写"改了点代码"。
* **一个提交一件事**: 格式化、改名、逻辑改动不要混在一起(评审与回滚都会受益)。
* **提交信息里不要出现内部环境信息**(内网主机名、路径、许可服务器等)。
* 提交前跑 `make ci-fast`; 涉及语义路径的改动必须 `make gates`。
* 版本号由 `.githooks/pre-commit` 自动递增补丁位; 要发某个特定版本时, 先手动改
  `Cargo.toml` 并 `git add Cargo.toml`(钩子检测到已暂存就不再加)。首次克隆后启用钩子:

```bash
git config core.hooksPath .githooks
```

* 改动用户可见行为 → **必须**在 `CHANGELOG.md` 的 `## [未发布]` 下写一行(分类见文件头)。

## 3. 文档规范

* 结构与约定见 [`docs/README.md`](docs/README.md): 每篇文档头部要有状态横幅
  (✅ 现行 / 🟡 计划或草案 / 📦 复盘 / 🗄 历史), 新增文档要登记进索引。
* 历史文档不追改, 由 CI 降级为提示; 不要把"当时的事实"改成现值。
* 语义类问题只有两个权威来源: `docs/4-state-semantics.md`(四值)与
  `docs/query-engine-design.md` §1(查询语义)。与它们冲突的文档要改。
* `python3 scripts/check_docs.py` 可单独跑, 支持 `--warn-only`。

## 4. 发版流程

```bash
# 1) 写 CHANGELOG: 把「未发布」内容整理到 ## [x.y.z] - YYYY-MM-DD(scripts/release.sh 会检查)
# 2) 演练(检查工作区/tag/CHANGELOG, 跑 CI 与交叉构建, 但不推送)
make release-dry VERSION=0.15.0
# 3) 正式发版: 改版本号 → 提交 → 跑 CI → zigbuild → 推送 → 创建 GitHub release
make release VERSION=0.15.0
```

* 发布物: `target/x86_64-unknown-linux-gnu.2.17/release/wal-rust`(**要求 glibc ≥ 2.17**,
  兼容 CentOS 7 / Ubuntu 16.04+)。`make dist` 只构建不发布。
* 推 tag(`v*`)会触发 `.github/workflows/release.yml`: 校验 tag 与 `Cargo.toml` 一致、
  CHANGELOG 有条目, 构建后创建/更新 release(幂等)。
* 交叉编译依赖 `cargo-zigbuild` + zig 0.13: 见 `scripts/install.sh`。

## 5. 报告问题

* Bug: 用 issue 模板, 附**最小复现**(波形片段优先, 大文件请给生成脚本)、
  `wal-rust --version`、`uname -m` 与 glibc 版本(`ldd --version | head -1`)。
* 性能问题: 附样本规模、冷/热缓存、`WAL_CACHE=off` 对照, 以及 `docs/fsdb-npi.md` 里的
  相关数字(便于判断是回归还是本来的量级)。
* 安全问题: 不要开公开 issue, 见 [`SECURITY.md`](SECURITY.md)。

## 6. 许可证与署名

双许可 **MIT OR Apache-2.0**(见 `LICENSE-MIT`、`LICENSE-APACHE`)。提交补丁即表示同意
以同样条款分发你的贡献。
