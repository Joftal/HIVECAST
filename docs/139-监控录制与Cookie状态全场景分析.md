# 139 · 监控/录制与 Cookie 状态全场景分析

> 状态：**代码路径审查完成**（2026-11-11）；同日 R1/R2/R3/R4/R6/R7 已在 [140](140-监控录制Cookie联动修复.md) 修复，R5 评估后保留现状。本文保留审查时的原始判断，修复细节以 140 为准。结论分三类：**代码确定**、**已有实测**（引用 138）、**待真机复核的风险**。
> 关联：[131-录制状态机与恢复](131-录制状态机与恢复.md)、[132-登录与Cookie](132-登录与Cookie.md)、[136-B站Cookie续期状态与手动检查](136-B站Cookie续期状态与手动检查.md)、[137-B站扫码与Cookie全链路复查](137-B站扫码与Cookie全链路复查.md)、[138-B站三态Cookie与取流审计](138-B站三态Cookie与取流审计.md)。

---

## 1. 结论先行

| 问题 | 结论 |
| --- | --- |
| Cookie 增删/失效/续期会不会打断已开始的录制？ | **不会**。已启动的 ffmpeg 只拿到 CDN URL 和 Referer/Origin/UA，不持有 Cookie；账号变更只清 Broker 缓存，不调用 `recorder.stop`。 |
| Cookie 轮转会不会让监控把在播判成离线？ | **不会**。监控只读 `room_info`，不读 `credential` 状态；`room_info` 失败走错误/退避，不会当成 `NotLive`。 |
| Cookie 什么时候真正生效？ | **下一次 `stream_plan`**：开录、续录（respawn）或观看重新解析时。已建立的 ffmpeg 连接不会热更新 Cookie。 |
| 录制会持续到什么时候？ | 持续到直播结束、手动停止、磁盘/并发等硬条件，或连续断流重试耗尽；Cookie 变化本身不触发结束。断流后的续录是**同一个 session_id 内换一个 ffmpeg 子进程**，不是新建场次。 |
| 最大的正确性问题是什么？ | 不是“录不下去”，而是**同一场续录画质变了，元数据/UI 仍显示开场档位**；默认“最高可用”还会在 Cookie 失效时静默从 1080p 降到 720p，不告警。 |

一句话：**Cookie 生命周期与录制生命周期已经解耦，方向是对的；但“录制质量元数据”没有跟着续录更新，属于可感知的正确性缺口。**

---

## 2. 证据链

### 2.1 开录：Cookie 只参与解析，不进入 ffmpeg

- `/Library/dshproject/HIVECAST/src-tauri/src/recorder.rs:834-886 start_inner`：拿到房间级锁后，调 `broker.plan(record_qn)` 解析线路；Cookie 在 `platform/http.rs send` 里按 URL 域名注入。
- `/Library/dshproject/HIVECAST/src-tauri/src/platform/bilibili.rs:369-373`：候选 `StreamCandidate.headers` 只有 Referer、Origin、User-Agent。
- `/Library/dshproject/HIVECAST/src-tauri/src/recorder.rs:2643-2669 build_command`：只把这些 headers 拼成 ffmpeg `-headers`，**不读 credential store**。
- CDN 域名 `*.bilivideo.com` 不匹配 `/Library/dshproject/HIVECAST/src-tauri/src/credential.rs:51 cookie_for_url` 的平台域名，因此 CDN 请求也不会被注入 Cookie。
- 所以：Cookie 影响的是 `getRoomPlayInfo` 返回的**档位和地址**，不是 ffmpeg 进程的运行参数。Cookie 变化对已启动进程没有“写入点”。

### 2.2 续录：同一个 session，重新解析线路

