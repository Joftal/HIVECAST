# 210 · eae0408 修复复核与 macOS 专机实机验收

2026-11-27 · 复核提交 `eae04085e6cd3b7ee29f08c889f507bf766f6575` · 候选版本 `0.1.0` · 关联报告：[209 号 Windows 上线前审查与修复复验](209-8042f91新增功能与Windows上线前审查.md)

**本轮结论（2026-11-27）**　对 `eae0408` 的全部 17 项编号修复、旧库迁移及 GitHub Latest 规则完成逐项源码逻辑复核，核心逻辑均严密正确。本机 macOS `arm64` 实机验收期间新发现 1 处 macOS 退出补写漏口（**M-F1**：经 Dock 退出或 `NSRunningApplication.terminate()` 直接走 `RunEvent::Exit` 时漏调 `window_state::flush_now`），已在 [lib.rs](../src-tauri/src/lib.rs#L449-L454) 补齐幂等补写并通过实机复测。本机可覆盖的 macOS `arm64` 质量门、旧库迁移、窗口状态、设置竞态、无障碍标签与内部 DMG 验签全部通过；非本机环境项目（Universal 双切片 / Intel 原生机 / Apple 公证 / 外接副屏 / 真实线上 Release）已整理至第六节待专机验证清单。

---

## 一、环境与基线核对

| 项目 | 实测值 |
| --- | --- |
| 复核基线 | `eae04085e6cd3b7ee29f08c889f507bf766f6575`（`fix: 防止 Windows 卸载误删数据，修复旧库搬迁、窗口状态与设置竞态`） |
| 操作系统与架构 | macOS `27.2.1`（`26B131`），`arm64`（单块 Retina 2x 显示器，逻辑 `1408×881`，左侧 Dock 宽 `53pt`） |
| 工具链 | Node `v24.22.3` · pnpm `12.23.0` · Rust `1.100.0`（Homebrew，仅含 `aarch64-apple-darwin` 目标） · FFmpeg `9.0.2` |
| 隔离与数据保护 | 全部 macOS 实机验收均通过独立临时 `HOME`、`HIVECAST_DATA_DIR`、`HIVECAST_CACHE_DIR`、`HIVECAST_LOG_DIR` 运行；未触碰本机 `~/Library/Application Support/com.hivecast.desktop/hivecast.db`，测试结束后无残留 `hivecast` 进程 |

---

## 二、`eae0408` 修复逐项逻辑复核

Windows 侧已标记通过的修复项按源码核对跨平台逻辑与边界条件，结果如下：

| 对应原项 | 源码位置与逻辑核对结论 | 判定 |
| --- | --- | --- |
| **P1-1 · 损坏安装包误入卸载** | [installer-win/src/install.rs](../src-tauri/installer-win/src/install.rs#L40-L65)、[payload.rs](../src-tauri/installer-win/src/payload.rs)：仅当无载荷且为正式卸载体（文件名 + 专用尾标 `UNINSTALL_TRAILER`）时进入卸载；载荷缺失/截断或改名副本显式报错返回，不再误触卸载路径。 | 逻辑通过 |
| **P1-2 / P3-1 · 目录归属认领与手输提示** | [installer-win/src/install.rs](../src-tauri/installer-win/src/install.rs#L217-L333)、[win_os.rs](../src-tauri/installer-win/src/win_os.rs#L63-L109)、[gui.rs](../src-tauri/installer-win/src/gui.rs#L206-L258)、[ui.html](../src-tauri/installer-win/src/ui.html#L1000-L1067)：通过 PE `VERSIONINFO`（`ProductName` + `FileDescription`）、安装回执 SHA-256（`.hivecast-install`）及产品数据库四表结构校验归属，拒绝普通 `data/` 与无身份同名 EXE，绿色版与其他文件混放时拒绝覆盖；手输目录防抖 150ms 触发 `inspect` 同步更新按钮与完成页状态。 | 逻辑通过 |
| **P1-3 / P2-1 · 卸载录像保护与文件占用恢复** | [settings.rs](../src-tauri/src/settings.rs#L327-L338)、[cleanup.rs](../src-tauri/installer-win/src/cleanup.rs#L36-L145)、[session_cache.rs](../src-tauri/src/session_cache.rs)、[state.rs](../src-tauri/src/state.rs#L272-L298)：保存媒体目录时阻止与 `cache/logs/webview/data` 重叠；卸载核对 `media.root`、历史 `media_root_history`、会话四类路径与 `media_file`（遇到未迁移的相对路径时 fail-closed 中止卸载，宁可保留也不误删）；运行期缓存与历史录像重叠时自动切临时目录；先清理运行文件再删主程序，文件被占时保留原程序和登记支持解除占用后原位修复。 | 逻辑通过 |
| **P1-4 · 卸载清配置误删旧档案** | [install_receipt.rs](../src-tauri/src/install_receipt.rs)、[cleanup.rs](../src-tauri/installer-win/src/cleanup.rs#L300-L394)：安装回执记录 `local` / `system` / `external` 档案类别；清理系统库时额外校验 `install.system_profile_owner` 与当前安装路径一致，且只按白名单成组原子暂存并移除本产品 DB/密钥文件（回滚保护），`EBWebView` 需同时带创建标记且不含未知用户文件才清理。 | 逻辑通过 |
| **P1-5 / 历史兼容 · 旧 Windows 布局与旧库迁移** | [paths.rs](../src-tauri/src/paths.rs#L92-L134)、[db.rs](../src-tauri/src/db.rs#L304-L350)：启动选库优先沿用已有用户数据的旧系统库（若旧版已误建空便携库则自动切回有数据的系统库，保留两份文件）；恢复旧表缺列补齐、`media_file` 回填、多片段首片段 `group_id` 回填、`task_event_fts` 回填（失败安全降级 LIKE）、历史明文 Cookie 就地 `enc:v1:` 加密迁移。已在 macOS 实机复测旧库迁移闭环。 | 逻辑通过（macOS 实测通过） |
| **P1-6 · 便携搬迁路径同步** | [paths.rs](../src-tauri/src/paths.rs#L151-L168)、[db.rs](../src-tauri/src/db.rs#L2480-L2580)：随库记录 `install.portable_root` 并在启动恢复前按路径组件同步重写 `media.root`、`log.dir`、`media_root_history`、会话路径、`media_file` 及 `.split.json` 清单；外部独立媒体盘保持原值；无标记旧库仅在目标 `Recording/` 存在对应文件时才推断搬迁。 | 逻辑通过 |
| **P1-7 / P2-6 / P2-7 · 窗口死锁、快速最大化与写库补写** | [window_state.rs](../src-tauri/src/window_state.rs#L440-L593)：事件回调中同步采样，后台防抖协程仅执行 `persist_if_changed`（彻底断开锁内调用窗口 API 的互等路径）；Windows 增补 `GetWindowPlacement` 过滤最大化过渡帧；仅在 `save()` 成功返回 `true` 时推进 `last_persisted`。**复核发现 macOS 退出路径漏口（见第三节 M-F1），已修复。** | 修复 M-F1 后通过（macOS 实测通过） |
| **P2-2 / P2-8 · 单实例错误区分与网络映射盘识别** | [lib.rs](../src-tauri/src/lib.rs#L70-L83)、[paths.rs](../src-tauri/src/paths.rs#L136-L149)：仅 `ErrorKind::WouldBlock` 视为单实例竞争，目录/权限故障明确报错提示；Windows 调用 `GetDriveTypeW` 将 `DRIVE_UNKNOWN` / `DRIVE_NO_ROOT_DIR` / `DRIVE_REMOTE` 统一视为远程卷并回落本地档案。 | 逻辑通过 |
| **P2-3 · 即时开关与普通保存交错** | [settings.rs](../src-tauri/src/settings.rs#L346-L382)、[commands.rs](../src-tauri/src/commands.rs#L3201-L3210)、[ipc.ts](../src/lib/ipc.ts#L419-L441)、[SettingsView.tsx](../src/features/settings/SettingsView.tsx#L465-L520)、[App.tsx](../src/App.tsx#L153-L325)：即时偏好（`closeAction` / `preventSleep` / `theme` / `language`）走专用原子命令 `settings_preferences`，绕过无关工具路径校验；普通保存与撤销改按差量（`Partial<Settings>`）进入同一 FIFO 写队列合并。已在 macOS 实机复测闭环。 | 逻辑通过（macOS 实测通过） |
| **P2-4 / P2-5 · Cookie 弹窗代次与续期互斥** | [AccountSection.tsx](../src/features/settings/AccountSection.tsx#L164-L426)：引入 `cookieSeq` 代次在打开/关闭/切标签/卸载时递增，迟到响应仅在需要时刷新列表而不关闭新弹窗或清空草稿；`checkRefresh` 与 `checkAllRefreshable` 互斥，批量按钮在任意单项检查中禁用，避免跳过项被虚报为成功。 | 逻辑通过 |
| **P3-2 · 设置帮助按钮无障碍名称** | [InfoTip.tsx](../src/components/ui/InfoTip.tsx#L6-L14)、[SettingsView.tsx](../src/features/settings/SettingsView.tsx#L276-L315)：`Row` 与 `Section` 向 `InfoTip` 传入字段名 `field`，渲染 `{{field}}说明` / `About {{field}}`。已在 macOS Accessibility 树中实测读取全部 9 个独立字段名。 | 逻辑通过（macOS 实测通过） |
| **GitHub Latest 稳定版选择** | [generate-release-notes.mjs](../scripts/ci/generate-release-notes.mjs#L70-L79)、[build-release.yml](../.github/workflows/build-release.yml#L319-L388)：发布作业加并发锁并分页拉全量 Release，由 `shouldMarkLatest` 按稳定 SemVer 判定 `--latest=true/false`，排除草稿、预发布和维护回补旧分支；Node 回归 6/6 通过。 | 逻辑通过 |

---

## 三、本轮 macOS 复核新发现与修复（M-F1）

| 编号 | 现象与根因 | 修复实现 | 实机复测结果 |
| --- | --- | --- | --- |
| **M-F1 · P2** macOS 经 Dock 退出或 `terminate()` 跳过 `window_state::flush_now` | macOS 上通过 Dock 右键退出、`NSRunningApplication.terminate()`（含 `agent-desktop close-app`）或系统注销退出时，Tauri 直接触发 `RunEvent::Exit` 而不经过 `RunEvent::ExitRequested`。实测用 SQLite 触发器拦截防抖写入后解除限制并直接 `close-app`，日志直接打印 `应用开始退出收尾`，未执行 `flush_now`，库中窗口尺寸仍停留在旧值 `1195×700`（丢失新值 `1255×745`）。 | [src-tauri/src/lib.rs](../src-tauri/src/lib.rs#L449-L454)：在 `RunEvent::Exit` 入口补调 `crate::window_state::flush_now(handle)`。`flush_now` 内部按 `last_persisted != Some(current)` 幂等提交，若此前已在 `ExitRequested` 或 `quit_now` 落盘则为空操作；窗口已隐藏/销毁时仅落盘已采样的脏快照。 | **macOS 实机复测通过**：SQLite 触发器拦截期间调整至 `1255×745`（库中保持 `1195×700`）→ 删除触发器 → 不再拖动直接 `close-app` → 库中立即补写为 `1255×745` → 重启实窗精确还原为 `1255×745`。<br>**Windows 11 x64 回归通过**：`cargo fmt --check`、全目标严格 `clippy` 0 警告、`window_state` 11/11、`installer-win` 11/11 全过。 |

---

## 四、macOS arm64 质量门与产物校验

| 检查项 | 最终结果 |
| --- | --- |
| 前端与静态校验 | `pnpm build` 通过（862ms）；`pnpm check:i18n` 通过（1085 键中英一致）；`node --test scripts/ci/release-regression.test.mjs` 6/6 通过；`pnpm check:docs` 通过 |
| Rust 格式与 Clippy | 主 crate 与 `installer-win` 的 `cargo fmt --check`、`cargo clippy --all-targets --locked -- -D warnings` 均 0 警告通过 |
| Rust 单元测试 | 主库 `cargo test --locked --lib`：**516 passed / 0 failed / 29 ignored**（含 `window_state` 11/11、`settings` 19/19）；从忽略项选跑 `local_hls_lifecycle`、`ffmpeg_pipeline_on_synthetic_clips`、`real_ffmpeg_source_convert_and_custom_are_playable`：**3/3 通过**；`hivecast-ipc`：**67/67 通过**；`installer-win`：**11/11 通过** |
| 构建产物（含 M-F1 修复） | `HIVECAST.app`：`35.69 MiB`（`lipo -archs` 为 `arm64`），主程序 SHA-256 `6ac8f59b4212bfcd5ef29051a2a14ae1dfc61221144c1b4ffe2af45547a6a966`；<br>`HIVECAST_0.1.0_arm64.dmg`：`17 MiB`，SHA-256 `32d434d0f819e9560985057162be98e29fddecbb0738e4363c41b5ed91a650a2` |
| DMG 与内部签名校验 | `hdiutil verify` 返回 `VALID`；只读挂载后目录包含 `HIVECAST.app`、`Applications -> /Applications`、`.background/bg.tiff`（840,404 B）；`codesign --verify --deep --strict --verbose=2` 返回 `valid on disk` / `satisfies its Designated Requirement`；无 Developer ID 证书故 `spctl` 仍为 `rejected`（符合内部 ad-hoc 预期） |

---

## 五、macOS arm64 专机实机验收记录（对照 209 第五、六节）

| 验收项 | 实机操作与证据（隔离环境运行真实 `HIVECAST.app`） | 结果 |
| --- | --- | --- |
| **默认与自定义媒体目录** | ① 空白隔离 `HOME` 首次启动：自动创建 `<HOME>/Movies/HIVECAST`，确认 `HIVECAST.app` 包内无 `Recording/` 残留；② 预置自定义 `media.root` 启动后原样保留，未改写或回退。 | 通过 |
| **旧格式数据库完整迁移** | 构造含明文 Cookie（`SESSDATA=plain_legacy`）、多片段旧相对路径（`s1: bilibili/part1.mp4, group_id=NULL`、`s2: bilibili/part2.mp4, group_id='s1'`）、未索引 `task_event`（`legacyMarker209`）的旧库并启动真实 `HIVECAST.app`：启动后 `credential.cookie` 转为 `enc:v1:` 且生成 `secret.bin`；`s1.file_path` 重写为自定义媒体根下的绝对路径且 `s1.group_id` 回填为 `'s1'`；`media_file` 回填 2 条绝对路径登记；`room.last_session_id` 回填为 `'s1'`；`db.task_event_fts_v1` 置为 `'1'`（FTS 检索由主库单测与 SQLite 触发器同步保障）。 | 通过 |
| **窗口快速最大化（P2-6）** | Retina 2x 屏幕下将窗口调至 `1240×740 @ (96, 60)`，仅间隔 **120ms**（小于 350ms 防抖期）即执行最大化至 `1356×851`：库内 `ui.window_state` 准确记录 `width=1240, height=740, maximized=true`（未丢失正常尺寸）；退出重启后以最大化（`1355×851`）恢复且正常尺寸保持不变。 | 通过 |
| **临时写库失败后退出补写（P2-7 + M-F1）** | 正常落盘 `1195×700` 后挂 SQLite `BEFORE UPDATE/INSERT` 触发器阻断 `ui.window_state` 写入，调整窗口至 `1255×745` 并等待防抖失败（日志确认写入报错、库中仍为 `1195×700`）；删除触发器后不再拖动直接 `close-app`：经 `RunEvent::Exit` 补写成功落盘 `1255×745`，重启实窗精确还原为 `1255×745`。 | 通过 |
| **关闭隐藏后台与 Dock 唤回** | 保持后台运行（`tray`）模式下将窗口改为 `1220×720` 后 100ms 内点击原生红灯关闭按钮（`AXCloseButton`）：窗口隐藏（`list-windows` 为空）、进程保持存活（PID `87006`）、库中立即补写 `1220×720`；随后执行 `open HIVECAST.app` 触发 `RunEvent::Reopen`，同一进程原窗口立即回到前台并保持 `1220×720`。 | 通过 |
| **设置竞态（P2-3）** | 数据库预置失效 `ffmpeg.path = /nonexistent/bad-ffmpeg` 后实窗进入设置：在系统页点击「直接退出程序」、开启「阻止系统休眠」并点击顶栏「切换到深色模式」，数据库原子更新为 `app.close_action=quit`、`app.prevent_sleep=true`、`ui.theme=dark`，原失效 `ffmpeg.path` 保持未篡改、无保存失败拦截。 | 通过 |
| **无障碍字段说明名称（P3-2）** | macOS Accessibility 树（VoiceOver 底层数据源）读取存储页全部 9 个帮助按钮，名称依次为：`媒体库目录说明`、`自动清理旧录像说明`、`删除文件先移入回收站说明`、`文件名模板说明`、`去掉 emoji 与特殊符号说明`、`按平台与主播分目录说明`、`日志目录说明`、`日志保留说明`、`ffmpeg 路径说明`，9/9 互不重复。 | 通过 |

---

## 六、非当前平台环境待专机验证需求清单

以下项目受限于当前本机硬件、证书或线上发布前提，本轮未在本机实测，留待对应环境专机或正式发布时逐项勾验并补录结果：

| 编号 | 所需环境 | 验证目标与操作步骤 | 通过标准 |
| --- | --- | --- | --- |
| **V-1 · macOS Universal 双架构构建与验证** | GitHub Actions `macos-15`（或已安装 `rustup` 双目标 `aarch64-apple-darwin` + `x86_64-apple-darwin` 的 macOS） | 按 [build-release.yml](../.github/workflows/build-release.yml) 构建 `universal-apple-darwin` 与 DMG，记录文件名和 SHA-256；挂载后执行 `hdiutil verify`、`lipo -archs HIVECAST.app/Contents/MacOS/HIVECAST`、`codesign --verify --deep --strict`。 | `lipo -archs` 同时包含 `x86_64` 与 `arm64`；DMG 校验和与验签通过。 |
| **V-2 · Intel macOS 原生运行** | Intel x86_64 物理 Mac（若仅在 Apple Silicon 上用 Rosetta 运行须单独标注为转译覆盖） | 使用同一 Universal DMG 拖入 `/Applications` 后启动，检查「关于」页架构展示、FFmpeg 探测、短录制 10–30 秒及本地回放、窗口缩放与重启记忆。 | 架构显示 `x86_64`，录制与回放正常，窗口记忆与退出无异常。 |
| **V-3 · Apple 公证与下载隔离首开** | 具备 `Developer ID Application` 证书与 Notary 凭据的发布环境 | 开启 `sign_macos=true` 构建并公证 DMG；验证 `xcrun stapler validate` 与 `spctl --assess --type execute --verbose=4`；给 DMG 添加 `com.apple.quarantine` 后模拟浏览器下载、拖入应用程序并首次打开。 | Gatekeeper 放行（`accepted`），无“已损坏/无法验证开发者”拦截，正常首开并退出。 |
| **V-4 · macOS 多显示器异缩放与小工作区** | 连接外接显示器（与内屏不同缩放比例，且支持配置在主屏左侧形成负坐标）的 Mac | ① 将窗口拖至外屏（含左侧负坐标）调整大小并快速最大化，退出重启核对恢复位置与还原后尺寸；② 断开外屏后重启，核对自动回落主屏居中；③ 在逻辑尺寸小于 `1180×680` 的工作区核对控件可达性。 | 跨屏位置和尺寸无翻倍/缩水，断屏自动回落主屏居中，操作控件不越界。 |
| **V-5 · macOS VoiceOver 与双语/双主题人工走查** | 任意 macOS 实机（开启 VoiceOver） | 在中文/英文、浅色/深色主题下，用 `Tab` 逐个遍历设置页各 `ⓘ` 按钮并按 `Esc` 关闭提示，听取 VoiceOver 朗读名称与正文。 | 每个按钮朗读出独立字段名，提示正文可读，`Esc` 后焦点留在原按钮。 |
| **V-6 · 线上真实 GitHub Release 与三端更新对齐** | 正式发版时（GitHub Actions `publish=true`） | 发布正式版后核对：① GitHub Release 页面 `Latest` 徽标与 SemVer 规则一致（含后续若回补旧分支不抢占 `Latest`）；② GitHub Pages 更新日志站点的资产文案（旧 `arm64` vs 新 `Universal`）、版本索引、筛选与时间轴高亮；③ 旧客户端「关于」页检测更新提示与下载直链。 | GitHub Release、更新日志站点与客户端推荐版本号三端一致，离线状态提示正常。 |
