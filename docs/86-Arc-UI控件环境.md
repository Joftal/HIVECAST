# 86-Arc UI 控件环境

> 状态：环境就绪（2026-11-08）。尚未替换任何现有界面，控件逐个迭代时再改页面。
> 范围：uiarc.dev（Arc）免费组件的安装方式、设计 token 桥接、开发期预览台与验证命令。
> 前置阅读：`40-UIUX-设计方案.md`（现有 token 与组件层）、`49-设置页重做方案.md`。

## 0. 结论速览

- Arc 是 **shadcn 兼容 registry**，不是 npm 组件库：组件源码装进仓库，样式走 **CSS Modules + CSS 变量**，不写 Tailwind 类。
- 已装好 `components.json`（含 `@uiarc` registry）、`@/` 路径别名、`clsx` / `tailwind-merge` / `lucide-react` / `motion` 依赖。
- 已装 `arc-foundation`（`src/components/arc/foundation.css`）与首个示例组件 `theme-switch`（连带 `button`、motion tokens）。
- 新增 `src/styles/arc-bridge.css`，把 Arc 的语义 token 映射到 HIVECAST 的 `--c-*` 面板，并统一控制高度与控件字号。
- 开发期预览台：`pnpm dev` 后访问 `http://localhost:1420/?arc-lab=1`，文件 `src/features/design/ArcLab.tsx`。
- 未做：没有替换顶栏主题按钮、没有引入 Pro 组件、没有改动任何现有页面。

## 1. 安装与目录

注册表已写入 `components.json`：

```json
{ "registries": { "@uiarc": "https://uiarc.dev/r/{name}.json" } }
```

安装一个免费组件：

```bash
pnpm dlx shadcn@latest add @uiarc/switch
# 等价的完整 URL
pnpm dlx shadcn@latest add https://uiarc.dev/r/switch.json
```

- 文件落在 `src/components/arc/<name>/<name>.tsx` 与同名 `.module.css`；
- 组件按需自动带上依赖项（`arc-motion-tokens`、`button` 等），不重复安装 foundation；
- 导入方式：`import { Switch } from "@/components/arc/switch/switch";`；
- Pro 组件需要 `ARC_PRO_TOKEN`，v1 不引入，也不根据截图重建源码。

## 2. 主题与 token 桥接

Arc 用 `<html data-theme="dark">` 切暗色，**与现有 `applyTheme()` 完全一致**，不需要改主题实现。
强调色可用 `data-accent`（neutral / violet / blue / green / amber / orange / coral / rose），本项目不使用它，统一走桥接。

`src/styles/arc-bridge.css` 做的事：

| Arc token | 映射到 | 说明 |
| --- | --- | --- |
| `--background` / `--surface` / `--surface-muted` | `--c-canvas` / `--c-surface` / `--c-raised` | 页面、卡片、悬浮层 |
| `--foreground` / `--text-secondary` / `--text-muted` | `--c-ink` / `--c-muted` / `--c-faint` | 三级文字 |
| `--border` / `--border-strong` | `--c-line` / `--c-line-strong` | 描边 |
| `--accent` / `--accent-subtle` / `--accent-foreground` | `--c-accent` / `--c-accent-soft` / `--c-accent-ink` | 浅色蓝、深色琥珀自动跟随 |
| `--control-on` / `--control-glyph` / `--control-track` | 强调色 / 强调色前景 / 强描边 | 开关、勾选、单选、滑杆 |
| `--success` / `--warning` / `--danger` | `--c-ok` / `--c-warn` / `--c-rec` | 状态色 |
| `--radius-control` / `--radius-panel` / `--radius-surface` | 6 / 10 / 8px | 对齐 `rounded-ctl` / `rounded-panel` / `rounded-card` |
| `--control-height-*` | 28 / 32 / 36px | 对齐现有 `h-7` / `h-8` / `h-9` |
| `--text-xs` / `--text-sm` / `--text-base` | 11 / 12 / 13px | 对齐桌面端紧凑字号 |

导入顺序固定在 `src/main.tsx`：`foundation.css` → `styles/index.css` → `styles/arc-bridge.css`。
桥接文件只放全局语义映射；单个控件的尺寸、间距、字体在迭代该控件时再改，不改 foundation。

## 3. 开发期预览台

`src/features/design/ArcLab.tsx` 是临时控件台，只在开发环境生效：

```tsx
const lab = import.meta.env.DEV && new URLSearchParams(window.location.search).has("arc-lab");
```

- `pnpm dev` → `http://localhost:1420/?arc-lab=1`；
- 当前放 `ThemeSwitch`（reveal / eclipse / iconOnly）与 `Button`（四种 variant、三种 size、loading），用于核对浅色/深色下的 token；
- 迭代新控件时直接加一段预览，验收后再决定是否搬进正式页面；
- 生产构建不会渲染它。

## 4. 已知取舍

- **Button primary 是黑白，不是品牌蓝**：Arc 的 `.primary` 用 `--foreground` 当底色。要不要给 HIVECAST 加一个 accent 主按钮变体，等迭代 Button 时再定；不要在桥接层硬改 `.primary`（CSS Modules 类名带哈希）。
- **组件源码可改但要留痕**：Arc 是 copy-paste 模式，改 `src/components/arc/**` 属于本项目代码；重新执行 `shadcn add --overwrite` 会覆盖，改动前先记录。
- **shadcn CLI 依赖根 `tsconfig.json` 的 paths**：根配置原本只有 references，CLI 解析不到 `@/` 会真的创建 `@/components/...` 目录。已在根 `tsconfig.json` 补 `baseUrl` 与 `paths`，后续安装不会再跑偏。
- **CSS Modules 与 Tailwind 并存**：Arc 组件内部不认 Tailwind 类，外层容器仍然用现有 Tailwind；不要在 Arc 组件上叠 `className` 期望改内部样式。
- **Windows 字体**：foundation 的 display/body 字体已指向 `--font-sans`，不会去加载 Geist / Inter Variable。

## 5. 验证

```bash
pnpm exec tsc -b          # 组件源码与类型
pnpm build                # 构建
pnpm run check:i18n       # 615 keys
pnpm run check:docs       # 文档表格 / 链接 / 代码围栏
```

- 浅色/深色实测：`?arc-lab=1` 下 ThemeSwitch 切换 `data-theme`，页面、按钮、卡片、强调色同步；截图见 `docs/assets/`（本轮未入库）。
- 本轮 `tsc -b`、`pnpm build` 通过；`check:i18n` 615 keys、`check:docs` 87 files 通过。
