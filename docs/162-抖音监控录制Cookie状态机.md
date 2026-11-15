# 162 抖音监控录制与 Cookie 状态机

- 日期：2026-11-15
- 范围：抖音房间在无 Cookie、有效 Cookie、中途补 Cookie、Cookie 失效、自动轮换、更新写回、退出登录、断流重连、应用重启等状态下的「监控 → 录制 → 弹幕」行为
- 环境：macOS 14，release bundle 32.03 MiB，真实抖音账号 uid 67305272492「执事只食芝士」；测试房间 `https://live.douyin.com/547977714661`（小央视频 / 央视新闻频道）
- 结论：**已启动的录制进程与 Cookie 解耦。Cookie 自动轮换、写回新 Cookie、账号失效、退出登录都不会停止或重启当前 ffmpeg；只有 ffmpeg 因断流/下播退出后的续录，才会用当时的 Cookie 重新取流。** 本轮修复了「过期账号仍执行 Cookie 轮换，把内存状态洗回 ok / 写出登录态有效」的状态机缺陷，并补了单测。

## 1. 状态模型

| 维度 | 状态 | 对监控/录制的影响 |
| --- | --- | --- |
| 凭据 | 无 Cookie | 每次取流走匿名 ttwid；监控、录制、弹幕都可用，画质按游客档 |
| 凭据 | `ok` + 有效 Cookie | `cookie_for_url` 注入平台请求；弹幕 WS 带完整 Cookie，`authed=true` |
| 凭据 | `expired` | `cookie_for_url` 返回 `None`，不再注入失效 Cookie；取流自动回退匿名 |
| 凭据 | 轮换中 | 只影响凭据表；活动 ffmpeg 仍用启动时的签名 URL |
| 凭据 | 写入新 Cookie | 清空平台取流缓存；活动 ffmpeg 不重启，下一次续录才使用新 Cookie |
| 录制 | `recording` | 固定输入 URL 的 ffmpeg 子进程；Cookie 变化不触碰进程 |
| 录制 | `respawning` | 仅在上一次 ffmpeg 退出后发生；重新调用 `plan_for_respawn` 取当前凭据的流 |
| 弹幕 | 已连接 | 连接建立时快照 Cookie；之后 Cookie 变化不主动重连，断线重连才换身份 |

四条不变量：

1. 监控在播自动开录前有硬闸 `!recorder.is_recording(&room.id)`，不会因为一次房间信息返回就重复开录。
2. 录制进程的输入 URL 在 `spawn` 时固定，Cookie 轮换不修改运行中进程的参数。
3. `clear_stream_cache` / `clear_groups` 只清候选地址缓存，不调用 `recorder.stop`。
4. 保存 Cookie 后触发的画质重检是**独立的质量升级动作**；只有目标档高于当前档时才会主动 `respawn`，这与 Cookie 轮换本身无关。

## 2. 场景矩阵

| # | 场景 | 实际行为 | 录制是否中断 |
| --- | --- | --- | --- |
| 1 | 无 Cookie 首次监控到开播 | `platform/douyin.rs:109 enter` 读不到凭据，走 `anon_ttwid()` 注册/复用 ttwid；房间信息、FLV 取流、弹幕 WS 全部可用；自动开录 | 不适用，正常开录 |
| 2 | 有 Cookie 监控到开播 | 页面请求带保存的 Cookie，再合并页面 Set-Cookie；弹幕 WS `authed=true`；开录使用登录态签名 URL | 不适用，正常开录 |
| 3 | 录制中粘贴并保存新 Cookie | `commands.rs` 保存后调用 `recheck_all_quality(...,"credential_saved")`；同档位不动作；目标档更高才 `respawn_inner`，同 session、分片续号，约 1–5 秒断点 | 只有主动升档时短暂断点，不是 Cookie 轮换导致 |
| 4 | 录制中自动轮换，`changed=0` | 首页 Set-Cookie 与现值相同，不写库、不清缓存；ffmpeg PID 不变 | 否 |
| 5 | 录制中自动轮换，`changed>0` | 写回新 Cookie，`credential::set` 置 `ok`，清空平台取流缓存；活动 ffmpeg 继续用旧 URL | 否 |
| 6 | 录制中 Cookie 失效 | 30 分钟复验探活失败，标记 `expired`、写 `key_stale=true`；当前 ffmpeg 不受影响；下一次续录按匿名取流 | 否，直到 ffmpeg 自己退出 |
| 7 | 失效后 Cookie 又恢复有效 | 复验探活成功，状态恢复 `ok`，发出账号刷新事件，并对活动录制做一次画质重检；必要时同会话升档 | 可能因升档短暂断点 |
| 8 | 录制中退出登录/删除账号 | 删除凭据并清空取流与房间信息缓存；不调用 stop；当前 ffmpeg 与弹幕旧连接继续 | 否 |
| 9 | 录制中断流/主播下播 | watcher 检测 ffmpeg 退出后查询平台状态；仍在播则用**当时**的 Cookie 续录，同一 session、分片续号；已下播则收尾 | 断流本身中断，续录按当前凭据 |
| 10 | 应用重启 | 进程内 ffmpeg 随应用退出；启动恢复把未完成场次标为中断；之后监控重新开新场 | 是，进程生命周期导致，与 Cookie 无关 |
| 11 | 一场录制跨越多次轮换/一次退出登录 | 见第 5 节真机证据：PID 4246 全程不变，`restarts=0` | 否 |

