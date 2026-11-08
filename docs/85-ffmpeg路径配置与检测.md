# 85-ffmpeg 路径配置与检测

> 状态：v1.0 已实施并完成 macOS 真机验收（2026-11-08）；Windows 完成编译级验证，未上真机。
> 范围：ffmpeg 自动检测顺序、手动路径设置、保存校验、状态展示、缺失提示；macOS 与 Windows。
> 前置阅读：`08-录制管线.md`、`09-弹幕子系统.md`、`78-录制后处理实现与验收.md`。
> 说明：本文描述当前代码，关键实现以源码为准。

## 0. 结论速览

- **原检测并非完全不兼容 macOS / Windows**：`ffmpeg.rs` 一直按 `cfg(windows)` 切换可执行文件名与候选目录，macOS 还额外补了 Homebrew 的 `/opt/homebrew/bin`。真正缺的是**用户可配置路径**、**统一状态**和**缺失提示**。
- **本次新增优先级**：手动配置 > 系统 `PATH` > 平台常见安装目录。手动配置为空串时完全回到自动检测。
- **保存时校验**：必须是绝对路径；允许选可执行文件或它所在目录；文件必须存在、能成功执行 `ffmpeg -version`；成功后存用户选择的绝对路径，不解析符号链接。
- **状态可见**：设置 → 存储 → 工具 显示实际使用的路径、来源（手动 / 系统 PATH / 常见目录 / 未找到）与版本；配置失效会显示「已回退自动检测」。
- **缺失提示**：应用启动和每次保存设置后探测一次；没有可用 ffmpeg 时，顶栏下方出现窄横幅，点「去指定路径」直接打开 设置 → 存储。
- **不做**：不自动下载 ffmpeg，不做全盘扫描，不强制最低版本，不替换 ffprobe 的独立配置。

## 1. 检测机制

### 1.1 解析顺序

`src-tauri/src/ffmpeg.rs` 的 `resolve_with_source()` 按固定顺序返回第一个能跑通的 `ffmpeg`：

| 顺序 | 来源 | 位置 | 说明 |
| --- | --- | --- | --- |
| 1 | 手动配置 | `ffmpeg.path` 设置项 | 存用户选择的绝对路径（不解析符号链接）；启动时先于后处理恢复写入；保存设置后立即更新 |
| 2 | 系统 `PATH` | 进程环境变量 | 按 `std::env::split_paths` 逐项拼 `ffmpeg(.exe)` |
| 3 | 平台候选目录 | 代码内置 | macOS：`/opt/homebrew/bin`、`/usr/local/bin`、`/usr/bin`；Windows：`C:\\ffmpeg\\bin`、`C:\\Program Files\\ffmpeg\\bin`；其他：`/usr/bin`、`/usr/local/bin` |

「能跑通」= 文件存在且 `ffmpeg -version` 退出码为 0（`is_runnable`）。因此一个同名但不是 ffmpeg 的文件不会误判。

### 1.2 macOS

从 Finder 启动的 GUI 进程通常拿不到用户 shell 的完整 `PATH`，Homebrew 的 `/opt/homebrew/bin` 可能不在其中，所以候选目录是必要的兜底。Intel Mac 的 `/usr/local/bin` 同样覆盖。

### 1.3 Windows

- 可执行文件名是 `ffmpeg.exe`，`ffprobe.exe` 同理。
- `PATH` 覆盖 winget / Scoop / Chocolatey 的 shim，以及用户手工加入的目录。
- 没有安装到 `PATH` 时，再查 `C:\\ffmpeg\\bin` 与 `C:\\Program Files\\ffmpeg\\bin`。
- 不读注册表、不扫描整个磁盘：这两条候选只覆盖最常见的解压式安装。

### 1.4 ffprobe 与运行时调用

- `probe_binary()` 先解析 ffmpeg，再查同目录的 `ffprobe(.exe)`；找不到时后处理退回 ffmpeg 自身校验。
- 所有实际调用点（`recorder.rs`、`postprocess.rs`、`local.rs`、`segment.rs`、`post.rs`）仍调用 `ffmpeg::binary()`。解析成功时返回绝对路径，失败时返回裸名交回 `PATH`，并在日志里打 WARN。