- `/Library/dshproject/HIVECAST/src-tauri/src/recorder.rs:1749-1755 respawn`：断流/异常退出后调 `broker.plan_for_respawn(&reference, qn)`；失败只 WARN「续录解析线路失败」并返回 false。
- `/Library/dshproject/HIVECAST/src-tauri/src/broker.rs:308-313 plan_for_respawn`：只 `drop_address_cache`（清该房间 groups+infos），保留 failures；本次允许穿冷却出门，但失败照常累计。
- `/Library/dshproject/HIVECAST/src-tauri/src/recorder.rs:1809-1839`：续录成功只替换 `active.child / stderr_tail / restarts / started`，**不更新 `active.recording.quality`、不更新 `active.qn`、不写 DB `record_session.quality`**。
- `/Library/dshproject/HIVECAST/src-tauri/src/recorder.rs:3587-3593 pinned_qn`：`requested==0` 时锁首段实际档位；`requested>0` 时保留请求值。默认“最高可用”是 `40000`，续录会继续请求 `40000→10000`，因此 Cookie 恢复后可以升档，Cookie 失效后可以降档。
- 结论：续录会换 ffmpeg 子进程，但仍是**同一场录制**；文件继续追加到同一母带，不会新建 `record_session`。

### 2.3 监控：不读 Cookie 状态

- `/Library/dshproject/HIVECAST/src-tauri/src/monitor.rs:634-682`：自动开录条件只看 `room.auto_record`、`auto_record_allowed`、是否已在录、`auto_suppressed`、`auto_blocked`、失败冷却、开录冷却、finalizing、磁盘；**没有 credential 条件**。
- `/Library/dshproject/HIVECAST/src-tauri/src/monitor.rs:715-743`：检测失败只记录错误/退避；`PlatformError::NotLive` 当正常；不会停正在录的会话。
- 离线判定走 `confirm_offline` 两次成功检查；确认后也只有 `should_stop_for_offline(confirmed, flowing)=confirmed && !flowing` 才停。文件仍在增长时只 WARN「平台报离线，但录像仍在增长，暂不停录」。
- 结论：Cookie 增删/过期不会改变监控的“开/停”判断；能停录的只有平台确认离线且文件不增长、手动停止、磁盘/并发等。

### 2.4 账号变更：只清缓存

- `/Library/dshproject/HIVECAST/src-tauri/src/commands.rs:1304-1324 credential_save / credential_delete`：保存/删除后调 `broker.clear_groups()`；`1332-1341 bilibili_qr_poll confirmed` 同样。
- `/Library/dshproject/HIVECAST/src-tauri/src/account.rs:1044-1050 clear_stream_cache`：只调 `broker.clear_groups()`，不碰 recorder。
- `/Library/dshproject/HIVECAST/src-tauri/src/broker.rs:308-313 clear_groups`：清所有 groups+infos，**保留 failures 冷却**。
- 因此：登录/退出/换号/续期写回都不会停止任何 active 录制；只会让下一次解析重新请求平台接口。

### 2.5 平台边界：不是所有“Cookie”都一样

| 平台 | API 请求是否带账号 Cookie | ffmpeg 是否带账号 Cookie | 说明 |
| --- | --- | --- | --- |
| B 站 | 是，`http::send` 按域名注入 | 否，只有 Referer/Origin/UA | Cookie 影响 `getRoomPlayInfo` 档位；CDN 不带 |
| 虎牙 | 是，`http::get_text` 自动注入 | 否 | 有 Cookie 就带，无专门续期 |
| 斗鱼 | API 可注入；候选 headers 显式 `Cookie: dy_did=...` | 只带设备号 `dy_did` | ffmpeg 不带你保存的斗鱼账号 Cookie |
| 抖音 | `enter()` 自取 `ttwid`，不读 credential store | 否 | 保存的抖音 Cookie 当前不参与取流 |

因此下文的“Cookie 生效”主要指 B 站账号 Cookie；斗鱼/抖音的录制链路本来就是设备号/匿名链路，不能套用 B 站结论。

---

## 3. 场景矩阵

“active”指已经启动的 ffmpeg；“下一次解析”指开录或 respawn。

