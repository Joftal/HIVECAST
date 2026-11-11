# 138 · B 站三态 Cookie 与取流审计

> 状态：**已实测并修复两个缺陷**（2026-11-11）。对比「扫码 Cookie / 网页 Cookie.txt / 未登录」三态下的登录、直播间数据、画质、线路、取流、录制、续期与 Cookie 泄露面。
> 关联：[137-B站扫码与Cookie全链路复查](137-B站扫码与Cookie全链路复查.md)、[134-B站扫码登录与Cookie自动续期审计](134-B站扫码登录与Cookie自动续期审计.md)、[130-取流与线路](130-取流与线路.md)、[127-平台Cookie实测与全量画质验证](127-平台Cookie实测与全量画质验证.md)。监控/录制跨 Cookie 生命周期的分析见 [139](139-监控录制与Cookie状态全场景分析.md)。

---

## 1. 结论先行

| 维度 | 扫码 Cookie | 网页 Cookie.txt | 未登录 |
| --- | --- | --- | --- |
| 登录态 | 有效，执事只食芝士 / uid 352130563 | 有效，同一账号 | 无效（nav -101） |
| 直播间信息 | 正常 | 正常 | 正常（公开数据） |
| 请求画质 | qn=10000 → 10000 原画 1080p60 | qn=10000 → 10000 原画 1080p60 | qn=10000 → 250 超清 720p30 |
| 画质清单 | 15000/10000 高位可用 | 15000/10000 高位可用 | 仅 250/150 可用，4K/原画不再标可用 |
| 线路 | 6 组 | 7 组 | 6 组 |
| FLV 取流/录制 | 成功，1080p60 | 成功，1080p60 | 成功，720p30 |
| HLS / fMP4 / HEVC | 成功 | 成功 | HLS 成功；fMP4/HEVC 未单独取 |
| 自动续期 | 可续期（有 ac_time_value） | **不可续期**（Cookie.txt 缺 ac_time_value） | 不适用 |
| Cookie 泄露 | 未发现 | 未发现 | 不适用 |

一句话：**扫码与网页 Cookie 在「取流、录制、画质、线路、直播间数据」上一致，差异只在凭据字段和自动续期能力；未登录不是不可用，而是画质降到 720p30，且本轮修掉了“匿名仍把 4K/原画标成可用”的展示错误。**

### 与 122–127 画质文档的关系

- **qn 映射、编码推断、40000→10000、g_qn_desc + accept_qn 合并、线路分组都没有变**；本轮只改了 `available` 的判定。
- **登录态结果不变**：扫码/网页仍实测 15000（2K）、10000（原画）、400、250、150 等档位可选；4K/杜比不在 `accept_qn` 时仍为 false。这与 [127](127-平台Cookie实测与全量画质验证.md) 的登录态表格一致。
- **匿名态是修正**：[125](125-画质动态变化、录制稳定性与兜底.md) 与 [126](126-全平台画质可用性改造.md) 里“`accept_qn` 内即标记可用”的说法对匿名不成立；匿名请求 400 或 10000 时 `current_qn` 都只回 250，但 `accept_qn` 仍列 15000/10000。现在无凭据时按 `current_qn` 封顶，匿名不再显示 4K/原画可选。
- [127](127-平台Cookie实测与全量画质验证.md) 的 B 站表是**带 Cookie** 的结果，仍然有效；不要拿它推断匿名能力。

---

## 2. 测试环境与方法

- 代码基线：`ead3c9e`，测试时间 2026-11-11 凌晨。
- 房间：`room_id=6`（哔哩哔哩英雄联盟赛事，LPL 直播中）。
- 三份凭据：
  - **扫码**：应用数据库 `credential` 表中的 B 站 Cookie（AES-256-GCM 加密，`secret.bin` 解密后使用）。
  - **网页**：`/Library/dshproject/HIVECAST/Cookie.txt` 中「哔哩哔哩Cookie：」那一行（26 个键、1262 字符）。
  - **匿名**：`credential::remove("bilibili")` 后不注入任何 Cookie。
- 三种验证手段：
  1. 直接 HTTP：`nav`、`cookie/info`、`getRoomPlayInfo`、CDN Range GET。
  2. 真实适配器：临时 `#[ignore]` 测试调用 `BilibiliAdapter::resolve("6")`、`room_info`、`stream_plan(QN_ORIGIN)`，测完已删除，不留在工作区。
  3. 真实录制：ffmpeg 9.0.2，完全复刻录制器参数（`-c copy`、`-f segment`、`-segment_format mpegts`、`-segment_time 6`、`-reset_timestamps 1`、`-rw_timeout 30000000`、Referer/Origin/UA），每态录 6 秒。

