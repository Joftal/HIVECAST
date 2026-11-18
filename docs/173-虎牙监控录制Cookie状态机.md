# 173 虎牙监控录制 Cookie 状态机

- 日期：2026-11-18
- 范围：虎牙「直播监控 → 自动/手动录制」在无 Cookie、有效 Cookie、中途补 Cookie、退出登录、Cookie 失效、自动续期、恢复、断流续录、应用重启等状态下的行为。
- 结论：已启动的 ffmpeg 与 Cookie 解耦；Cookie 只影响下一次平台页面 / 取流请求。Cookie 新增、轮换、失效、删除都不会主动停止活动录制；只有直播结束、断流重取、画质升级或应用退出才会改变录制进程。
- 本轮修复两处：① 凭据重检只作用于同平台，保存虎牙 Cookie 不再顺带探测 B 站/斗鱼/抖音的活动录制；② 虎牙扫码确认补上清缓存与画质重检，和 B 站/斗鱼一致。

## 1. 状态模型

| 状态 | 判定 | 平台请求 | 监控/录制 |
| --- | --- | --- | --- |
| 无凭据 | credential 表无 huya 行 | 匿名 | 可监控、可录制 |
| 有效 ok | 有 Cookie 且 status = ok | 注入 Cookie | 可监控、可录制；画质按实际取流能力 |
| 失效 expired | checkLogin 返回未登录 | cookie_for_url 返回 None，自动匿名 | 不停录；继续匿名监控，断流续录也匿名 |
| 更新中 | cookieExchange / checkLogin 在途 | 继续用内存中的旧 Cookie | 不影响活动 ffmpeg |
| 更新完成 | 新 Cookie 已落库并替换内存 | 后续请求用新 Cookie | 活动 ffmpeg 不变；仅当「原状态 expired 后恢复」才触发画质重检 |

关键实现：

- src-tauri/src/credential.rs:66 is_ok：有 Cookie 且状态为 ok 才算登录态。
- src-tauri/src/credential.rs:85 cookie_for_url：非 ok 时不注入 Cookie，避免拿失效登录态去请求平台触发风控。
- src-tauri/src/platform/http.rs：按请求 URL 的 host 找平台并注入 Cookie；CDN 域名不映射到任何平台，因此流地址请求不带账号 Cookie。
- src-tauri/src/huya_auth.rs:1066 refresh_once：20 分钟 cookieExchange，失败回退 credLogin；成功后才替换内存 Cookie。
- src-tauri/src/huya_auth.rs:1389 revalidate：6 小时 checkLogin 复验；只有原状态为 expired 且本次恢复时才重检画质。

## 2. 监控与录制链路

1. monitor.rs:1055 run_check 按间隔检查房间。
2. check_room → broker.room_info_fresh 取房间信息；虎牙页面请求按 URL 自动带当前 Cookie。
3. apply_room_info：
   - 在播且满足自动录制条件时发 RoomAction::Start；
   - 离线要连续两次确认，且 should_stop_for_offline(confirmed, flowing) 为真才 Stop；
   - 平台错误只写检测错误并退避，不猜测下播，也不直接停录。
4. recorder.start_inner 取流时生成签名 CDN 地址和 ffmpeg 参数，之后 ffmpeg 只认这份快照。
5. 录制中 Cookie 变化不会改写已经启动的 ffmpeg：不重启、不换 header、不重签 URL。
6. ffmpeg 退出后，respawn_inner 才重新取流；这时才使用「当时」的 Cookie，属于新的录制进程/续录。

## 3. 场景矩阵