### 1.5 启动时序

`state.rs` 在数据库打开后、`reap_orphans` 与后处理恢复 `spawn` 之前，先执行：

```rust
let mut settings = crate::settings::load(&db);
crate::settings::apply_ffmpeg_path(&settings.ffmpeg_path);
```

这样「上次退出时还有未完成的后处理任务」会使用用户配置的 ffmpeg，而不是先按默认路径跑一遍。之后的 `settings::apply` 会再次写入，保持幂等。

## 2. 设置项

### 2.1 数据模型

| 字段 | 类型 | 默认 | 持久化 key | 说明 |
| --- | --- | --- | --- | --- |
| `ffmpeg_path` | `String` | `""` | `ffmpeg.path` | 空串 = 自动检测；非空 = 手动路径，存用户选择的绝对路径（不解析符号链接） |

IPC 类型在 `src-tauri/ipc-types/src/lib.rs` 的 `Settings` 中；前端绑定由 `pnpm run gen:bindings` 生成。

### 2.2 保存校验

`settings::save()` 在写库前调用 `ffmpeg::validate_selection()`：

1. `trim()` 后为空：直接写空串，恢复自动检测；
2. 非绝对路径：拒绝，提示「请填写 ffmpeg 的绝对路径」；
3. 选的是目录：自动拼 `ffmpeg(.exe)`；
4. 文件不存在：拒绝并回显路径；
5. 不是普通文件：拒绝；
6. 无法启动或 `-version` 非 0：拒绝，带 stderr 首行；
7. 成功：直接写用户选择的绝对路径，**不解析符号链接**。Homebrew 的 `/opt/homebrew/bin/ffmpeg` 必须保持稳定入口；如果写成 `Cellar/9.0.2_1/bin/ffmpeg`，一次 `brew upgrade` 就会让配置失效。

失败统一返回 `AppError::User("ffmpeg 路径不可用：<原因>")`，设置页顶部错误条直接展示。

### 2.3 配置失效时的行为

`set_configured_path()` **不会**因为路径失效就清空配置：

- 设置页仍显示用户填的内容，状态行提示「手动路径不可用，已回退到自动检测」；
- 运行时 `resolve_with_source()` 跳过失效项，继续查 `PATH` 与候选目录；
- 用户把外接盘拔掉、临时移动 ffmpeg 后，重新插回即可恢复，不用重新填写。

这与「保存时校验」并不矛盾：保存时拒绝新写入的坏路径；已经保存的路径事后失效则降级运行并可见。

## 3. 设置界面

位置：`设置 → 存储 → 工具 → ffmpeg 路径`。

- 输入框：可手填绝对路径，占位文案「留空自动检测」；
- 「选择文件…」：调用 `@tauri-apps/plugin-dialog`；macOS / Linux 不过滤扩展名，Windows 只过滤 `.exe`；
- 「恢复自动检测」：把输入框清空，仍需点保存才落库；
- 状态行：实际路径、来源、`ffmpeg -version` 首行（截断 160 字符）；未找到时红字提示。

手动路径与自动检测同时可用时，状态行显示：

```text
实际使用：/opt/homebrew/bin/ffmpeg
来源：系统 PATH · 版本 ffmpeg version 9.0.2 ...
```

配置失效但自动检测成功时，额外显示一行红字：

```text
手动路径 /bad/path/ffmpeg 不可用，已回退到自动检测。请检查后重新保存。
```

## 4. 缺失提示

`App.tsx` 在启动时与设置保存成功后调用 `ffmpeg_status()`；当 `path === null` 时，在顶栏下方渲染一条 32px 高的警告横幅：

- 文案：「未找到 ffmpeg，录制、转封装与观看都会受影响。」
- 操作：「去指定路径」→ 打开设置并直接落到「存储」页（`SettingsView` 新增 `initialPage`）；
- 观看页全屏沉浸时隐藏，避免挡画面；退出沉浸后恢复；
- 不在浏览器预览环境请求 IPC（`isTauri()` 守卫）。

