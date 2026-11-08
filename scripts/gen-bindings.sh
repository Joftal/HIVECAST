#!/usr/bin/env bash
# 从 Rust 类型导出 TS 绑定（ts-rs）。改完 ipc-types 后必须重跑，否则前端类型会漂移。
set -euo pipefail
cd "$(dirname "$0")/.."

cargo test --manifest-path src-tauri/ipc-types/Cargo.toml --quiet
# ts-rs 在类型行尾留空格，清掉，避免 git diff --check 失败。
perl -pi -e 's/[ \t]+$//' src/lib/bindings/*.ts
echo "bindings written to src/lib/bindings:"
ls -1 src/lib/bindings