| # | 场景 | 监控行为 | 录制行为 | 说明 / 证据 |
| --- | --- | --- | --- | --- |
| 1 | 无 Cookie 启动 | 匿名检查，正常识别在播 | 自动开录，匿名取流 | 真机 11336726 于 09:03:30 匿名开录 蓝光4M · FLV |
| 2 | 有 Cookie 启动 | 带 Cookie 检查 | 自动开录 | 真机 189201 于 09:56:25 登录态开录 蓝光15M · FLV |
| 3 | 匿名录制中补 Cookie | 后续检查用新 Cookie | 活动 ffmpeg 不变；重检同平台活动录制，若可升档才换流 | 真机 189201：10:05:35 退出登录后 PID 92286 不变；10:10:10 补 Cookie 后仍 92286 |
| 4 | 有 Cookie 录制中退出登录 | 立即转匿名；页面仍 200 | 不停录，PID 不变 | 真机 189201 10:05:35 删除凭据，PID 92286 持续；11336726 09:22:53 同样不停 |
| 5 | 录制中 Cookie 失效 | 自动匿名检查；若页面仍返回在播则继续 | 不停录，PID 不变 | 真机 189201 于 10:00:59 标记 expired，至 10:02:34 仍 PID 92286，三次匿名检查均 live=true |
| 6 | 失效后恢复（立即检查/自动续期） | 恢复 ok，后续请求带 Cookie | 活动 ffmpeg 不变；只重检虎牙活动录制 | 真机 10:02:34 huya_refreshed，两个虎牙房重检，PID 92286 不变 |
| 7 | 录制中正常轮换 changed=true | 用新 Cookie | 不重检、不打断 | 09:33:23 write_flag=true changed=true；该房 09:33:32 自身断流，09:33:35 续录，属流断不是轮换 |
| 8 | 录制中轮换 changed=false | 状态写 fresh | 不重检、不打断 | 10:02:34 changed=false，录制 PID 不变 |
| 9 | 保存无效 Cookie | 拒绝保存，保留原凭据 | 不停录 | 真机 10:16:35 toast/日志「虎牙 Cookie 已失效或未登录」，PID 92286 不变 |
| 10 | 手动 Cookie 无 udb_cred | 可正常监控 | 可正常录制 | 只能校验，不能自动续期；UI 显示「自动续期不可用」 |
| 11 | 平台 401/403/风控/网络错误 | 写检测错误并退避，不判下播 | 不停录；断流续录可能失败并重试 | check_room_error 只提示 + 退避；停止仍要求连续离线且文件不再增长 |
| 12 | 断流续录 | 平台仍显示在播时按「断流续录」 | 旧 ffmpeg 退出，新 ffmpeg 用当前 Cookie 重新取流，同一 session 续写 | 11336726 多次 2 分钟级断流均走该路径 |
| 13 | 恢复后画质可升 | 取流能力变化 | 同容器：短暂停旧 ffmpeg 后同会话续写；跨编码/封装：旧会话收尾 + 新会话，group_id 关联 | 本次虎牙探测无更高档，未触发实际升档；代码路径见 recorder.rs:1949 |
| 14 | 应用重启 | 重新读当前凭据 | 重启前录制已随进程结束；重启后按当前凭据新开 | 不会恢复旧 ffmpeg 或旧签名 URL |
| 15 | 扫码确认 | 清取流/房间信息缓存 | 只重检虎牙活动录制 | 本次修复 huya_qr_poll；此前虎牙少这一步 |
| 16 | 多平台并发 | 各平台独立 | 保存虎牙 Cookie 不影响其他平台 ffmpeg | 真机 B 站 PID 90200 从 09:07:17 到 10:16 未变 |

## 4. 真机证据

环境：macOS，release bundle，RUST_LOG=hivecast_lib=debug，日志 /tmp/hc-huya-cookie.log；测试库备份 /tmp/hivecast-before-huya-cookie-audit.db。

- 匿名开录：11336726 于 09:03:30「开始录制 ... quality=蓝光4M · FLV requested_qn=40000 actual_qn=10000 line=华为云」。
- 录制中补 Cookie：09:17:32 保存 36 个 Cookie 的网页虎牙 Cookie。日志只有虎牙重检：
  「凭据恢复，检查是否可升级画质 room_id=d8c99ef7... platform="huya" reason="credential_saved"」；
  「平台当前没有更高画质，保持当前档位 current_qn=10000 candidate_qn=10000 target_qn=40000」。
  保存前后虎牙 PID 90853、B 站 PID 90200 均不变。
- 录制中退出登录：09:22:53「删除平台登录凭据 platform=huya」，未出现停止/收尾；虎牙 PID 91321（09:22:42 断流后续录的进程）保持不变，B 站 90200 不变。
- 稳定房间基线：189201 于 09:56:25 开录，PID 92286；到 10:16 的 20 分钟内没有断流，便于观察 Cookie 操作。
- 失效不打断：10:00:59 把 credential.status 置为 expired。10:01:30、10:02:00、10:02:31 三次房间检查均 live=true，PID 92286 不变。
- 恢复并重检：10:02:34 点「立即检查」，cookieExchange changed=false，状态恢复 ok，日志：
  「虎牙 Cookie 自动续期完成 changed=false rotated=false」
  「凭据恢复，检查是否可升级画质 room_id=d8c99ef7... platform="huya" reason="huya_refreshed"」
  「凭据恢复，检查是否可升级画质 room_id=cdaefe06... platform="huya" reason="huya_refreshed"」
  两个虎牙房均探测后保持 10000 档；PID 92286 不变。
