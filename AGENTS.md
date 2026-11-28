# HIVECAST 协作说明

## 桌面自动化：agent-desktop

Use `agent-desktop` to see and operate desktop apps. Before the first use, run
`agent-desktop skills get desktop` and `agent-desktop skills get platform`, and follow them.

首次使用或升级 agent-desktop 后，先执行：

```bash
agent-desktop status
agent-desktop skills get desktop
agent-desktop skills get platform
```

官方文档：

- https://agent-browser.dev/desktop
- https://agent-browser.dev/desktop/skills

### 本项目约定

- 应用名固定为 `HIVECAST`；开发 bundle 路径为
  `/Library/dshproject/HIVECAST/src-tauri/target/release/bundle/macos/HIVECAST.app`。
- 优先使用 `snapshot` / `find` 获取 qualified ref，再执行 `click`、`set-value` 等语义操作；
  不要把屏幕坐标点击（如 `hcclick2`）作为首选。只有没有可访问性路径且用户明确允许物理输入时，才使用 `--headed`。
- 每次动作后重新 `snapshot`，或用 `get` / `is` 验证结果。遇到 `STALE_REF` 或
  `AMBIGUOUS_TARGET` 时重新快照；`disposition.retry` 不是 `safe` 时不要重试同一动作。
- 默认使用 headless 语义操作。`--headed` 仅用于用户明确允许的物理点击、拖拽、悬停等场景。
- 不自动执行退出登录、删除录像、支付等破坏性操作。验收优先使用测试房间和测试数据，并先备份
  `~/Library/Application Support/com.hivecast.desktop/hivecast.db`。
- 应用未启动时可用 `agent-desktop launch com.hivecast.desktop --activate`；
  测试结束后用 `agent-desktop close-app HIVECAST` 正常退出。

## Windows 真机测试

- Windows 相关功能验证固定使用项目约定的远程 Windows 测试机；连接方式、源码路径和工具链位置见本地 `GEMINI.md`（仅本机，不入库）。
- 测试前必须先在远端同步到 Mac 端最新提交：`git fetch origin && git pull --ff-only origin main`，确认 `git rev-parse HEAD` 与 Mac 一致、工作区干净；不要用旧构建做验收。
- 远端 GUI 必须在交互桌面 Session 1 通过一次性计划任务启动；SSH 默认在 Session 0，直接启动会以 101 退出。
- 安装/卸载测试只用 `%TEMP%` 下的独立目录，并确认没有既有 HIVECAST 卸载注册表项；不得操作真实 Cookie、账号数据或录像。测试后清理测试目录、注册表项和辅助脚本。