| # | 场景 | 监控 | 已录 active | 下一次解析 | 元数据/UI | 判定 |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 无 Cookie 监控-录制 | 正常，公开 `room_info` | B 站匿名档，通常 qn=250 超清 720p | 仍匿名 250 | 开场 250，正确 | ✅ |
| 2 | 有效 Cookie 监控-录制 | 正常 | B 站登录档，qn=10000 原画 1080p | 仍 10000 | 开场原画，正确 | ✅ |
| 3 | 无 Cookie 开录，中途补 Cookie | 不受影响 | **不中断**，继续当前匿名流 | 默认最高会请求 10000，可升到原画；显式低档按请求值 | 仍显示开场 250 | ⚠️ 画质可能升档但 UI 不更新 |
| 4 | 有 Cookie 开录，中途删除 Cookie | 不受影响 | **不中断**，当前 CDN URL 继续 | 匿名 250 | 仍显示开场原画 | ⚠️ 画质可能降档但 UI 不更新 |
| 5 | Cookie 失效（-101） | 公开信息仍正常 | **不中断**，Cookie 不参与 ffmpeg | 过期 Cookie 仍会被注入；通常仍返回 250 | 仍显示开场档位；低档画质清单可能虚标 | ⚠️ 见 §5 R3 |
| 6 | 自动续期进行中 | 不受影响 | **不中断** | 取决于请求落在写回前/后 | 不变 | ✅ 允许一次旧档解析 |
| 7 | 自动续期成功 | 不受影响 | **不中断**；`clear_groups` 只清缓存 | 下一次用新 Cookie | 不变 | ✅ 但画质元数据仍不更新 |
| 8 | 续期失败 / key_stale | 不受影响 | **不中断**；旧 Cookie 保留 | 继续用旧 Cookie | 不变 | ✅ 录制不受影响 |
| 9 | 换账号 / 重新扫码 | 不受影响 | **不中断** | 下一次用新账号权限 | 不变 | ✅ |
| 10 | 仅监控，不录制 | 正常 | 无 | 不适用 | 不适用 | ✅ |
| 11 | 开录解析途中 Cookie 被替换 | 不受影响 | 本次可能仍用旧 Cookie 解析结果 | 下一次才用新 Cookie | 开场档位可能旧 | ⚠️ 缓存回填竞态，见 §5 R4 |
| 12 | `clear_groups` 后仍在失败冷却 | 可能继续退避 | 已录不受影响 | 新开录可能被 Broker 冷却挡住最多 60s | 不变 | ⚠️ 见 §5 R5 |

---

## 4. 举例

### 4.1 场景 3：无 Cookie 开录，10 分钟后补 Cookie

时间线（房间 `auto_record=1`，全局画质“最高可用” `record_qn=40000`）：

1. **09:00** 房间在播，无 Cookie。监控自动开录；`request_qn(40000)=10000`，B 站匿名实际只给 `current_qn=250`。录制器用实际 qn 250 记录文件名/质量，`RecordingInfo.quality="超清"`，DB `record_session.quality=250`。
2. **09:10** 用户在设置页保存 B 站 Cookie。`account::save_cookie` 先 `nav` 校验，成功后写 store/DB、唤醒续期循环，命令层 `broker.clear_groups()`。
3. **09:10** 正在跑的 ffmpeg **没有任何变化**：它的 argv 里只有 CDN URL 和 headers，没有 Cookie；不会因为清缓存被杀。
4. **09:20** 网络抖动，ffmpeg 退出。`respawn` 在 3 秒后重新解析：`plan_for_respawn(..., qn=40000)` 现在带新 Cookie，平台返回 `current_qn=10000`，新 ffmpeg 以 1080p 续录，仍是同一个 `session_id`。
5. **09:20 之后** 媒体库/房间行/任务卡仍显示“超清 720p”，因为 `respawn` 不更新 `active.recording.quality`，也不写 DB。实际文件后半段已经是 1080p。
6. 如果 09:20 没有断流，这场会一直 720p 到直播结束；补 Cookie 不会主动重连，也不会主动升档。

**结论**：录制不中断是对的；“补 Cookie 立即生效”是错误预期——它只对下一次解析生效，且元数据不会跟着变。

### 4.2 场景 4：有效 Cookie 开录，跨天自动续期后失效

时间线（默认最高画质，实际开场 1080p）：

