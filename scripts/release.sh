#!/usr/bin/env bash
# ============================================================================
# 发版脚本 —— 把"之前流程"固化下来, 避免每次靠记忆。
#
#   ./scripts/release.sh                 # 用 Cargo.toml 里的版本发
#   ./scripts/release.sh 0.15.0          # 指定版本(会先改 Cargo.toml 并提交)
#   ./scripts/release.sh --dry-run       # 演练: 只检查与构建, 不推送不建 release
#
# 发版前检查(任一不过就停):
#   1. 工作区干净、在 main 分支、远端没有同名 tag
#   2. CHANGELOG.md 里有该版本的条目(没有就报错并给出模板)
#   3. 本地 CI 通过(可用 --skip-ci 跳过, 不推荐)
#   4. glibc 2.17 交叉构建成功(最老支持到 CentOS 7 / Ubuntu 16.04)
#
# 发布物: target/x86_64-unknown-linux-gnu/release/wal-rust + CHANGELOG 里的该版本说明
# ============================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

ZIG_TARGET=x86_64-unknown-linux-gnu.2.17   # `.2.17` 是 glibc 标记
OUT_TARGET=x86_64-unknown-linux-gnu        # 产物目录名不带标记
BIN="target/$OUT_TARGET/release/wal-rust"
DRY=0
SKIP_CI=0
VERSION=""

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY=1; shift ;;
        --skip-ci) SKIP_CI=1; shift ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        *) VERSION="$1"; shift ;;
    esac
done

say() { printf '\033[1m==> %s\033[0m\n' "$*"; }
die() { printf '\033[31m!! %s\033[0m\n' "$*" >&2; exit 1; }

export CARGO_HOME="${CARGO_HOME:-$REPO_ROOT/.cargo-home}"
export XDG_CACHE_HOME="${XDG_CACHE_HOME:-$REPO_ROOT/.tools/cache}"
ZIG_DIR="$REPO_ROOT/.tools/zig-linux-x86_64-0.13.0"
[ -x "$ZIG_DIR/zig" ] && export PATH="$ZIG_DIR:$PATH"
export PATH="$CARGO_HOME/bin:$PATH"

# --- 1. 版本与仓库状态 --------------------------------------------------------
CFG_VERSION="$(sed -n 's/^version = "\([0-9.]*\)"/\1/p' Cargo.toml | head -1)"
if [ -n "$VERSION" ] && [ "$VERSION" != "$CFG_VERSION" ]; then
    say "把 Cargo.toml 版本改为 $VERSION(原 $CFG_VERSION)"
    if [ $DRY -eq 0 ]; then
        sed -i "s/^version = \"$CFG_VERSION\"/version = \"$VERSION\"/" Cargo.toml
        git add Cargo.toml
        git commit -q -m "chore(release): v$VERSION" || die "提交版本号失败"
    fi
    CFG_VERSION="$VERSION"
fi
VERSION="$CFG_VERSION"
TAG="v$VERSION"
say "版本: $VERSION (tag: $TAG)"

[ -n "$(git status --porcelain)" ] && die "工作区有未提交改动; 发版必须从干净的工作区开始"

BRANCH="$(git rev-parse --abbrev-ref HEAD)"
[ "$BRANCH" = "main" ] || die "当前分支是 $BRANCH, 发版请在 main 上"

if git ls-remote --tags origin "refs/tags/$TAG" 2>/dev/null | grep -q "$TAG"; then
    die "远端已存在 tag $TAG; 新版本请先改 Cargo.toml 的版本号"
fi

# --- 2. CHANGELOG ------------------------------------------------------------
say "检查 CHANGELOG.md 是否有 $VERSION 的条目"
if ! grep -qE "^##\s*\[?$VERSION\]?" CHANGELOG.md; then
    die "CHANGELOG.md 里没有 $VERSION 条目。先补:

## [$VERSION] - $(date +%F)

### Added
### Changed
### Fixed

然后把「未发布」一节清空。"
fi
NOTES="$(awk -v v="$VERSION" '
    $0 ~ "^##[[:space:]]*\\[?"v"\\]?" {found=1; next}
    found && /^##[[:space:]]/ {exit}
    found {print}
' CHANGELOG.md)"
[ -n "$(printf '%s' "$NOTES" | tr -d '[:space:]')" ] || die "CHANGELOG 里 $VERSION 一节是空的"

# --- 3. 本地 CI --------------------------------------------------------------
if [ $SKIP_CI -eq 1 ]; then
    say "跳过本地 CI(--skip-ci)"
else
    say "跑本地 CI(全套必过阶段)"
    ./scripts/ci.sh || die "CI 未通过, 不发布"
fi

# --- 4. 交叉构建 -------------------------------------------------------------
command -v cargo-zigbuild >/dev/null || die "缺 cargo-zigbuild; 见 scripts/install.sh"
command -v zig >/dev/null || die "缺 zig; 见 scripts/install.sh"
say "构建 $ZIG_TARGET"
if [ $DRY -eq 1 ]; then
    echo "  (dry-run: 跳过实际构建)"
else
    cargo zigbuild --release --target "$ZIG_TARGET"
    [ -x "$BIN" ] || die "构建产物不存在: $BIN"
    "$BIN" --version
    echo "  glibc 需求: $(objdump -T "$BIN" | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -1)"
    echo "  sha256: $(sha256sum "$BIN" | cut -c1-16)…"
fi

if [ $DRY -eq 1 ]; then
    say "演练完成: 一切就绪(未推送、未创建 release)"
    exit 0
fi

# --- 5. 推送 + 建 release ----------------------------------------------------
say "推送 main"
if ! git push origin main 2>.tools/push.err; then
    # 有些环境(容器/受限 ssh 配置)下 ssh 推不上去;此时用 gh 的 HTTPS 凭据重试。
    if command -v gh >/dev/null && gh auth status >/dev/null 2>&1; then
        say "ssh 推送失败, 改用 gh 的 HTTPS 凭据重试"
        slug="$(gh repo view --json nameWithOwner -q .nameWithOwner)"
        git -c credential.helper='!gh auth git-credential' push "https://github.com/$slug.git" main
    else
        cat .tools/push.err >&2
        die "推送失败: 见上面的错误(可手动 git push 后重新执行本脚本)"
    fi
fi

say "创建 GitHub release $TAG"
printf '%s\n' "$NOTES" > .tools/release-notes.md
gh release create "$TAG" \
    --title "$TAG" \
    --notes-file .tools/release-notes.md \
    "$BIN"

say "完成: $(gh release view "$TAG" --json url -q .url 2>/dev/null || echo "$TAG 已创建")"
echo
echo "提醒: 下一条 CHANGELOG 条目请开「## [未发布]」一节(见 CONTRIBUTING.md「发版流程」)"
