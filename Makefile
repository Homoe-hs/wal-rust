# ============================================================================
# wal-rust 常用任务入口 —— `make help` 看全部。
#
# 设计原则: Makefile 只做**转发**, 真正的逻辑写在 scripts/ 里
# (脚本能单独跑、能在 CI 里跑, 也能被 AI/同事直接读)。这样避免"Makefile 里藏逻辑"。
# ============================================================================
SHELL := /usr/bin/env bash
.DEFAULT_GOAL := help
CARGO_HOME ?= $(CURDIR)/.cargo-home
XDG_CACHE_HOME ?= $(CURDIR)/.tools/cache
export CARGO_HOME XDG_CACHE_HOME

.PHONY: help ci ci-fast ci-full fmt fmt-check lint build test gates test-fsdb docs-check docs-serve perf bench-names bench-fsdb fsdb-prewarm quickcheck release release-dry clean dist clean-logs

help: ## 显示所有可用任务
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  \033[1m%-16s\033[0m %s\n", $$1, $$2}'

# --- CI ---------------------------------------------------------------------
ci: ## 本地 CI: 默认全套必过阶段(提交/发版前跑这个)
	./scripts/ci.sh

ci-fast: ## 本地 CI: 最快一圈(改完随手跑)
	./scripts/ci.sh --fast

ci-full: ## 本地 CI: 含大规模 fuzz 与打包冒烟(发版前跑)
	./scripts/ci.sh --full

fmt: ## 直接格式化(只格式化改动文件: make fmt FILES="src/a.rs")
	cargo fmt $(if $(FILES),-- $(FILES))

fmt-check: ## 检查格式(不修改)
	./scripts/ci.sh --only fmt

lint: ## clippy(提示级, 见 rustfmt.toml 注释里的收敛计划)
	./scripts/ci.sh --only clippy

build: ## release 构建并打印版本
	./scripts/ci.sh --only build

test: ## 全部测试(release)
	./scripts/ci.sh --only test

gates: ## 只跑语义冻结闸(矩阵 + 随机差分 + 旧版语义门)
	./scripts/ci.sh --only gates

test-fsdb: ## FSDB↔VCD 同源差分门(需 VERDI_HOME / WAL_FSDB_TEST_FILE / WAL_FSDB_TEST_VCD)
	@: $${WAL_FSDB_TEST_FILE:?请先 export WAL_FSDB_TEST_FILE=design.fsdb}
	@: $${WAL_FSDB_TEST_VCD:?请先 export WAL_FSDB_TEST_VCD=design.vcd}
	WAL_CACHE=off cargo test --release --test fsdb_diff -- --nocapture

# --- 文档 -------------------------------------------------------------------
docs-check: ## 文档一致性: 断链、索引完整性、声称的数字
	python3 scripts/check_docs.py

# --- 性能 -------------------------------------------------------------------
bench-names: ## 名字解析/裸符号求值基准(默认 200k 信号 × 2000 时间戳)
	./scripts/bench_name_resolution.sh $(N) $(T)

bench-fsdb: ## FSDB 查询基准(冷建时间线/暖查询): make bench-fsdb FSDB=a.fsdb [SIG=tb.clk]
	@: $${FSDB:?用法: make bench-fsdb FSDB=a.fsdb [SIG=tb.clk]}
	./scripts/bench_fsdb.sh "$$FSDB" "$(SIG)"

fsdb-prewarm: ## 并行预计算 FSDB 时间线缓存: make fsdb-prewarm FSDB=a.fsdb [SHARDS=8] [QUEUE=q] [LOCAL=1]
	@: $${FSDB:?用法: make fsdb-prewarm FSDB=a.fsdb [SHARDS=8] [QUEUE=队列] [LOCAL=1]}
	./scripts/lsf_fsdb_prewarm.sh "$$FSDB" $(or $(SHARDS),8) $(if $(QUEUE),--queue $(QUEUE)) $(if $(LOCAL),--local)

quickcheck: ## FSDB↔VCD 一分钟一致性诊断: make quickcheck FSDB=a.fsdb VCD=a.vcd [SIG=tb.clk]
	@: $${FSDB:?用法: make quickcheck FSDB=a.fsdb VCD=a.vcd}
	@: $${VCD:?用法: make quickcheck FSDB=a.fsdb VCD=a.vcd}
	./scripts/fsdb_vcd_quickcheck.sh "$$FSDB" "$$VCD" $(SIG)

perf: ## 性能冒烟(需要 bench/data 里有大样本)
	./scripts/ci.sh --only perf

# --- 发布 -------------------------------------------------------------------
dist: ## 构建 glibc2.17 发布二进制 → target/x86_64-unknown-linux-gnu/release/wal-rust(需要 zig)
	./scripts/ci.sh --only package

release: ## 发版: make release VERSION=0.15.0(默认用 Cargo.toml 里的版本)
	./scripts/release.sh $(VERSION)

release-dry: ## 发版演练(不推送/不创建 release)
	./scripts/release.sh --dry-run $(VERSION)

# --- 清理 -------------------------------------------------------------------
clean-logs: ## 清掉 CI 日志与本地缓存
	rm -rf .tools/ci-logs .tools/ci-cache

clean: ## cargo clean + 清日志(不动样本与缓存)
	cargo clean
	rm -rf .tools/ci-logs