1. **20:00** 有效 Cookie 开录，`actual_qn=10000`，`quality="原画"`，DB quality=10000。
2. **02:00** 12 小时续期循环触发：`cookie/info` 显示 `refresh=true`，走 `correspond → refresh → confirm`，写回新 Cookie，然后 `clear_stream_cache`（`broker.clear_groups()`）。
3. **02:00** ffmpeg 仍在跑；续期写回不会停录，也不会切 session。日志只有「B 站 Cookie 续期成功」。
4. **03:00** CDN 连接断开，`respawn` 用新 Cookie 重新解析 qn=10000，续录仍是原画。此时元数据“原画”刚好正确。
5. **10:00** Cookie 过期；下一次 `cookie/info` 返回 `-101`。`account.rs` 把 DB `credential.status` 标为 `expired`、`clear_stream_cache`、发过期通知，但**不删除 store/DB 里的 Cookie**。正在录的 ffmpeg 不受影响。
6. **11:00** CDN URL 过期，`respawn` 带过期 Cookie 解析 qn=10000，平台按游客返回 `current_qn=250`。`quality_fallback(40000,250)=false`（因为请求值是“最高可用”哨兵 40000），所以**没有 WARN、没有通知**；新 ffmpeg 以 720p 续录。
7. **11:00 之后** 媒体库仍显示“原画 1080p”，DB 仍是 10000；实际录像后半段 720p。直到用户重新登录，下一次 respawn 才会恢复 1080p。

**结论**：Cookie 轮转/失效都没有打断录制，但“默认最高”路径在失效时是**静默降档 + 元数据错误**。这是本轮最值得修的点。

### 4.3 场景 11：开录解析途中换 Cookie

1. 09:00:00 自动开录，`start_checked` 进入 `broker.plan`，请求已带着旧 Cookie 发出。
2. 09:00:00.2 用户保存新 Cookie，命令层 `clear_groups()`。
3. 09:00:00.5 旧请求返回，`plan_inner` 在 `clear_groups` 之后把旧结果写回 groups/infos。
4. 本次开录用旧档位；之后 60 秒内（`GROUPS_TTL`）同一房间的解析可能命中旧缓存，直到 TTL 过期或下一次清缓存。
5. 不会停录，也不会报错；只是“刚登录仍按游客档开录”的低概率窗口。

### 4.4 场景 6/7：续期写回与 respawn 撞车

- 续期写回是原子的：先拿 `CREDENTIAL_WRITE` 锁比较快照，再写 store/DB、`clear_groups`。
- respawn 若在写回前解析，用旧 Cookie；写回后解析，用新 Cookie。两种都能出流，不会中断；最多多一次旧档/新档的 respawn。
- 若写回时 respawn 正在等 Broker 房间 gate，写回后的 `clear_groups` 不会取消在途请求，仍可能写回旧结果（同 4.3）。

---

## 5. 风险与缺陷

| ID | 风险 | 严重度 | 证据 | 影响 | 建议 |
| --- | --- | --- | --- | --- | --- |
| R1 | 续录不更新画质元数据 | **中高** | `recorder.rs:1809-1839` 只换 child；`recorder.rs:1107-1129` 只在开录时写 quality | 媒体库/房间行/DB 显示开场档位，实际文件可能已变档 | respawn 成功后更新 `active.recording.quality`、`active.qn` 与 DB；若要保留历史，增加 `quality_history` |
| R2 | 默认最高档静默降档 | **中高** | `quality_fallback` 对 `requested=40000` 恒 false（`recorder.rs:3599-3604`） | Cookie 失效后 1080p→720p 无 WARN、无通知 | respawn 时比较“上次实际 qn”和“本次实际 qn”，变化就 WARN/通知；不要只看请求值 |
| R3 | 过期 Cookie 让低档画质清单虚标 | 中 | `bilibili.rs:310` 只判断 store 是否有 Cookie；`bilibili.rs:519-521` 非匿名且 qn<10000 时把 granted 当 10000 | 用户选 400/250 时看到高档“可用”，实际只给 250；默认 40000 不受影响 | 用 `credential.status` 或实际 `current_qn` 封顶；过期态按匿名处理展示 |
| R4 | `clear_groups` 与在途 plan 的回填竞态 | 中低 | `broker.rs:107-185` 先 await adapter，后拿锁写缓存；`clear_groups` 不取消在途请求 | 登录/续期后最长 60 秒仍可能命中旧档缓存 | 给 Broker 加 generation/epoch，写回前比对；或在 clear 时递增代次 |
| R5 | `clear_groups` 保留 failures | 低 | `broker.rs:308-313` 明确保留冷却；`note_failure` 只对 throttle 退避 | 换 Cookie 后若此前被限流，新开录仍要等冷却；已录不受影响 | 凭据变更时可选清 failures；respawn 已 bypass，开录可继续等 |
| R6 | 续录画质变化没有通知事件 | 中 | `recorder.rs:1774-1782` 只 `tracing::warn!`，没有 `emit_notice` | 用户只能看日志，UI 无感 | 复用 `quality_fallback` notice，或在 respawn 变化时发 `quality_changed` |
| R7 | 一场内混合画质无法表达 | 低 | DB 只有单个 `record_session.quality` | 媒体库无法说明“前 2 小时 1080p、后 1 小时 720p” | 增加 `quality_history`（时间/档位/原因），或至少记录“发生过变档” |

