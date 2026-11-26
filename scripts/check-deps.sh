#!/usr/bin/env bash
# 依赖升级后的固定检查。默认只跑本地可重复项；审计、过期检查与打包需显式开启。
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

audit=0
outdated=0
release=0

usage() {
  cat <<'EOF'
用法: bash scripts/check-deps.sh [--audit] [--outdated] [--release]

  --audit     运行 pnpm audit 与 cargo audit（macOS/Windows 两个目标）
  --outdated  列出前端与 Rust 直接依赖的可用更新（cargo-upgrade 需已安装）
  --release   额外构建 macOS .app 与 DMG
  -h, --help  显示本帮助

默认检查：冻结安装、前端 i18n/docs/build、Rust fmt/clippy/test。
EOF
}

for arg in "$@"; do
  case "$arg" in
    --audit) audit=1 ;;
    --outdated) outdated=1 ;;
    --release) release=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "未知参数: $arg" >&2; usage >&2; exit 2 ;;
  esac
done

step() { printf '\n== %s ==\n' "$1"; }

step "工具链"
node --version
pnpm --version
rustc --version
cargo --version

step "冻结安装"
pnpm install --frozen-lockfile

step "前端检查与构建"
pnpm check:i18n
pnpm check:docs
pnpm build

step "Rust 格式"
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check

step "Rust Clippy"
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked

step "Rust 测试"
cargo test --manifest-path src-tauri/Cargo.toml --locked
cargo test --manifest-path src-tauri/ipc-types/Cargo.toml --locked

if ((outdated)); then
  step "前端过期检查"
  pnpm outdated || true

  step "Rust 过期检查"
  if command -v cargo-upgrade >/dev/null 2>&1; then
    (cd src-tauri && cargo upgrade --dry-run --ignore-rust-version -vv)
  else
    echo "未安装 cargo-upgrade；执行 cargo install cargo-upgrade 后再试" >&2
    exit 1
  fi
fi

if ((audit)); then
  step "npm 安全审计"
  pnpm audit

  step "Rust 安全审计（macOS / Windows）"
  for target in macos windows; do
    echo "-- target-os $target"
    (cd src-tauri && cargo audit --target-os "$target")
  done
fi

if ((release)); then
  step "release 构建"
  pnpm tauri build --bundles app

  step "DMG"
  bash scripts/build-dmg.sh --no-build
fi

step "完成"