---

## 3. 凭据差异

| 字段 | 扫码 Cookie | 网页 Cookie.txt | 说明 |
| --- | --- | --- | --- |
| SESSDATA | 有 | 有 | 登录主凭据 |
| bili_jct | 有 | 有 | CSRF |
| DedeUserID | 有 | 有 | 两者相同：352130563 |
| ac_time_value | 有 | **无** | 续期 correspond 必需；网页 Cookie 因此不可自动轮换 |
| buvid3 | 无 | 有 | 设备标识，不影响登录/取流 |
| bili_ticket | 无 | 有 | 风控票据，不影响本次取流 |
| 键数量 | 7 | 26 | 网页 Cookie 更完整，但缺续期字段 |

两者 `nav` 返回完全一致：`code=0`、`isLogin=true`、`uname=执事只食芝士`、`mid=352130563`、`vipStatus=1`。匿名 `nav` 返回 `code=-101`。

`Cookie.txt` 原先权限是 `0666`（同机其他用户可读），本轮已收紧为 `0600`；文件内容未改，仍由 `.gitignore:46` 的 `Cookie*.txt` 忽略。

---

## 4. 登录与直播间数据

直播间公开数据三态一致：

| 字段 | 三态结果 |
| --- | --- |
| 主播/房间名 | 哔哩哔哩英雄联盟赛事 |
| live_status | 1（直播中） |
| online | 941055 |
| 头像 / 封面 | 均可取到 |
| 房间号 | 6 |

无效 Cookie 的退化行为也测了：`SESSDATA=invalid; bili_jct=invalid; DedeUserID=1; ac_time_value=invalid` 时，`nav` 返回 `-101`、`cookie/info` 返回 `-400`，但 `getRoomPlayInfo` 仍返回 `code=0`、`current_qn=[250]`。也就是说失效 Cookie 会自动退化成匿名，不会让取流整体失败。

---

## 5. 画质

### 5.1 三态实测

| 请求 qn | 扫码 | 网页 | 未登录 |
| --- | --- | --- | --- |
| 10000（原画） | current_qn=10000，1920×1080 60fps | current_qn=10000，1920×1080 60fps | current_qn=250，1280×720 30fps |

三态 `g_qn_desc` 都是同一组平台档位：30000 杜比、20000 4K、15000 2K、10000 原画、400 蓝光、250 超清、80 流畅；`accept_qn` 里还出现 150。

### 5.2 匿名“4K/原画可用”的展示错误（已修）

原实现 `/Library/dshproject/HIVECAST/src-tauri/src/platform/bilibili.rs:300` 把所有 codec 的 `accept_qn` 求并集，`available: accepted.contains(qn)`（原 `:472`）直接当作“可用”。但 `accept_qn` 是编码能力清单，匿名也会列 15000/10000；实际只能拿到 250。结果是未登录用户看到 4K/原画可选，选中后平台悄悄回 250。

修复：

- 无凭据时，或以最高档探测（`request_qn(qn) >= 10000`）时，取所有 codec 的 `current_qn` 最大值作为当前实际封顶 `granted_qn`（`:311`）。
- 新增 `:519 quality_available(qn, accepted, granted)`：`accepted` 必须包含，且 `granted >= 10000` 或 `qn <= granted`。
- `:488` 与补档分支 `:505` 都改用该函数。
- 登录后请求 10000 仍返回 10000，无法据此判断 2K/4K 权限，因此登录态继续沿用平台能力清单；匿名态被正确收窄到 250。

新增单测 `caps_quality_availability_to_granted_when_probing_highest`：`granted=10000` 时 15000/10000 可用；`granted=250` 时只有 250 可用，400/10000/15000 不可用。

修复后真实匿名适配器输出（qn, available）：

```text
probe 400:  [(30000,false),(20000,false),(15000,false),(10000,false),(400,false),(250,true),(150,true),(80,false)]
probe 10000:[(30000,false),(20000,false),(15000,false),(10000,false),(400,false),(250,true),(150,true),(80,false)]
```

即匿名只剩 250 超清、150 高清两档可选；扫码/网页登录态实测 15000/10000 高位可用，仍按平台能力清单展示。

> 残留：登录账号显式以低档（如 qn=400）请求时不封顶，因为此时 `current_qn` 只反映请求档而不是账号上限；登录账号的 2K/4K 真实权限也没有单独探测。

### 5.3 编码组合

