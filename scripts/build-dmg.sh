#!/usr/bin/env bash
# 打 macOS 安装包（.dmg）。
#
# 为什么不用 tauri 自带的 dmg 打包：
#   tauri 的 bundle_dmg.sh 会用 AppleScript 指挥 Finder 摆图标/窗口大小，
#   在没有 Finder 自动化权限的环境（CI、tmux、被 MDM 管控的机器）里会等到
#   AppleEvent 超时（-1712），整个 build 直接失败。
# 这里改成「先出 .app，再用 hdiutil 压成 dmg」：没有花哨的窗口布局，
# 但任何机器/CI 都能稳定产出可双击安装的 dmg。
#
# 用法：scripts/build-dmg.sh            # 全量构建
#       scripts/build-dmg.sh --no-build # 复用已有 .app 重新打包
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"
APP="$ROOT/src-tauri/target/release/bundle/macos/HIVECAST.app"
OUT_DIR="$ROOT/src-tauri/target/release/bundle/dmg"
VERSION="$(node -p "require('./src-tauri/tauri.conf.json').version")"
ARCH="$(uname -m)"
DMG="$OUT_DIR/HIVECAST_${VERSION}_${ARCH}.dmg"

if [[ "${1:-}" != "--no-build" ]]; then
  PATH="$HOME/.local/bin:$PATH" pnpm tauri build --bundles app
fi

[[ -d "$APP" ]] || { echo "找不到 $APP，先跑一次构建" >&2; exit 1; }

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"

mkdir -p "$OUT_DIR"
rm -f "$DMG"
hdiutil create -volname HIVECAST -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
echo "dmg: $DMG"
du -h "$DMG" | awk '{print "size:", $1}'
