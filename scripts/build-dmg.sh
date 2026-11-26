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

PUBLIC="${HIVECAST_PUBLIC_MACOS:-false}"
case "$PUBLIC" in
  true|false) ;;
  *) echo "HIVECAST_PUBLIC_MACOS 必须是 true 或 false" >&2; exit 1 ;;
esac
STAGE="$(mktemp -d)"
KEYCHAIN=""
cleanup() {
  if [[ -n "$KEYCHAIN" ]]; then security delete-keychain "$KEYCHAIN" >/dev/null || true; fi
  rm -rf "$STAGE"
}
trap cleanup EXIT
APP_COPY="$STAGE/payload/HIVECAST.app"
mkdir -p "$STAGE/payload"
ditto "$APP" "$APP_COPY"

if [[ "$PUBLIC" == true ]]; then
  : "${APPLE_SIGNING_IDENTITY:?需要 Developer ID Application 身份}"
  : "${APPLE_ID:?需要公证账号}" "${APPLE_TEAM_ID:?需要开发者团队 ID}" "${APPLE_PASSWORD:?需要公证专用密码}"
  case "$APPLE_SIGNING_IDENTITY" in "Developer ID Application:"*) ;; *) echo "需要 Developer ID Application 身份" >&2; exit 1;; esac
  signing_args=(--force --timestamp --sign "$APPLE_SIGNING_IDENTITY")
  if [[ -n "${APPLE_CERTIFICATE:-}" ]]; then
    : "${APPLE_CERTIFICATE_PASSWORD:?需要证书密码}"
    KEYCHAIN="$STAGE/signing.keychain-db"
    pass="$(openssl rand -hex 24)"
    security create-keychain -p "$pass" "$KEYCHAIN"
    security unlock-keychain -p "$pass" "$KEYCHAIN"
    security set-keychain-settings -lut 3600 "$KEYCHAIN"
    printf '%s' "$APPLE_CERTIFICATE" | base64 -D > "$STAGE/identity.p12"
    security import "$STAGE/identity.p12" -k "$KEYCHAIN" -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign >/dev/null
    security set-key-partition-list -S apple-tool:,apple: -s -k "$pass" "$KEYCHAIN" >/dev/null
    signing_args+=(--keychain "$KEYCHAIN")
  fi
  codesign --options runtime "${signing_args[@]}" "$APP_COPY"
  notary=(--apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_PASSWORD")
  ditto -c -k --keepParent "$APP_COPY" "$STAGE/app.zip"
  xcrun notarytool submit "$STAGE/app.zip" "${notary[@]}" --wait
  xcrun stapler staple "$APP_COPY"
  xcrun stapler validate "$APP_COPY"
  spctl --assess --type execute "$APP_COPY"
elif ! codesign --verify --deep --strict "$APP_COPY" 2>/dev/null; then
  codesign --force --sign - "$APP_COPY"
fi
codesign --verify --deep --strict "$APP_COPY"
ln -s /Applications "$STAGE/payload/Applications"

mkdir -p "$OUT_DIR"
rm -f "$DMG"
hdiutil create -volname HIVECAST -srcfolder "$STAGE/payload" -ov -format UDZO "$DMG" >/dev/null
if [[ "$PUBLIC" == true ]]; then
  codesign "${signing_args[@]}" "$DMG"
  xcrun notarytool submit "$DMG" "${notary[@]}" --wait
  xcrun stapler staple "$DMG"
  xcrun stapler validate "$DMG"
  spctl --assess --type open --context context:primary-signature "$DMG"
fi
hdiutil verify "$DMG" >/dev/null
echo "dmg: $DMG"
du -h "$DMG" | awk '{print "size:", $1}'