- 退出登录后匿名续录：10:05:35 退出登录，PID 92286 不变；10:06:30、10:07:00 两次房间检查匿名 live=true。
- 匿名录制中补 Cookie：10:10:10 保存同一 Cookie，「凭据恢复 ... platform="huya" reason="credential_saved"」，探测保持 10000 档，PID 92286 不变。
- 无效 Cookie：10:16:35 保存 acf_uid=1; acf_auth=fake，日志「ERROR 界面错误 context=ipc:credential_save 虎牙 Cookie 已失效或未登录」；PID 92286 不变，原凭据未被覆盖。
- 平台隔离：以上所有操作期间 B 站 90200 从 09:07:17 持续到 10:16，未被虎牙重检触碰。
- 流本身不稳定：11336726 约每 2–2.5 分钟 ffmpeg 退出，走「平台仍显示在播，按断流续录」；09:35:39 因重试耗尽出现「多次复核仍未确认下播，按未确认结束收尾」，09:42:30 重新开录。该现象与 Cookie 无关，但说明虎牙房间流质量差异很大，分析时必须区分「流断」与「凭据变化」。

## 5. 参考项目对照

- bililive-go：src/live/lives.go:133 RetainRecordingVersion 在 Cookie/Quality/AudioOnly 变化时递增 recording version；src/live/internal/base_live.go:102 更新 Options 时保留版本；src/recorders/recorder.go:453 只丢弃「取流期间选项变了」的这一轮，不重启已经在录的 recorder。结论与本项目一致：凭据热更新只影响下一次取流。
- streamlink：session/http.py:259 set_cookies_from_file 管 session cookie jar；stream 对象打开时固定 args，后续 cookie 变化不影响已打开的 stream。没有「轮换 Cookie 就重建 stream」的耦合。

## 6. 本轮修复

### 6.1 凭据重检按平台过滤

src-tauri/src/recorder.rs:2225 recheck_all_quality(self, app, broker, platform, reason)：

    let room_platform = self.db.get_room(&recording.room_id).ok().flatten().map(|r| r.platform);
    if room_platform.as_deref() != Some(platform) { continue; }

原因：此前保存任意平台 Cookie 都会遍历所有活动录制并 broker.probe，可能让另一平台录像短暂换 ffmpeg。现在保存虎牙 Cookie 只重检虎牙；扫码确认也传各自平台。调用点：

- commands.rs:2179 credential_save → &platform
- commands.rs:2221 bilibili_qr_poll → "bilibili"
- commands.rs:2251 douyu_qr_poll → "douyu"
- commands.rs:2287 huya_qr_poll → "huya"
- douyin_auth.rs → "douyin"
- douyu_auth.rs → "douyu"
- huya_auth.rs:1318 / :1437 → "huya"

### 6.2 虎牙扫码确认补重检

src-tauri/src/commands.rs:2287 huya_qr_poll：qr_poll 返回 confirmed 时 clear_groups + recheck_all_quality(..., "huya", "huya_qr_confirmed")。此前 huya_auth::store_account 只清缓存，不重检；docs/169 却写「命令层再触发画质重检」，实现与文档不一致。现在与 B 站/斗鱼一致。

## 7. 边界与未闭环

- 4K 账号能力：虎牙没有 B 站式 accept_qn 字段；4K 用真实签名地址 Range 探测，probe_quality 不带 Cookie。若未来 4K 变成账号权限，当前实现无法用 Cookie 解锁，需要新增账号能力接口。本次没有正在直播的 4K 房间，未验证 4K 成功路径。
- 画质升级的短暂断点：保存 Cookie / 扫码确认 / 失效恢复触发的重检，如果发现更高档，同容器会停旧 ffmpeg 后同会话续写（约 1–5 秒），跨编码/封装会开新会话。这不是 20 分钟常规轮换造成的，但用户手动保存 Cookie 时可能感知到一次换流。
- 自动轮换未实等 20 分钟：本次用「立即检查」走同一 refresh_once 代码路径；正常轮换 was_expired=false 不重检，已由代码和 09:33 的 changed=true 日志确认。
- 网络中断/401 未真机造：代码上 check_room_error 只退避、不判下播；停止仍要求连续离线 + 文件不再增长。活动 ffmpeg 用已签名 URL，短期网络错误不会改命令行。
- 失效判定依赖 checkLogin：虎牙房间页对无效 Cookie 仍返回 200，不能从页面判断登录态；只能靠 checkLogin，因此「页面还能开、账号已失效」是正常组合。
- 多房间重检成本：保存一次虎牙 Cookie 会探测所有虎牙活动录制；房间很多时会有一批页面请求，但都是一次性、无 ffmpeg 重启。

## 8. 验证与文件

- cargo fmt --check、cargo check --all-targets 0 警告。
- cargo test --lib：386 passed / 0 failed / 19 ignored。
- pnpm app:build：release bundle 32.54 MiB。
- 代码：src-tauri/src/recorder.rs、src-tauri/src/commands.rs、src-tauri/src/douyin_auth.rs、src-tauri/src/douyu_auth.rs、src-tauri/src/huya_auth.rs。
- 文档：本篇；同步修正 docs/169 的扫码重检描述，README 索引与 docs/19 M139/v3.31。
