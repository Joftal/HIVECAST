# 13 · IPC 与前端接口

> 参考实现：`bililive-go/src/servers/server.go`（396 行，gorilla/mux 路由表）、`handler.go`（4392 行）、`docs/API.md`、`src/webapp/src/utils/{api,sse}.ts`。
> 参考项目是"浏览器 ↔ HTTP 服务"，桌面端是"WebView ↔ Rust 核心"。协议不同，但**语义可以一一对应**。

## 1. 两种通信形态

```text
┌─────────────── WebView (React) ───────────────┐
│  invoke("list_rooms", {...})  ──请求/响应──┐   │
│  listen("room://updated", cb) ◀──事件推送──┘   │
└───────────────────────────────────────────────┘
                    │ Tauri IPC
┌───────────────────▼───────────────────────────┐
│  Rust 核心：命令层 → 服务层 → 领域层            │
└───────────────────────────────────────────────┘
```

| 形态 | 用途 | 对应 HTTP 版 |
| --- | --- | --- |
| Command（invoke） | 所有"用户主动发起"的操作 | `GET/POST/PUT/DELETE` |
| Event（emit） | 所有"服务端主动"的推送 | SSE `/api/sse` |

**不要**在桌面端再起一个本地 HTTP 服务给前端用——多一层序列化、多一个端口冲突风险。除非需要被第三方集成。

## 2. 命令命名规范

```text
<资源>_<动作>
```

| 命令 | 参数 | 返回 |
| --- | --- | --- |
| `rooms_list` | `{filter?, page?, page_size?}` | `{items, total}` |
| `rooms_get` | `{live_id}` | `Room` |
| `rooms_add` | `{url, options?}` | `Room` |
| `rooms_remove` | `{live_id}` | `void` |
| `rooms_update` | `{live_id, patch}` | `Room` |
| `rooms_start_listen` | `{live_id}` | `void` |
| `rooms_stop_listen` | `{live_id}` | `void` |
| `rooms_start_record` | `{live_id}` | `void` |
| `rooms_stop_record` | `{live_id}` | `void` |
| `rooms_switch_stream` | `{live_id, candidate_id}` | `void` |
| `streams_list` | `{live_id}` | `StreamCandidate[]` |
| `sessions_list` | `{live_id?, page?, page_size?}` | `{items,total}` |
| `files_list` | `{session_id?, status?, page?}` | `{items,total}` |
| `files_delete` | `{ids[]}` | `{deleted, failed[]}` |
| `files_reveal` | `{id}` | `void`（打开访达/资源管理器） |
| `tasks_list` | `{status?, page?}` | `{items,total}` |
| `tasks_retry` | `{id, from_stage?}` | `void` |
| `tasks_cancel` | `{id}` | `void` |
| `config_get` | `{}` | `{config, version}` |
| `config_update` | `{patch, version}` | `{config, version}` |
| `config_raw_get` | `{}` | `{yaml, version}` |
| `config_raw_update` | `{yaml, version}` | `{version}` |
| `config_preview_template` | `{template, sample}` | `{result, error?}` |
| `accounts_list` | `{}` | `Account[]` |
| `accounts_qr_start` | `{platform}` | `{qrcode_url, key}` |
| `accounts_qr_poll` | `{platform, key}` | `{status, account?}` |
| `accounts_remove` | `{id}` | `void` |
| `app_info` | `{}` | `{version, platform, ffmpeg}` |
| `logs_tail` | `{live_id?, lines}` | `string[]` |
| `tools_status` | `{}` | `{ffmpeg, bt_tools, openlist}` |

## 3. 统一返回与错误

```rust
#[derive(Serialize)]
struct ApiError {
    code: String,          // "room_not_found"
    message: String,       // 面向用户的中文
    detail: Option<String>,// 面向开发者的原始错误
}

// Tauri 命令签名
#[tauri::command]
async fn rooms_add(url: String) -> Result<Room, ApiError>;
```

**错误码约定**（前端按 code 分支，不解析 message）：

| code | HTTP 类比 | 前端处理 |
| --- | --- | --- |
| `invalid_argument` | 400 | 表单标红 |
| `not_found` | 404 | 提示 + 刷新列表 |
| `conflict` | 409 | "配置已被修改，请刷新" |
| `unauthorized` | 401 | 弹登录 |
| `rate_limited` | 429 | 提示稍后 |
| `tool_missing` | 503 | 提示安装/启动工具 |
| `internal` | 500 | 展示 detail + 上报 |

> 参考项目用 `{err_no, err_msg, data}` 包一层。桌面端用 `Result` 更自然，但要保证**前端永远能拿到结构化错误**，而不是字符串。

## 4. 事件设计

### 4.1 命名

```text
<domain>://<action>
```

| 事件 | 载荷 | 频率 |
| --- | --- | --- |
| `room://updated` | `Room` | 状态变化时 |
| `room://added` / `room://removed` | `{live_id}` | 低频 |
| `recording://started` | `{live_id, session_id, file_path}` | 低频 |
| `recording://progress` | `{live_id, duration_ms, size, bitrate, health}` | **每 2s** |
| `recording://ended` | `{live_id, session_id, reason}` | 低频 |
| `danmaku://message` | `DanmakuMessage` | 高频（需节流） |
| `task://updated` | `PipelineTask` | 阶段变化时 |
| `config://updated` | `{version}` | 低频 |
| `log://line` | `{level, target, message, ts}` | 中频（可开关） |
| `tool://status` | `{name, ready, version}` | 低频 |

### 4.2 高频事件的处理（关键）

弹幕和录制进度是高频的。直接逐条 `emit` 会让 WebView 卡死（Tauri 的 IPC 序列化 + React 重渲染）。