每次响应 6 个 codec 组合：

- `http_stream / flv / avc`
- `http_stream / flv / hevc`
- `http_hls / ts / avc`
- `http_hls / ts / hevc`
- `http_hls / fmp4 / avc`
- `http_hls / fmp4 / hevc`

每条 2 个 host，去重后 6–7 个 CDN host，均为 `*.bilivideo.com`。

---

## 6. 线路

| 状态 | 分组数 | 首个线路 host |
| --- | --- | --- |
| 网页 | 7 | `d1--cn-gotcha04b.bilivideo.com` |
| 扫码 | 6 | `d1--cn-gotcha08b.bilivideo.com` |
| 匿名 | 6 | `d1--cn-gotcha09b.bilivideo.com` |

- 对每个 host 发 Range GET：FLV 返回 `200 video/x-flv`；ts/fmp4 返回 `200 application/vnd.apple.mpegurl` 或 `text/html`，正文是 `#EXTM3U` 播放列表，均能在 32 KB 内读到数据。
- 线路数量和 host 顺序会随平台调度变化，不是账号能力差异；三态都能正常出流。
- 所有 `bilivideo.com` 请求都不带 Cookie（见第 8 节）。

---

## 7. 取流与录制

### 7.1 FLV / HLS

| 状态 | 格式 | 分片 | 首片 |
| --- | --- | --- | --- |
| 网页 | FLV | 3 | 6.02s / 2.6 MB / 1920×1080 60fps / H.264+AAC / 3.48 Mbps |
| 扫码 | FLV | 3 | 6.02s / 2.7 MB / 1920×1080 60fps / H.264+AAC / 3.58 Mbps |
| 匿名 | FLV | 3 | 6.06s / 783 KB / 1280×720 30fps / H.264+AAC / 1.03 Mbps |
| 网页 | HLS | 3 | 正常出片，出现一次 `Failed to open segment … skipping` |
| 扫码 | HLS | 3 | 正常出片 |

网页 HLS 的那次 segment 告警是 CDN 瞬时 404/断连，后续分片继续，整体录制成功；不是 HLS 不可用。录制器本身有分段重试/重连逻辑，见 [131-录制状态机与恢复](131-录制状态机与恢复.md)。

### 7.2 fMP4 与 HEVC

- fMP4 HLS（网页 Cookie，format=fmp4、codec=avc、qn=10000）：6.007667s / 1,065,020 B / 1920×1080 60fps / H.264+AAC；stderr 有 `Found duplicated MOOV Atom. Skipped it`，不影响出片。
- HEVC FLV（网页 Cookie，codec=hevc、qn=10000）：2 片，首片 6.044667s / 1,903,876 B / 1920×1080 60fps / HEVC+AAC；stderr 为空。

说明四种主要组合（FLV/HLS × H.264/HEVC，以及 fMP4）都能真实录制，不只是接口返回可用。

---

## 8. Cookie 使用与泄露面

### 8.1 注入边界

- `/Library/dshproject/HIVECAST/src-tauri/src/credential.rs:51 cookie_for_url` 只按域名找 Cookie；`:57 platform_for_host` 只匹配 `bilibili.com`、`b23.tv`（以及斗鱼/虎牙/抖音自己的域名）。
- `/Library/dshproject/HIVECAST/src-tauri/src/platform/http.rs:203 send` 在请求未显式带 Cookie 时注入；`:213` 调 `cookie_for_url`。
- CDN `*.bilivideo.com` 不匹配，因此不会拿到登录 Cookie；适配器实测 `cookie_to_cdn=false`。
- 扫码、续期、`cookie/info` 等账号接口显式带 Cookie，不走全局注入。

### 8.2 录制器请求头

`/Library/dshproject/HIVECAST/src-tauri/src/recorder.rs:2643 build_command` 只把候选的 `Referer`、`Origin`、`User-Agent` 交给 ffmpeg，不传 Cookie。ffmpeg 直接访问 CDN，因此 CDN 节点看不到 SESSDATA/bili_jct。

### 8.3 重定向与日志

- reqwest 0.12.28 的 `remove_sensitive_headers` 在跨 host/port 重定向时删除 `COOKIE`；B 站接口本次没有跨站重定向。
- `http.rs:167 redact_url` 把 query 值全部替换为 `***`，错误与 `tracing` 日志都用脱敏 URL。
- 日志检索：`~/Library/Logs/com.hivecast.desktop` 中 `SESSDATA`、`bili_jct`、`ac_time_value` 出现次数均为 0；数据库文件 `grep SESSDATA` 为 0；日志里 13 条 `bilivideo` 记录只有 host，没有 URL/query/expires/token。
- DB 中 Cookie 是 AES-256-GCM 密文；`Set-Cookie` 只解析，不写日志。
- 需要留意的本地暴露面：ffmpeg 进程 argv 里会有带签名参数的流 URL（不是 Cookie），同机同用户可通过进程列表看到；应用日志没有打印它。

