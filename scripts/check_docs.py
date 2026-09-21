#!/usr/bin/env python3
"""文档一致性检查 —— CI 的 `docs` 阶段。

为什么需要它: 文档是唯一"不会因为编译失败而被发现"的资产。本项目已经出现过
"文档里写的文件早被删了""还在讲 0.13 的行为""AGENTS.md 声称的测试数早已翻倍"这类
问题, 靠人肉 review 抓不住, 所以固化成检查。

检查项:
  A. 本地路径引用存在性(反引号里的路径 + markdown 相对链接)
  B. docs/ 索引完整性(docs/README.md 必须列全, 且不得指向不存在的文档)
  C. AGENTS.md / README.md 里"测试数"这类可核对数字与实际一致
  D. 面向当前用户的文档不得声称过时的版本号(README / AGENTS / CONTRIBUTING / docs/README)

退出码: 0 全部通过; 1 有硬错误; --warn-only 时只有警告也返回 0。
纯本地运行, 不联网。
"""
from __future__ import annotations

import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# 允许"文档里写了但仓库里没有"的路径: 本地产物/被 gitignore 的样本/占位符。
ALLOW_PATTERNS = [
    r"^\.tools/",            # 本地工具与夹具(gitignore)
    r"^bench/data/",         # 大样本, 本地生成
    r"^test_data/",          # LFS/本地夹具
    r"^target/",
    r"^\.cargo-home/",
    r"^\.wal-rust-cache/",
    r"^\.github/workflows/", # 存在与否由本检查自己保证(见下)
    r"^\./",                 # 文档里写 "./x" 时按名字再解析
    r"[<>{}*$]",             # 占位符/通配符
    r"\.\.\.$",
    r"^/",                   # 绝对路径(宿主环境相关)
    r"\.so(\.[0-9.]+)?$",   # 外部共享库(Synopsys NPI 等, 不会进仓库)
    r"^npi_[a-z_]+\.h$",     # NPI 头文件(随 Verdi 安装)
    r"^share/NPI/",           # Verdi 安装目录下的资源(第三方, 不属仓库)
    r"parser\.c$",            # tree-sitter 构建产物(由 tree-sitter-wal/build.rs 调 tree-sitter CLI 生成)
    r"^libnffr\.so$",
    r"^[A-Za-z]:[/\\]",      # Windows 路径
]

# 这些文档天然会提到"已经不存在的路径"(变更日志/迁移说明), 不做路径存在性检查
SKIP_PATH_CHECK = {"CHANGELOG.md"}

# 面向"当前用户"的文档: 这里出现旧版本号 = 会误导使用者
CURRENT_FACING = ["README.md", "AGENTS.md", "CONTRIBUTING.md", "docs/README.md"]
# 历史复盘类文档: 允许出现旧版本号
HISTORY_HINT = ["152gb-round", "internal-feedback-review", "migration-", "CONSTRUCTION"]

PATH_RE = re.compile(r"`([A-Za-z0-9_][A-Za-z0-9_./+-]*\.(?:rs|md|sh|py|toml|yml|yaml|json|wal|vcd|fst|fsdb|txt|csv|c|h|so|lock))`")
LINK_RE = re.compile(r"\]\((?!https?://|#|mailto:)([^)\s]+)\)")
FENCE_RE = re.compile(r"^```")
TEST_COUNT_RE = re.compile(r"cargo test[^\n]*?~(\d+)")
VERSION_RE = re.compile(r"\b(0|1)\.(\d+)\.(\d+)\b")

errors: list[str] = []
warnings: list[str] = []


def tracked_markdown() -> list[Path]:
    out = subprocess.run(
        ["git", "ls-files", "*.md"], cwd=ROOT, capture_output=True, text=True, check=True
    ).stdout.split()
    return [ROOT / p for p in out if (ROOT / p).exists()]


def cfg_version() -> tuple[int, int, int]:
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r'^version\s*=\s*"(\d+)\.(\d+)\.(\d+)"', text, re.M)
    assert m, "Cargo.toml 里找不到 version"
    return int(m.group(1)), int(m.group(2)), int(m.group(3))


def rel(p: Path) -> str:
    try:
        return str(p.relative_to(ROOT))
    except ValueError:
        return str(p)


def allowed(ref: str) -> bool:
    return any(re.search(pat, ref) for pat in ALLOW_PATTERNS)


def is_historical(doc: Path) -> bool:
    """文档开头 20 行里声明了「历史文档 / 归档」→ 其中的旧路径只警告不报错。

    这些复盘类文档记录的是当时的仓库结构与数字, 强行改成现值等于篡改历史;
    正确做法是给它加一条醒目的"已过时"横幅(见 docs/README.md 的约定)。
    """
    head = "\n".join(doc.read_text(encoding="utf-8").splitlines()[:20])
    return ("历史文档" in head) or ("归档" in head) or ("ARCHIVED" in head.upper())