三条规则：

1. **合并**：进度类事件每 2s 合并一次（只发最新值），不要每个 tick 都发。
2. **批量**：弹幕每 200ms 打包成数组发一次，前端批量入 state。
3. **订阅制**：前端"打开弹幕面板"才订阅 `danmaku://message`，关闭时 `unlisten`。后端根据订阅者数量决定是否推送。

```rust
// 后端：只有存在订阅者时才做序列化
if !emitter.has_listeners("danmaku://message") { return; }
```

### 4.3 事件与查询的一致性

事件只发"变化"，前端维护本地状态：

```text
首次加载：invoke("rooms_list") → 全量
后续：listen("room://updated") → 局部更新
断线重连/页面刷新：重新全量拉取
```

**必须提供"全量刷新"入口**，否则事件丢一条就永久不一致。

## 5. 与参考项目 API 的映射

| bililive-go | HIVECAST 命令 | 说明 |
| --- | --- | --- |
| `GET /api/lives` | `rooms_list` | 数组 → 分页对象 |
| `POST /api/lives` | `rooms_add` | 支持批量 |
| `DELETE /api/lives/{id}` | `rooms_remove` | |
| `GET /api/lives/{id}/start` | `rooms_start_record` | 原 API 用 GET 触发副作用，桌面端改命令 |
| `GET /api/lives/{id}/stop` | `rooms_stop_record` | |
| `GET /api/lives/{id}/switchStream` | `rooms_switch_stream` | |
| `GET/PUT /api/config` | `config_get/update` | 加 version |
| `GET/PUT /api/raw-config` | `config_raw_get/update` | |
| `GET /api/sse` | 多个 `//` 事件 | 拆细，前端按需订阅 |
| `GET /api/file/{path}` | `files_*` + asset 协议 | 文件读走 Tauri asset 协议，不走 IPC |
| `GET /api/metrics` | `app_metrics` | 可选 |
| `GET /api/info` | `app_info` | |

### 5.1 文件访问

播放/缩略图**不要**通过 IPC 传字节。用 Tauri 的 asset 协议：

```rust
tauri::Builder::default()
    .asset_protocol_scope()
    .allow_directory(&output_dir, true)
```

前端：`<video src="asset://localhost/.../xxx.mp4">`。

- 只允许输出目录（不要 `allow_directory("/")`）。
- 大文件播放靠 Range 请求，asset 协议原生支持。

## 6. 状态推送的可靠性

| 风险 | 对策 |
| --- | --- |
| WebView 未就绪时发事件 | 前端就绪后主动全量拉取，事件只做增量 |
| 事件丢失 | 定期（30s）对账：拉一次摘要，与本地比对 |
| 窗口最小化被节流 | 后端不依赖前端 ACK，状态以 DB 为准 |
| 多窗口 | 每个窗口独立订阅；后端广播到所有窗口 |

## 7. 日志与诊断

```rust
// 内存环形缓冲，保留最近 5000 行
struct LogBuffer { ring: Mutex<VecDeque<LogLine>> }

#[tauri::command]
fn logs_tail(live_id: Option<String>, lines: usize) -> Vec<LogLine>;
```

- UI 的"日志"面板拉取 + 订阅增量。
- 每个房间的日志带 `live_id` 标签（参考项目的 `LiveLogger` 按 roomID 绑定）。
- 提供"导出诊断包"：日志 + 配置（脱敏）+ 系统信息，方便用户反馈问题。

## 8. 安全

| 风险 | 对策 |
| --- | --- |
| 任意文件读取 | asset 协议限定目录；命令层校验路径必须在输出目录内 |
| 命令注入 | 自定义命令模板必须转义（见 10 文档） |
| 敏感数据泄露 | cookie/token 永不下发（见 11 文档 §5） |
| 远程内容 | 禁用 `dangerousRemoteDomainIpcAccess`；不加载远程页面 |
| 更新包篡改 | 签名校验（Tauri updater 内置） |

**路径校验**是桌面端最容易漏的一条：

```rust
fn ensure_in_output_dir(path: &Path) -> Result<()> {
    let canon = path.canonicalize()?;
    let root = output_dir().canonicalize()?;
    if !canon.starts_with(&root) { return Err(ApiError::invalid_argument("路径越界")); }
    Ok(())
}
```

## 9. 前端数据层建议

```text
src/
  lib/
    ipc.ts          // invoke 的类型化封装，统一错误处理
    events.ts       // listen 的订阅管理（自动 unlisten）
    queries/        // TanStack Query：缓存 + 失效 + 轮询兜底
      rooms.ts
      sessions.ts
      tasks.ts
```

推荐 **TanStack Query**：

- `useQuery` 负责全量拉取与缓存；
- 事件到达时 `queryClient.setQueryData` 做局部更新；
- `invalidateQueries` 做兜底刷新；
- 自带重试、去重、窗口聚焦刷新。

这样"事件驱动 + 轮询兜底"两套机制可以共存而不冲突。

## 10. 反模式

1. ❌ 每条弹幕一个事件 → WebView 卡死。
2. ❌ 用 HTTP 本地服务代替 IPC → 端口冲突、多一层。
3. ❌ 文件内容走 IPC → 大文件卡死，改用 asset 协议。
4. ❌ 只靠事件不同步全量 → 丢一条就永久错。
5. ❌ 错误只返回字符串 → 前端无法分支处理。
6. ❌ asset 协议放开整个磁盘 → 任意文件读取漏洞。
7. ❌ 后端假设前端一定在监听 → 无订阅者时白白序列化。
8. ❌ 命令无鉴权（多窗口场景） → 任意窗口都能改配置（桌面端可接受，但要知晓）。