**修复状态（2026-11-11）**：R1/R2/R3/R4/R6/R7 已在 [140](140-监控录制Cookie联动修复.md) 落地——续录更新 qn/quality/line 并写回 `quality_history`，实际档位变化发 WARN + 通知，过期 Cookie 按匿名封顶，Broker 用 epoch 防止旧结果回填。R5 保持“保留 failures”不变，评估见 140 §5。

**已确认不是问题的点：**

- Cookie 轮转不会导致 `recorder.stop`；`clear_stream_cache` 只清 Broker 缓存。
- Cookie 失效不会让监控把房间判离线；`room_info` 失败不会走 `confirm_offline`。
- Cookie 删除不会让已录 ffmpeg 退出；ffmpeg 不依赖 Cookie。
- 续期成功/失败都不会新建 `record_session`；断流续录仍是同一 session。
- 斗鱼的 `dy_did` 候选 Cookie 与账号 Cookie 是两回事；抖音当前不读保存的 Cookie。不要把 B 站结论外推到这两家。

---

## 6. 其他可能情况

1. **保存了无效 Cookie**：B 站 `save_cookie` 会先 `nav` 校验，失败直接拒绝，不会进 store；因此正常 UI 路径不会出现“无效 Cookie 开录”。但旧数据库/手工写入可能留下过期 Cookie，仍会走 R3。
2. **Cookie 被删除后重新添加**：删除时 `clear_groups`，active 继续；重新添加再 `clear_groups`。下一次解析用新 Cookie；没有 session 中断。
3. **续期 `key_stale`**：只写 `credential_refresh` 状态，不删 Cookie；旧 Cookie 继续可用到真正过期。录制不受影响。
4. **换账号**：`store_account` 写新 Cookie、清 `credential_refresh`、`clear_groups`；active 继续，下一次解析用新账号权限。弹幕 WS 认证固定 `uid=0`，不随账号切换，不会因换号重连。
5. **多房间/多平台**：`clear_groups` 是全局的，一次 B 站续期会清掉所有平台的缓存；不会停任何录制，只会让各房间下一次解析重新请求。并发上限/信号量仍在，不会因为清缓存瞬间放大请求。
6. **手动停止/收尾**：`stop` 与 Cookie 无关；若 Cookie 在 finalize 期间变化，只影响之后的解析，不影响收尾。
7. **应用退出/崩溃**：不在本分析范围；退出会停 ffmpeg，崩溃恢复走 131 的恢复逻辑，与 Cookie 轮转无直接关系。
8. **Cookie 状态过期但 store 仍有值**：这是当前设计的“保留凭据以便重登/重试”，但 recorder 不读 status；所以“status=expired”不会阻止开录，只会让低档画质清单虚标（R3）。

---

## 7. 参考项目对照

| 项目 | Cookie 更新方式 | 是否重启录制 | 与 HIVECAST 的关系 |
| --- | --- | --- | --- |
| bililive-go | `src/live/internal/base_live.go:80 UpdateLiveOptionsbyConfig` 构造新 `Options` 并原子替换；`:102 RetainRecordingVersion` 保留录制相关版本；`src/servers/handler.go:3389 applyCookiesToLives` 只热替换 options | **不重启**。`src/recorders/manager.go:243 RestartRecorder` 只用于分段/房间名等场景；`src/recorders/recorder.go` 的重试会重新 `GetStreamInfos` | 同构：Cookie 是“下一次取流”的凭据，不是流的一部分。HIVE 用全局 credential store + `clear_groups` 代替 options 热替换 |
| streamlink | `src/streamlink/session/http.py` 的 requests Session CookieJar；`session/options.py` 的 `http-cookies` / `http-cookies-files` 只影响请求 | 不适用；stream 打开后 Cookie 不再是流的一部分，重连由 stream 层处理 | 同构：请求期凭据，已打开的流不热更新 |
| HIVECAST | `credential::set/remove` + `broker.clear_groups()`；下一次 `stream_plan` 生效 | **不重启**；断流 respawn 才换 ffmpeg 子进程 | 方向正确；缺“续录后更新质量元数据”和“默认最高档降档告警” |

