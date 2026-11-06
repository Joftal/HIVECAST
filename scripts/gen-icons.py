
#!/usr/bin/env python3
"""从 app-icons-v2/ 生成运行时需要的全部图标资产。

用法（需要 Pillow，仓库只提交产物，构建不依赖本脚本）：
    python3 scripts/gen-icons.py

产物：
    src-tauri/icons/**            应用图标（macOS .icns / Windows .ico / 各尺寸 PNG / MSIX 方块图）
    src-tauri/icons/windows/*.png  Windows 任务栏浅色/深色版本，运行时按系统主题切换
    src-tauri/icons/tray/*.png    托盘图标（macOS 模板图 + Windows 浅/深两版）
    src/assets/app-icon.png       界面里的品牌标记（顶栏、关于面板）
    public/favicon.png            网页预览用的 favicon
"""

from pathlib import Path
import shutil

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "app-icons-v2"
ICONS = ROOT / "src-tauri" / "icons"
ASSETS = ROOT / "src" / "assets"
PUBLIC = ROOT / "public"

MAC_ART = SRC / "macos-rounded" / "png" / "icon-1024.png"
WIN_LIGHT = SRC / "windows-taskbar" / "light" / "png" / "taskbar-light-1024.png"
WIN_DARK = SRC / "windows-taskbar" / "dark" / "png" / "taskbar-dark-1024.png"


def load(path: Path) -> Image.Image:
    return Image.open(path).convert("RGBA")


def resize(im: Image.Image, size: int) -> Image.Image:
    return im.resize((size, size), Image.LANCZOS)


def save(im: Image.Image, path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    im.save(path)
    print(f"  {path.relative_to(ROOT)}  {im.width}x{im.height}")


def draw_tray(color: str) -> Image.Image:
    """托盘图标：一块显示器 + 播放三角，取自图标里的显示器造型。

    先在 4 倍尺寸上画再缩小，边缘才有抗锯齿。macOS 用纯黑模板图（由系统着色），
    Windows 没有模板图机制，浅色任务栏给深色字形、深色任务栏给白色字形。
    """
    scale = 4
    size = 36
    canvas = Image.new("RGBA", (size * scale, size * scale), (0, 0, 0, 0))
    d = ImageDraw.Draw(canvas)
    s = scale

    def box(x0, y0, x1, y1):
        return [x0 * s, y0 * s, x1 * s, y1 * s]

    # 显示器外框
    d.rounded_rectangle(box(2.5, 5.5, 33.5, 25.5), radius=3.2 * s, fill=color)
    # 屏幕（挖空）
    d.rounded_rectangle(box(5.2, 8.2, 30.8, 22.8), radius=1.6 * s, fill=(0, 0, 0, 0))
    # 播放三角
    d.polygon(
        [(15.2 * s, 11.4 * s), (23.2 * s, 15.5 * s), (15.2 * s, 19.6 * s)],
        fill=color,
    )
    # 支架与底座
    d.polygon(
        [(16.2 * s, 25.5 * s), (19.8 * s, 25.5 * s), (21.2 * s, 30.6 * s), (14.8 * s, 30.6 * s)],
        fill=color,
    )
    d.rounded_rectangle(box(10.8, 30.4, 25.2, 33.4), radius=1.4 * s, fill=color)
    return canvas.resize((size, size), Image.LANCZOS)


def main() -> None:
    for path in (MAC_ART, WIN_LIGHT, WIN_DARK):
        if not path.exists():
            raise SystemExit(f"缺少源文件：{path}")

    mac = load(MAC_ART)
    win_light = load(WIN_LIGHT)
    win_dark = load(WIN_DARK)

    print("应用图标")
    save(resize(mac, 1024), ICONS / "icon.png")
    save(resize(mac, 512), ICONS / "128x128@2x.png")
    save(resize(mac, 256), ICONS / "128x128.png")
    save(resize(mac, 128), ICONS / "64x64.png")
    save(resize(mac, 64), ICONS / "32x32.png")

    # Windows 安装包 / exe 图标：多尺寸，从浅色版本出。
    ico_sizes = [(16, 16), (20, 20), (24, 24), (32, 32), (40, 40), (48, 48), (64, 64), (128, 128), (256, 256)]
    win_light.save(ICONS / "icon.ico", sizes=ico_sizes)
    print(f"  src-tauri/icons/icon.ico  {[s[0] for s in ico_sizes]}")

    # MSIX（Windows 商店包）方块图与徽标，Tauri 只在 targets 含 msix 时用到。
    for size in (30, 44, 71, 89, 107, 142, 150, 284, 310):
        save(resize(win_light, size), ICONS / f"Square{size}x{size}Logo.png")
    save(resize(win_light, 50), ICONS / "StoreLogo.png")

    # macOS .icns 直接用设计稿里已经打包好的那份。
    shutil.copyfile(SRC / "macos-rounded" / "AppIcon-rounded.icns", ICONS / "icon.icns")
    print("  src-tauri/icons/icon.icns  (copied)")

    print("Windows 任务栏（运行时按主题切换）")
    save(resize(win_light, 64), ICONS / "windows" / "taskbar-light.png")
    save(resize(win_dark, 64), ICONS / "windows" / "taskbar-dark.png")
    for size in (16, 20, 24, 32, 48):
        save(resize(win_light, size), ICONS / "windows" / f"taskbar-light-{size}.png")
        save(resize(win_dark, size), ICONS / "windows" / f"taskbar-dark-{size}.png")

    print("托盘图标")
    save(draw_tray("#000000"), ICONS / "tray" / "tray-mac.png")
    save(draw_tray("#1b1b1f"), ICONS / "tray" / "tray-win-light.png")
    save(draw_tray("#ffffff"), ICONS / "tray" / "tray-win-dark.png")

    print("界面素材")
    save(resize(mac, 256), ASSETS / "app-icon.png")
    # 顶栏的小标记：裁掉画布四周的透明留白，让图形填满 24px 的方块。
    bbox = mac.split()[3].getbbox()
    save(resize(mac.crop(bbox), 96), ASSETS / "app-mark.png")
    save(resize(mac, 64), PUBLIC / "favicon.png")
    save(resize(win_light, 180), PUBLIC / "apple-touch-icon.png")
    save(resize(mac, 512), PUBLIC / "icon.png")


if __name__ == "__main__":
    main()
