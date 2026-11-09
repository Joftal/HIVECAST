# 104-监控选择工具栏改用 Arc FloatingButtonGroup

> 状态：v1 已实施（2026-11-09）；前端编译与构建通过，Tauri 真实窗口的多选悬浮栏待复看。
> 范围：监控页选中直播间后底部出现的浮动工具栏，从自绘 `div + Button` 换成 Arc（uiarc.dev）`FloatingButtonGroup`。
> 前置阅读：`92-四页结构统一.md`、`101-监控列表细节优化.md`、`103-监控页性能与加载优化.md`。

## 0. 结论速览

- 悬浮栏改为 Arc `FloatingButtonGroup`：`variant="floating"`、`size="sm"`，沿用 Arc 的托盘、共享高亮、移动端按压和键盘导航。
- 选中数量与「分屏模式」作为托盘内的静态前置文案，不再单独用自绘容器。
- 清除选择、分隔线、各分屏模式都变成标准 toolbar item；分屏模式带 `LayoutGlyph` 图标。
- 工具栏获得单一 Tab 停靠点，方向键在按钮间移动，`Home` / `End` 跳首尾，`Enter` / `Space` 执行。

## 1. 结构

```tsx
<div className="absolute bottom-6 left-1/2 z-40 -translate-x-1/2">
  <FloatingButtonGroup
    label={t("monitor.selectionActions")}
    variant="floating"
    size="sm"
    leading={<>已选 {{n}} 个 · 分屏模式</>}
    items={[
      { id: "clear", label: t("monitor.clearSelection"), onSelect: clear },
      { type: "separator", id: "selection-sep" },
      ...splitModes.map((mode) => ({
        id: mode.kind,
        label: mode.label,
        icon: <LayoutGlyph kind={mode.kind} />,
        onSelect: () => onOpenWatch?.(pickedIds, mode.kind),
      })),
    ]}
  />
</div>
```

- 外层 `div` 只负责绝对定位和水平居中；托盘自身保持 Arc 的 `position: relative`，避免工具类与组件样式互相覆盖。
- 分屏模式仍按选中数量给出：1 单画面、2 左右/上下、3 上大下二/左大右二、4 田字。
- 清除选择是普通 action，不设为 destructive：它只退出分屏选择，不删除任何数据。

## 2. 组件扩展：leading

Arc 原组件只有按钮和分隔线，没有放「已选 N 个」这类静态文案的位置。为保持一个完整托盘，新增可选 `leading?: ReactNode`：

- 渲染在托盘内、所有按钮之前，不包成按钮。
- 不写入 `[data-fbg-item]`，因此不参与共享高亮测量、Tab 停靠点和方向键循环。
- 外层带 `aria-live="polite"`，选中数量变化时读屏可感知。
- `.leading` 使用小号次要文字、`z-index: 2`，位于共享高亮之上，但不拦截指针（纯文本）。

这是 HIVECAST 对 vendored Arc 组件的本地扩展；上游无此 prop，升级组件时需保留。

## 3. 交互与可访问性

- 托盘 `role="toolbar"`，`aria-label` 为 `monitor.selectionActions`（已选直播间操作 / Selected room actions）。
- 整组只有一个 Tab 停靠点；焦点进入后左右方向键移动，`Home` / `End` 跳首尾，`Enter` / `Space` 触发。
- 每个分屏按钮的可见文字就是可访问名称；`LayoutGlyph` 仅作装饰，`aria-hidden`。
- 分隔线使用 `role="separator"`，方向随横向托盘。
- 清除后 `picked` 清空，悬浮栏立即卸载；Arc 的退出动画由组件内部处理。

## 4. 验证

- `pnpm exec tsc -b`、`pnpm build` 通过。
- `check:i18n` 646 keys（新增 `monitor.selectionActions`）、`check:docs` 105 files 通过。
- 键盘逻辑沿用 Arc 组件既有实现；本轮未改其高亮和焦点算法。
- Tauri 真实窗口选中 2/3/4 个直播间时，分屏按钮数量与文案需复看。

## 5. 已知限制

- **托盘不换行**：窗口很窄或文案很长时仍可能超出视口；与旧悬浮栏一致，后续可对窄宽度改用 `iconOnly`。
- **leading 是静态内容**：如果以后需要在托盘内放可点击的状态按钮，应使用 action item，而不是继续扩展 leading。
- **aria-live 可能偏吵**：连续勾选时读屏会播报每次数量变化；若反馈过度，可去掉 live region 或改为节流。