## 3. 代码链路

### 3.1 监控

- `src-tauri/src/monitor.rs:1072 apply_room_info`：写房间状态；在播且未在录时进入自动开录判断。
- `src-tauri/src/monitor.rs:1138`：`&& !recorder.is_recording(&room.id)`，已在录直接跳过。
- `src-tauri/src/monitor.rs:1149`：离线需 `confirm_offline` 连续确认；确认后 `1160` 仍检查 `is_recording`，文件还在增长就继续录，避免主播短暂断流被误停。
- `src-tauri/src/monitor.rs:1181 run_room_action`：Start 只调 `start_checked`，Stop 只调 `stop(...,"stream_ended")`。
- 抖音不在 `monitor.rs:520` 的批量分支内（批量只覆盖 B 站/斗鱼），始终逐房轮询；Cookie 失效只影响下一次房间信息/取流请求。

### 3.2 录制

- `src-tauri/src/recorder.rs:1415 spawn_watcher`：每秒检查子进程；退出后读取平台状态，仍在播则续录。
- `src-tauri/src/recorder.rs:1703 respawn_inner`：重新 `broker.plan_for_respawn`，用当前凭据取流；同一 session 续写、分片继续编号，不重启弹幕。
- `src-tauri/src/recorder.rs:1949 recheck_quality`：只有目标档未达最高且当前 qn 未达标才探测；同容器同编码走 `respawn_inner(...,"quality_upgraded", ...)`，编码/封装变化才另开新会话并用 `group_id` 关联；失败退回旧档续录。
- `src-tauri/src/recorder.rs:2221 recheck_all_quality`：遍历活动录制触发上述检查。

### 3.3 凭据与抖音鉴权

- `src-tauri/src/credential.rs:41 set`：写 Cookie 后强制 `set_status(platform,"ok")`。
- `src-tauri/src/credential.rs:66 is_ok`：必须有 Cookie 且状态为 `ok`。
- `src-tauri/src/credential.rs:85 cookie_for_url`：状态非 `ok` 返回 `None`，这是「过期 Cookie 不再注入」的关键闸门。
- `src-tauri/src/credential.rs:97 platform_for_host`：只映射 `bilibili/b23`、`douyu`、`huya`、`douyin/iesdouyin`，不映射 `douyincdn.com`，避免 CDN 请求带平台 Cookie。
- `src-tauri/src/platform/douyin.rs:109 enter`：先 `cookie_for_url(page_url)`，为空才匿名 ttwid；页面 Set-Cookie 与 base 合并；`has_ttwid` 才继续。
- `src-tauri/src/platform/douyin.rs:32 anon_ttwid`：`OnceCell<String>` + `get_or_try_init`，只缓存成功结果，注册失败可重试。
- `src-tauri/src/douyin_ws.rs:141 open`：建立连接时读取一次 `cookie_for_url` / 匿名 ttwid；连接期间不再读凭据。

### 3.4 复验与轮换

- `src-tauri/src/douyin_auth.rs:31`：`REVALIDATE_INTERVAL = 30 分钟`；`32 REFRESH_EVERY = 6 小时`；`33 RETRY_FAILED = 30 分钟`。
- `src-tauri/src/douyin_auth.rs:503 revalidate_inner`：无 `sessionid` 直接返回；探活成功且原状态 `expired` 时恢复 `ok`、发事件、`recheck_recording(app,"douyin_revalidated")`；失效则持写锁标过期；网络错误不改状态。
- `src-tauri/src/douyin_auth.rs:333 refresh_once`：只在 `rotation_allowed` 时轮换；写回前两次重读凭据，防止并发覆盖。