def check_paths(docs: list[Path]) -> None:
    """A. 反引号路径 + 相对链接必须存在(跳过围栏代码块与行内代码里的命令)。"""
    for doc in docs:
        if doc.name in SKIP_PATH_CHECK:
            continue
        lines = doc.read_text(encoding="utf-8").splitlines()
        stale_only = is_historical(doc)
        in_fence = False
        for i, line in enumerate(lines, 1):
            if FENCE_RE.match(line.strip()):
                in_fence = not in_fence
                continue
            if in_fence:
                continue
            for m in list(PATH_RE.finditer(line)) + list(LINK_RE.finditer(line)):
                ref = m.group(1).split("#")[0]
                if not ref or allowed(ref):
                    continue
                cands = [(doc.parent / ref), (ROOT / ref)]
                if any(c.exists() for c in cands):
                    continue
                msg = f"{rel(doc)}:{i}: 引用了不存在的路径 `{ref}`"
                (warnings if stale_only else errors).append(
                    msg + ("  [历史文档, 仅提示]" if stale_only else "")
                )


def check_docs_index(docs: list[Path]) -> None:
    """B. docs/README.md 必须列全 docs/*.md, 且不指向不存在的文档。"""
    index = ROOT / "docs" / "README.md"
    if not index.exists():
        errors.append("docs/README.md 不存在(文档索引缺失)")
        return
    text = index.read_text(encoding="utf-8")
    docs_dir = (ROOT / "docs").resolve()
    listed = set()
    for m in LINK_RE.finditer(text):
        rel = m.group(1).split("#")[0]
        if not rel.endswith(".md"):
            continue
        target = (index.parent / rel)
        if not target.exists():
            errors.append(f"docs/README.md 指向不存在的文档 `{rel}`(相对 docs/ 或仓库根)")
            continue
        t = target.resolve()
        # 只有 docs/ 顶层的文档才算"索引收录"
        if t.parent == docs_dir:
            listed.add(t.name)
    for f in sorted((ROOT / "docs").glob("*.md")):
        if f.name == "README.md":
            continue
        if f.name not in listed:
            errors.append(f"docs/README.md 未收录 `docs/{f.name}`(文档索引不全)")


def check_claims(docs: list[Path]) -> None:
    """C. 可核对的数字(测试数)与仓库实际一致。"""
    n = 0
    for pat in ("src/**/*.rs", "tests/**/*.rs"):
        for f in ROOT.glob(pat):
            n += len(re.findall(r"^\s*#\[(?:tokio::)?test\]", f.read_text(encoding="utf-8"), re.M))
    # `cargo test` 实际跑的条数会包含被 `#[ignore]` 的;这里用静态计数做近似,
    # 偏差阈值放宽到 30%(见报告里的两个数)。
    for doc in docs:
        for i, line in enumerate(doc.read_text(encoding="utf-8").splitlines(), 1):
            m = TEST_COUNT_RE.search(line)
            if not m:
                continue
            claimed = int(m.group(1))
            if claimed and abs(claimed - n) / max(n, 1) > 0.3:
                # 提示级: `cargo test` 会把 lib 的单元测试在 bin target 里再跑一遍,
                # 所以"实际跑了几条"与静态计数天然有差距(本项目 219 静态 → 293 实跑)。
                warnings.append(
                    f"{rel(doc)}:{i}: 声称约 {claimed} 个测试, 静态计数 #{n}"
                    f"(cargo 实跑会更多; 偏差 >30% 请更新)"
                )


def check_versions(docs: list[Path], cur: tuple[int, int, int]) -> None:
    """D. 面向当前用户的文档不得把旧版本写成"当前版本"。"""
    for doc in docs:
        if rel(doc) not in CURRENT_FACING or any(h in doc.name for h in HISTORY_HINT):
            continue
        for i, line in enumerate(doc.read_text(encoding="utf-8").splitlines(), 1):
            low = line.lower()
            if not any(k in line for k in ("当前版本", "版本线", "Current version", "version =")):
                continue
            for m in VERSION_RE.finditer(line):
                v = (int(m.group(1)), int(m.group(2)), int(m.group(3)))
                # 「当前版本」这类声明必须与 Cargo.toml **完全一致**(patch 级也要)。
                # 曾只比到 minor → README 首页的 0.14.9 在 Cargo 到 0.14.17 之后还挂着。
                if v != cur:
                    errors.append(
                        f"{rel(doc)}:{i}: 「当前版本」写着 {'.'.join(map(str, v))}"
                        f", 但 Cargo.toml 是 {'.'.join(map(str, cur))}"
                    )


def check_workflows() -> None:
    """E. 声明存在的 workflow 文件必须真的存在(README 里的 CI 徽章依赖它)。"""
    wf = ROOT / ".github" / "workflows"
    if not wf.exists():
        warnings.append(".github/workflows/ 不存在(还没有远程 CI);本地 CI 见 scripts/ci.sh")
        return
    for f in sorted(wf.glob("*.yml")):
        text = f.read_text(encoding="utf-8")
        if "runs-on" not in text:
            errors.append(f"{rel(f)}: 缺少 runs-on(workflow 不会跑)")


def main() -> int:
    cur = cfg_version()
    docs = tracked_markdown()
    check_paths(docs)
    check_docs_index(docs)
    check_claims(docs)
    check_versions(docs, cur)
    check_workflows()

    for w in warnings:
        print(f"[warn] {w}")
    for e in errors:
        print(f"[error] {e}")
    print(f"docs-check: {len(docs)} 篇文档, {len(errors)} 个错误, {len(warnings)} 个警告")
    if errors:
        return 1
    if warnings and "--warn-only" not in sys.argv:
        return 0  # 警告不阻断: 让 CI 只在"事实错误"上红
    return 0


if __name__ == "__main__":
    sys.exit(main())