---

## 9. 续期

| 状态 | cookie/info | 能否自动续期 |
| --- | --- | --- |
| 扫码 | `code=0, refresh=false`（刚轮换过） | 能，有 `ac_time_value` |
| 网页 | `code=0, refresh=true` | **不能**，Cookie.txt 缺 `ac_time_value` |
| 无效 Cookie | `code=-400` | 不能 |

网页 Cookie 虽然 `refresh=true`，但 `ac_time_value` 是 B 站网页 JS 写在 localStorage 里的字段，不在 Cookie 头里；`Cookie.txt` 没有它，`correspond` 无法计算，应用只能判 `not_refreshable`，账号卡会显示不可续期。要自动续期，需要重新扫码，或从浏览器 localStorage 额外导出 `ac_time_value`（应用目前没有这个导入入口）。

扫码 Cookie 已在 [137](137-B站扫码与Cookie全链路复查.md) 做过真实 `refresh=true` 强制轮换：`ac_time_value` 的 SHA-256 前缀从 `cd83e780` 变为 `00b56388`，写回后 `cookie/info` 返回 `refresh=false`，`nav` 仍是同一账号。

---

## 10. 本轮修复

| 问题 | 修法 | 验证 |
| --- | --- | --- |
| 匿名仍把 4K/原画标成可用 | 最高档探测时用 `current_qn` 封顶，新增 `quality_available` | 新单测 + `cargo test --lib` 270 passed |
| `Cookie.txt` 的「哔哩哔哩Cookie：」前缀导致 buvid3 键名带前缀、实际丢失 | `credential.rs:74 strip_platform_prefix` 支持 `B站`/`Bilibili` 与可选 `Cookie` 词 | `strips_cookie_file_platform_prefix` 四个变体；270 passed |
| `Cookie.txt` 权限 0666 | `chmod 600` | `ls -l` 为 `-rw-------` |

构建：`cargo check --all-targets`、`pnpm app:build` 通过（release bundle 30.64 MiB）。

---

## 11. 限制与后续

- 只测了 `room_id=6` 一个直播房间、一个时间点；不同分区、不同 CDN、不同账号等级可能有差异。
- 没有单独探测登录账号的 2K/4K/杜比真实权限；当前仍用平台能力清单展示，匿名态已按 `current_qn` 收窄。
- 没有测长时间录制、断流重连、跨天续期；这些在 131/136/137 有独立验证。
- HLS 的瞬时 segment 失败没有做专项弱网注入；当前依赖录制器重试。
- 没有覆盖 `b23.tv` 短链解析；本轮只验证了 `live.bilibili.com` 房间号/URL。

---

## 12. 复现要点

不要把真实 Cookie 写进命令历史或仓库。建议用环境变量：

```bash
# 只读检查（示例，Cookie 从安全位置读取，不落盘）
COOKIE="$(cat /path/to/bili.cookie)"
curl -s 'https://api.bilibili.com/x/web-interface/nav' -H "Cookie: $COOKIE"
curl -s 'https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo?room_id=6&protocol=0,1&format=0,1,2&codec=0,1&qn=10000&platform=web&ptype=8&dolby=5&panorama=1' \
  -H "Cookie: $COOKIE" -H 'Referer: https://live.bilibili.com/6'
```

单测：

```bash
cd /Library/dshproject/HIVECAST/src-tauri
cargo test --lib
```

---

## 13. 涉及文件

- 审计与修复：`src-tauri/src/credential.rs`、`src-tauri/src/platform/bilibili.rs`
- Cookie 注入与脱敏：`src-tauri/src/platform/http.rs`
- 录制器请求头：`src-tauri/src/recorder.rs`
- 续期与账号：`src-tauri/src/account.rs`
- 相关文档：[130](130-取流与线路.md)、[131](131-录制状态机与恢复.md)、[132](132-登录与Cookie.md)、[134](134-B站扫码登录与Cookie自动续期审计.md)、[135](135-B站扫码取态与Cookie自动续期实现.md)、[136](136-B站Cookie续期状态与手动检查.md)、[137](137-B站扫码与Cookie全链路复查.md)