## 4. 本轮修复的状态机缺陷

### 缺陷 A：过期账号仍执行轮换，把内存状态洗回 `ok`

修复前 `refresh_once` 只检查 `has_session_cookie`，不看 `row.status`。过期账号点「立即检查」或维护循环到点时，仍会带失效 `sessionid` 访问首页：

- 若首页返回了不同的 `ttwid`，`db.save_credential` 会保留 `expired`，但随后 `credential::set` 强制把内存状态置 `ok`，失效 Cookie 又被注入下一次请求；
- 若首页只重发同值 `ttwid`（`changed=0`），旧代码无条件写 `fresh`「登录态有效」，账号卡出现「红色已失效 + 登录态有效」自相矛盾。

这不会直接打断已经在跑的 ffmpeg，但会让**下一次取流/续录/弹幕重连**错误地带回失效凭据，并让 UI 状态不可信。

修复：

- 新增 `fn current_row(db) -> Option<CredentialRow>`（`douyin_auth.rs:267`）。
- 新增 `fn rotation_allowed(row) -> bool { row.status == "ok" && has_session_cookie(&row.cookie) }`（`:275`）。
- `refresh_once` 首次读取（`:340`）、拿锁后重读（`:362`）、写回前（`:431`）、最终写状态前（`:472`）全部改为 `!rotation_allowed(...)` 即跳过/丢弃，返回 `NotRefreshable`；并发期间 Cookie 被改则 `Discarded`。
- 新增单测 `rotation_requires_ok_status_and_session`（`:697`）：`ok+sessionid` 允许，`expired` 拒绝，`ok` 但无 `sessionid` 拒绝。

### 缺陷 B：`changed=0` 无条件写 `fresh`

旧逻辑即使服务端只重发同值 `ttwid` 也写「登录态有效」。修复后仅在允许轮换且凭据仍一致时记录结果；`changed=0` 不再掩盖过期状态。

## 5. 真机证据

### 5.1 录制跨越自动轮换与退出登录

| 时间 | 事件 | 日志/结果 |
| --- | --- | --- |
| 17:42:46 | 自动开录 | `开始录制 ... quality=原画 · FLV requested_qn=40000 actual_qn=10000 line=f11 FLV`；ffmpeg PID 4246 |
| 17:45:26 | 录制中点「立即检查」 | `抖音首页轮换 Cookie rotated=["ttwid"]` → `差异计算完成 changed=0`；PID 仍 4246 |
| 17:50:10 | 录制中点「退出登录」并二次确认 | `删除平台登录凭据 platform=douyin` → `已删除平台凭据` → `账号凭据变更，已清空取流与房间信息缓存`；PID 仍 4246，弹幕 frames 从 203 继续到 205 |
| 17:51:13 | 手动停止 | `开始合并分片 parts=9 bytes=211357684` → `分片合并完成 parts=9 bytes=203794272 elapsed_ms=1204` |
| 17:51:15 | 收尾 | `录制结束 status="finished" reason="stopped" file_size=203794272 restarts=0 elapsed_ms=508686` |

这场 8 分 28 秒的录制同时跨越了「自动轮换检查」和「退出登录」，`restarts=0`、PID 全程 4246。DB 最终 `status=finished / file_size=203,794,272 / duration_ms=507,000 / danmaku_count=0 / post_state=done`；该央视新闻房只有 like/stats 帧，没有聊天弹幕，所以 0 条属实。

### 5.2 过期账号 + 立即检查不会复活

- 先把抖音 `status=expired` 但保留有效 Cookie，启动后日志 `抖音登录态已恢复`，证明「过期但实际仍有效」可由复验恢复。
- 再把 Cookie 改为 `sessionid=invalid-expired-test`、`status=expired`、`credential_refresh failed/key_stale=1`，启动新 bundle。约 35 秒后：
  `DEBUG 抖音登录态不可轮换，跳过 Cookie 轮换 status=expired`
  DB 仍 `expired`（87 B）、`failed/key_stale=1`。
- 设置 → 账号点抖音「立即检查」，再次出现同一条跳过日志；DB 未变。
- 账号卡显示红色「登录已失效」「登录态已失效，自动复验已暂停」「上次检查 17:29 · 需要重新登录」「抖音登录态已失效，请重新粘贴 Cookie」，不再出现「登录态有效」。