只提示，不阻断启动：用户仍可浏览媒体库、管理房间、查看任务；真正需要 ffmpeg 的操作会返回 `ffmpeg_missing`。

## 5. 行为矩阵

| 场景 | 解析结果 | 设置页状态 | 顶栏横幅 |
| --- | --- | --- | --- |
| 手动路径有效 | manual | 实际路径 + 来源手动 | 不显示 |
| 手动路径为空，PATH 有 | path | 实际路径 + 来源 PATH | 不显示 |
| 手动路径为空，PATH 无，候选目录有 | candidate | 实际路径 + 来源候选目录 | 不显示 |
| 手动路径失效，PATH 有 | path | 实际路径 + 来源 PATH + 回退红字 | 不显示 |
| 手动路径失效，全部没有 | 无 | 未找到 + 回退红字 | 显示 |
| 保存时填入坏路径 | 不写库 | 顶部错误条 | 保持原状态 |
| 删除已保存的 ffmpeg 后重启 | 回退或未找到 | 对应状态 | 未找到时显示 |

## 6. 验证

### 6.1 静态与单测

- `cargo fmt --check`、`cargo check --all-targets`：0 警告；
- `cargo test --lib`：249 passed / 0 failed / 17 ignored；
- 新增 `selection_keeps_executable_and_expands_directory`、`relative_paths_are_rejected`；
- `pnpm exec tsc -b`、`pnpm run check:i18n`（615 keys）、`pnpm run check:docs`（86 files）、`pnpm build` 通过。

### 6.2 macOS 真机

验收路径：设置 → 存储。

1. 自动检测显示 `/opt/homebrew/bin/ffmpeg`，来源「系统 PATH」，版本 `ffmpeg version 9.0.2`；
2. 填入不存在的 `/tmp/not-ffmpeg` 保存：保存被拒绝，顶部错误条显示后端原因；
3. 填入 `/opt/homebrew/bin`（目录）保存：接受并扩展为 `/opt/homebrew/bin/ffmpeg`，来源变为「手动指定」；数据库里存的是符号链接路径，不是 `Cellar/9.0.2_1/bin/ffmpeg`，`brew upgrade` 后不会失效；
4. 重启应用后仍为手动指定，说明 `ffmpeg.path` 已持久化且启动时序生效；
5. 退出应用、清空设置、临时 `mv` 走 `/opt/homebrew/bin/ffmpeg` 后重新启动：顶栏出现缺失横幅；点「去指定路径」直接进入「设置 → 存储」，状态行显示「未检测到可用 ffmpeg」，输入框为空；恢复 ffmpeg 后重启，横幅消失。

### 6.3 Windows

本次只做编译级验证：`cargo check --all-targets` 通过，`cfg(windows)` 分支使用 `ffmpeg.exe` / `ffprobe.exe` 与两个候选目录。仍需在 Windows 真机补：自动检测、选择 `.exe`、路径含空格/中文、保存后重启、缺失横幅。

## 7. 已知限制

- **不自动下载 ffmpeg**：安装仍是用户责任，应用只负责发现与提示。
- **不校验最低版本**：只要 `-version` 成功就接受；过老的 ffmpeg 可能缺 `-progress`、`hevc` 等能力，要到实际任务才暴露。
- **Windows 未真机验收**：注册表安装、带空格路径、中文用户名尚未实跑。
- **ffprobe 不可单独配置**：固定取 ffmpeg 同目录；某些发行包把两者拆开时，会退回 ffmpeg 校验。
- **保存设置会整体校验**：手动路径事后失效且用户未改它时，保存其它设置也会被同一条 ffmpeg 校验拦下；先「恢复自动检测」或重新指定即可。这是沿用媒体目录、文件名模板等字段的既有保存语义。
- **状态不是实时推送**：只在启动、保存设置、打开设置页时刷新；进程运行期间外部替换 ffmpeg 不会主动改变横幅。