参考项目的共同点：**没有任何一家在 Cookie 更新时重启录制**。HIVE 的“不打断”与它们一致；差异在 HIVE 把“录制质量”当成开场快照，而 bililive-go 的录制器每次重试都会重新取流并重新选择档位。这正是 R1/R2 的来源。

---

## 8. 建议的修复顺序（历史）

> 2026-11-11 更新：下列 P1/P2 已实现，见 [140](140-监控录制Cookie联动修复.md)；P3 评估后决定本轮不做。

1. **P1：respawn 后更新质量元数据**。最小改法：`respawn` 拿到 `candidate` 后，更新 `active.recording.quality`、`active.qn`，并 `db.update_session_quality(session_id, actual_qn)`；同时 emit `rooms://changed` / 任务事件。若要表达混合画质，再加 `quality_history`。
2. **P1：续录实际档位变化必须告警**。把 `quality_fallback(requested, actual)` 改成“与上一次实际 qn 比较”；默认 40000 从 10000→250 也要 WARN，并复用 `emit_notice("quality_fallback", ...)`。
3. **P2：过期 Cookie 的画质清单按匿名封顶**。`bilibili.rs:310` 不能只看 store 是否有值；读取 `credential.status` 或直接用本次 `current_qn` 作为 `granted`，避免低档请求把 4K/原画标可用。
4. **P2：Broker 增加代次**。`clear_groups` 递增 epoch，`plan_inner` 写回前比对；防止登录/续期后的旧结果回填 60 秒。
5. **P3：凭据变更时是否清 failures**。当前保留冷却更保守；如果希望“换 Cookie 立刻重试”，可在 `credential_save/delete` 时清 failures，但要评估限流风险。
6. **P3：文档/UI 说明**。在账号卡或录制详情里标注“本场画质可能因 Cookie/线路变化而变”，避免用户把元数据当全程承诺。

---

## 9. 真机验收清单（设计稿，尚未全部执行）

> 表中的“复现 R1/R2”步骤用于验证修复前缺陷；修复后应改为验证同一 session 内 quality/history 更新且通知出现。

| 步骤 | 预期 |
| --- | --- |
| 无 Cookie 开录 2 分钟，记录 quality/DB | qn=250、quality=超清、文件可播 |
| 录制中保存有效 Cookie，不制造断流 | 录制不中断；日志无 stop；quality 不变（仍 250） |
| 录制中保存 Cookie 后人为断流（切网/重启 CDN 连接） | 同一 session 续录；新 ffmpeg 实际 qn=10000；日志有续录；UI/DB 若仍 250 即复现 R1 |
| 有效 Cookie 开录，等待/触发一次续期 | 不中断、不新建 session；日志「B 站 Cookie 续期成功」 |
| 把 Cookie 改成过期值（不动真实账号）后断流 | 续录成功但 qn=250；检查是否无 WARN（复现 R2） |
| 删除 Cookie 后断流 | 续录匿名 250；不中断 |
| 多房间同时录制时切换 B 站账号 | 所有房间不停；下一次解析用新账号；无重复 session |
| 检查日志 | 不应出现 `录制被 Cookie 中断` 之类路径；只应出现续录/解析日志 |

---

## 10. 限制

- 本文最初是**代码路径审查**，没有重新做“真实直播 + Cookie 轮转 + 断流”的长时间实验；画质降档/升档的实机表现引用 138 的真实接口与录制数据。修复后的验证状态见 140 §4：单测/DB 回写已覆盖，真实 1080p→720p 断流已在 140 的真机 smoke 中复现并验证。
- 斗鱼/抖音的 Cookie 链路与 B 站不同，本报告的“Cookie 生效”结论只对 B 站账号 Cookie 成立；虎牙是“有就带”，斗鱼 ffmpeg 只带 `dy_did`，抖音不读 store。
- 录制质量元数据的修复已涉及 DB schema/事件（140），但媒体库/任务中心尚未展示 `quality_history`；分割片段按完整 history 复制，跨片段展示语义未定。