截图：`/tmp/dy-expired-settings2.png`、`/tmp/dy-expired-after-check.png`、`/tmp/dy-account-expired-full.png`。

## 6. 参考项目对照

| 项目 | 机制 | 与本项目一致性 |
| --- | --- | --- |
| bililive-go | `servers/handler.go:3385-3405 applyCookiesToLives` → `live/internal/base_live.go:80 UpdateLiveOptionsbyConfig` 只原子替换 Options，不重启 recorder；`recorders/recorder.go:402 tryRecord` 每轮按当时 Options 的 Cookie 取流，已启动 parser 继续旧 URL | 一致：配置/凭据热更新不触碰运行中进程，下一次取流才换 |
| bililive-go | 改名/分段/改流设置走 `manager.go:243 RestartRecorder` | 对应本项目的画质升档 `respawn_inner`：只有明确的录制参数变化才重启 |
| streamlink | `stream/http.py:65 open` 用固定 args；`stream/hls/hls.py:617-625 HLSStreamReader.__init__` 对 `dict(stream.args)` 快照；`ffmpegmux.py:61 _open_streams` 只在 open 时打开子流 | 一致：打开后的 stream 不受后续 Cookie 变化影响 |

结论：本项目「Cookie 轮换不动活动录制、断流续录才重新取流」与两个参考项目的解耦方式一致；差异是本项目把画质升档也做成同 session 续写，而不是新开一场。

## 7. 边界与未覆盖

1. **`changed>0` 的完整写回没有真机证据。** DB 里 Cookie 是 `enc:v1:` 密文，不能直接复制到 UI 制造新 Cookie；本轮该路径只有代码审查与单测。`changed=0` 路径有真机证据。
2. **抖音没有真正的长期续期。** 当前只做「粘贴 Cookie 校验 + 30 分钟复验 + 6 小时首页 Set-Cookie 同步」，没有扫码登录或长期票据；会话失效后必须重新粘贴。因此「自动刷新」通常只是吸收 `ttwid`，不保证登录态永续。
3. **弹幕 WS 不会因 Cookie 更新主动重连。** 连接建立时快照身份，连接期间继续用旧 Cookie；只有断线重连才使用新身份。这是与录制一致的「已开始任务不被凭据变化打断」策略。
4. **录制中升档会短暂断流。** 保存 Cookie 后若目标画质高于当前档，`recheck_quality` 会主动 `respawn`，同 session、分片续号，约 1–5 秒空档；这不是 Cookie 轮换导致的，而是用户/登录解锁了更高档位。
5. **应用重启会中断录制。** ffmpeg 是应用子进程，退出应用即结束；启动恢复会把未完成场次标记为中断。与 Cookie 状态无关。
6. **活动 ffmpeg 的签名 URL 过期后由 CDN 决定。** 若 URL 本身到期，ffmpeg 退出后走续录，用当时 Cookie 重新取流；若 Cookie 也已失效，则回退匿名，画质可能下降。

## 8. 验证

- `cargo fmt --check`、`cargo check --all-targets` 通过（0 警告）。
- `cargo test --lib`：**342 passed / 0 failed / 18 ignored**（原 341，新增 1 个轮换权限用例）。
- `pnpm exec tsc -b`、`pnpm run check:i18n`（821 keys）、`pnpm run check:docs`（163 files）、`pnpm build` 通过。
- `pnpm app:build` 成功：release bundle **32.03 MiB**（2026-11-15 18:07，日志 `/tmp/hc-build-douyin-cookie3.log`）。
- 本轮只改 `src-tauri/src/douyin_auth.rs`，未触碰 B 站/斗鱼平台代码；342 个单测全过，账号页状态展示与添加弹窗文案未回归。

## 9. 文件清单

- `src-tauri/src/douyin_auth.rs`：`current_row`、`rotation_allowed`、`refresh_once` 四处闸门、单测。
- `src-tauri/src/monitor.rs`：在录硬闸、离线二次确认。
- `src-tauri/src/recorder.rs`：watcher、`respawn_inner`、`recheck_quality`。
- `src-tauri/src/credential.rs`：Cookie 注入与状态闸门。
- `src-tauri/src/platform/douyin.rs`：匿名 ttwid、登录 Cookie 合并。
- `src-tauri/src/douyin_ws.rs`：弹幕连接建立时的身份快照。
