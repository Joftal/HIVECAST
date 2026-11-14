# 155 · 斗鱼三态 Cookie 与直播全链路审计

> 范围：斗鱼扫码 Cookie、网页 Cookie.txt、未登录三种状态下的登录识别、直播间数据、直播流、画质、线路、录制、弹幕、Cookie 使用/轮转与泄露风险。
> 结论：**三种状态都能拿到直播流并录制；登录态与匿名态的差异集中在「最高画质」，不在「能不能播」。** 扫码 Cookie 与网页 Cookie 对直播流/录制/画质/线路/房间数据的行为一致；差异只在长期凭证：扫码 Cookie 有 LTP0，可自动轮换并在旧 Cookie 失效后恢复，网页 Cookie 只能校验、不能续期。审计中发现并修复了「匿名态画质菜单仍允许选原画，实际静默降级」的缺陷。
> 时间：2026-11-14。基线：`154` 完成后的工作区 + 真实斗鱼账号（uid 100921751，昵称「番茄炒芝士」）。
> 环境：macOS，release bundle 31.73 MiB，房间 9999（yyfyyf / 陪伴每一天！ / DOTA2）。

---

## 1. 结论摘要

1. **登录识别正确**：扫码 Cookie 与网页 Cookie 都能被识别为已登录，显示同一 uid/昵称；匿名态不显示已登录。
2. **房间数据一致**：三态请求 `betard/9999` 均返回 `show_status=1`、`videoLoop=0`、同一主播与标题；`getEncryption` 均 `error=0`。
3. **线路一致**：三态都拿到线路7（hw-h5）与线路13（hs-h5），录制使用线路13，观看可切两条线路。
4. **画质是唯一实质差异**：登录态请求原画 `rate=0` 返回 `rate=0`，实际 2560×1440；匿名态请求 `rate=0` 被平台降为 `rate=4`，实际 1920×1080。网页 Cookie 与扫码 Cookie 一样能拿到原画。
5. **匿名态画质入口曾误导**：`multirates` 匿名仍列出「原画 2K60」，旧实现把所有档位标成 available，用户点原画后静默变蓝光。已改为按平台实际返回档位限制可选性，现匿名菜单把原画标为「当前不可用」。
6. **修复不能反向锁死登录用户**：第一版按本次响应 `rate` 封顶，导致登录用户从原画切蓝光后原画被永久标灰。最终只对未登录态使用实际档位封顶，登录态信任账号能力表。
7. **扫码 Cookie 能轮转**：手动触发 `safeAuth` 后 18 个 Cookie 全部换新（长度 1783→1767，哈希变化），LTP0 与设备号保持不变；轮换后观看/录制仍为原画。
8. **旧 Cookie 失效可自愈**：恢复轮换前数据库启动应用，日志先「登录态已失效，已标记过期」，1 秒内「Cookie 自动续期完成」，状态回到 ok，无需重新扫码。
9. **网页 Cookie 不轮转**：没有 LTP0，账号卡明确「自动续期不可用」，没有「立即检查」；手动粘贴只做 follow/top3 校验。
10. **未发现 Cookie 泄露**：24 个高熵敏感值扫描 15 个日志文件、16,260 条 URL，0 命中；日志无 `Cookie:` 头、无敏感 query、无 LTP0；CDN 只收到派生 `dy_did`。

---

## 2. 审计方法与证据来源

### 2.1 三种状态

| 状态 | Cookie 来源 | 数量 | 长期凭证 |
| --- | --- | --- | --- |
| 扫码 | 应用内斗鱼二维码登录 | 18 项 / 1304 字符 | LTP0 327 字符 + device_id 87 字符，加密存 `credential_secret` |
| 网页 | 用户提供的 `Cookie.txt` 第 3 行 | 30 项 / 1875 字符（trim 后） | 无 LTP0 |
| 匿名 | 删除 `douyu` credential / secret / refresh | 0 | 无 |

网页 Cookie 原始行末尾带 CR。第一次测试直接把含 CR 的整行写入加密库，应用日志报 `Cookie 含非法字符，已跳过注入`；这是测试注入绕过 `credential::normalize` 造成的，不是 UI 粘贴链路的问题。重新用 trim 后的 1875 字符经设置页「更新 Cookie」保存后，日志无非法字符，登录/观看/录制全部正常。UI 链路会 trim、按分号/换行拆分并合并同名项。

### 2.2 三条证据链

1. **直接 API 探针**：Node 脚本 `/tmp/dyprobe.mjs` 用三套 Cookie 请求 `betard`、`getEncryption`、`getH5PlayV1`，只记录状态码、`error`、`rate`、线路名与画质档位，不输出 token/URL query。
2. **应用真机**：release bundle 分别以扫码、网页、匿名三态启动，走真实「监控 → 观看 → 手动录制 → 停止」链路，读取 `record_session` 与 `ffprobe`。
3. **代码与日志审计**：`src-tauri/src/platform/http.rs`、`credential.rs`、`platform/douyu.rs`、`douyu_auth.rs`；对应用日志、观看 ffmpeg 日志做敏感值扫描。

测试前数据库备份：`/tmp/hivecast-before-douyu-audit.db`（扫码凭据完整）与 `/tmp/hivecast-dy-qr-final.db`（轮换后的扫码凭据）。所有录制文件在收尾删除。

---

## 3. 登录与 Cookie 对比

### 3.1 两套 Cookie 的构成

同一账号，同一昵称，但会话不同：

| 项 | 扫码 Cookie | 网页 Cookie |
| --- | --- | --- |
| `acf_uid` | 100921751 | 100921751 |
| `acf_username` | 100921751 | 100921751 |
| 昵称 | 番茄炒芝士（URL 编码） | 番茄炒芝士（URL 编码） |
| `acf_auth` / `jwt` / `PHPSESSID` | 一套 | 另一套 |
| `acf_devid` | 有 | 无 |
| `dy_did` / `acf_did` / `acf_ssid` | 无 | 有 |
| `mba_*` | 无 | 有 |
| LTP0 | 有（secret 中） | 无 |
| 自动续期 | 可用 | 不可用 |

扫码 Cookie 的 18 项里保留 `acf_devid`，适配器用 `existing_device` 优先取 `dy_did` / `acf_did` / `acf_devid`；因此扫码态的设备号稳定，后续 `safeAuth` 不会因为缺 `dy_did` 而漂移。

### 3.2 账号卡呈现

- 扫码态：斗鱼卡显示「18 个 Cookie / 已登录 / 番茄炒芝士 / 自动续期可用（每 3 天检查）/ 上次检查 09:34 · 已轮换 Cookie」，按钮有「扫码登录 / 更新 Cookie / 退出登录 / 立即检查」。
- 网页态：显示「30 个 Cookie / 已登录 / 番茄炒芝士 / 自动续期不可用：手动粘贴的 Cookie 没有长期凭证，扫码登录后可启用」，**没有「立即检查」**。
- 匿名态：未登录，只有「扫码登录 / 粘贴 Cookie」；B 站卡不受影响。

网页态误点 B 站「立即检查」时，日志只出现 `platform="bilibili"`，斗鱼 Cookie 哈希未变化；证明「立即检查」按平台路由，不是全局续期。

---

## 4. 直播间数据

三态直接请求 `https://www.douyu.com/betard/9999`，结果一致：

| 字段 | 值 |
| --- | --- |
| HTTP / `error` | 200 / 0 |
| `room_id` | 9999 |
| `show_status` | 1 |
| `videoLoop` | 0 |
| 主播 | yyfyyf |
| 标题 | 陪伴每一天！ |
| 分区 | DOTA2（应用详情抽屉） |

应用监控页三态都显示 LIVE、斗鱼、yyfyyf、陪伴每一天！；详情抽屉分区 DOTA2，画质「原画」（这是房间默认标签，不代表匿名真的能取到原画）。在线人数在斗鱼接口缺失时不显示，属预期。

`getEncryption?did=***` 三态均返回 `error=0` 且有 data；日志中的 `did` 已脱敏。

---

## 5. 直播流、画质与线路

### 5.1 直接 API：匿名被降档

对 `getH5PlayV1/9999` 逐档请求：

| 请求 `rate` | 扫码实际 | 网页实际 | 匿名实际 |
| --- | --- | --- | --- |
| 0（原画） | 0 | 0 | **4（蓝光4M）** |
| 4（蓝光4M） | 4 | 4 | 4 |
| 8（蓝光8M） | 4 | 4 | 4 |

三态 `multirates` 都列出：`0 / 原画2K60 / 20142~20236 kbps`、`4 / 蓝光4M / 4000`、`3 / 超清 / 2000`、`2 / 高清 / 900`。匿名态仍然列出原画，但 `rate=0` 的播放地址实际是 `rate=4` 的流。这是平台行为，不是应用改参数。

三态 `rate=0` 的播放 URL host/path 均 `huos1a.douyucdn2.cn/live`，仅 token 不同；审计未输出 query。

### 5.2 应用观看实测

扫码态逐档切换：

| 画质 | qn | 实际分辨率 |
| --- | --- | --- |
| 原画 2K60 | 10000 | 2560×1440 |
| 蓝光 4M | 400 | 1920×1080 |
| 超清 | 250 | 1280×720 |
| 高清 | 150 | 960×540 |

每次切换日志都有「切换观看画质 qn=…」与「观看起播 quality=…」，没有静默失败。

网页态默认 qn=0：日志「取流请求完成 qn=0 groups=2 qualities=4」「观看起播 quality=原画2K60 · FLV」，截图 2560×1440；画质菜单原画/蓝光/超清/高清均可用，码率 20182/4000/2000/900 kbps。

匿名态默认 qn=0：日志「观看起播 quality=蓝光4M · FLV」，截图 1920×1080；修复后菜单「原画2K60」显示「当前不可用」，蓝光4M/超清/高清分别 4000/2000/900 kbps。

### 5.3 缺陷：匿名画质可选性误导

**现象**：修复前匿名态画质菜单把原画显示为可选（20176 kbps）。点击原画，日志确实发出 `qn=10000`，但「观看起播 quality=蓝光4M」，分辨率仍是 1920×1080；UI 没有任何降级提示。

**根因**：`src-tauri/src/platform/douyu.rs` 的 `stream_plan` 用 `multirates` 构造 `QualityInfo` 时全部 `available: true`。平台匿名请求 `rate=0` 返回 `rate=4`，`multirates` 仍列原画，于是入口与能力脱节。

**修复**：

- `src-tauri/src/platform/douyu.rs:315` 增加登录判断：`let authed = crate::credential::is_ok("douyu");`
- 登录态 `best_qn = 0`，表示不封顶，信任账号能力表；匿名态用 `qn_for_rate(data.rate)` 作为初值，并在每条线路响应后用 `payload.rate` 取最大值。
- 构造 `QualityInfo` 时 `available = quality_available(qn, best_qn)`；不可用档位 `hint = None`，前端统一显示「当前不可用」，不再展示会误导的码率。
- `src-tauri/src/platform/douyu.rs:564` 新增 `fn quality_available(qn: u32, best_qn: u32) -> bool`：`best_qn == 0 || qn <= best_qn`；`best_qn == 0` 表示平台没有给出可判定档位，避免误伤全部画质。
- `src-tauri/src/credential.rs:66` 的 `is_ok` 从 `#[cfg(test)]` 改为正式函数，供平台适配器判断登录态；语义仍是「有 Cookie 且 status=ok」。

**回归教训**：第一版直接用本次响应 `data.rate` 封顶。登录用户请求 `rate=4` 时平台返回 `rate=4`，从原画切到蓝光后，菜单会把原画永久标成不可用，等于把用户锁在低画质。最终改为「只有未登录态才用实际档位封顶」；登录态即使当前请求被降档，也保留账号能力表里的全部档位。画质可选性必须由账号能力决定，不能拿单次请求的降级结果反推。

**复验**：登录态先切蓝光（1920×1080），再开菜单，原画仍为 20153 kbps 可选；点回原画，日志 `qn=10000`，画面 2560×1440。匿名态原画保持「当前不可用」，蓝光/超清/高清可选。单测 `quality_available_follows_actual_rate` 覆盖登录、匿名、未知档位三种情况。

### 5.4 线路

三态都返回两条线路：

| 名称 | CDN | 录制/观看 |
| --- | --- | --- |
| 线路7 | hw-h5 | 观看默认线路，起播正常 |
| 线路13 | hs-h5 | 录制实际使用，起播正常 |

扫码态在线路13上切高清，日志「观看起播 quality=高清 · FLV line=线路13」，画面 960×540；说明线路与画质的组合不是只对默认线路生效。录制侧日志均为 `line=线路13`。

---

## 6. 录制实测

| 状态 | 时长 | 文件大小 | 弹幕 | 记录画质 | requested/actual qn | 线路 |
| --- | --- | --- | --- | --- | --- | --- |
| 扫码 | 75 s | 125,255,980 B | 50 | 原画2K60 · FLV | 10000 / 10000 | 线路13 |
| 扫码轮换后 | 16 s | 29,871,072 B | 50 | 原画2K60 · FLV | 10000 / 10000 | 线路7 |
| 扫码旧 Cookie 自愈后 | 12 s | 26,463,856 B | 13 | 原画2K60 · FLV | 10000 / 10000 | 线路13 |
| 网页 | 20 s | 8,058,135 B | 38 | 原画2K60 · FLV | 10000 / 10000 | 线路13 |
| 网页终验 | 12 s | 12,338,194 B | 25 | 原画2K60 · FLV | 10000 / 10000 | 线路13 |
| 匿名 | 15 s | 3,742,511 B | 22 | 蓝光4M · FLV | 10000 / 400 | 线路13 |
| 匿名终验 | 10 s | 1,476,553 B | 13 | 蓝光4M · FLV | 10000 / 400 | 线路13 |

`ffprobe` 验证：

- 扫码 75 秒文件：H.264，2560×1440，60 fps，79.6 秒，125,255,980 B；
- 网页 20 秒文件：H.264，2560×1440，60 fps，23.03 秒，8,058,135 B；
- 匿名 15 秒文件：H.264，1920×1080，30 fps，3,742,511 B。

停止后 `record_session` 均为 `status=finished`、`post_state=done`，弹幕 JSONL/ASS 正常导出。录制侧始终记录**实际** `quality` 与 `actual_qn`，匿名态会明确写「蓝光4M」，不会伪装成原画。

---

## 7. Cookie 使用与轮转

### 7.1 请求注入边界

- `src-tauri/src/platform/http.rs` 的 `send_inner` 只在请求没有显式 Cookie 时，按 URL 域名注入平台 Cookie；只对 `douyu.com` / `douyu.tv` 等平台域名生效，不发给 CDN。
- `redact_url` 把 query 值、敏感路径段、userinfo 替换为 `***`；日志只打印脱敏 URL。
- 斗鱼 CDN 候选 headers 固定 `Cookie: dy_did=<adapter derived did>`，**不带账号 Cookie**；`Referer https://www.douyu.com`、`Origin https://www.douyu.com`、平台 UA。
- `credential.rs` 只按域名匹配平台；`normalize` 会 trim、按分号/换行拆分、合并同名项。手动 Cookie 的尾部 CR 在 UI 链路会被去掉。
- 无效 Cookie 的错误文案只包含平台名与结果（如「斗鱼 Cookie 已失效或未登录」），不回显 Cookie 值。

### 7.2 扫码轮转

点击斗鱼「立即检查」触发 `safeAuth`：

| 项 | 轮换前 | 轮换后 |
| --- | --- | --- |
| Cookie 数量 | 18 | 18 |
| 长度 | 1783 | 1767 |
| SHA-256 前 16 位 | e5d49c97f68f8331 | 5aa4a732ecb1385a |
| `acf_auth` | 旧 | 新 |
| `acf_devid` | 有 | 有 |
| `dy_did` | 无 | 无 |
| LTP0 长度 | 327 | 327 |
| device_id 长度 | 87 | 87 |

日志：`手动检查 Cookie 续期 platform=douyu` → `斗鱼 Cookie 自动续期完成`；DB `credential_refresh` 变为 `outcome=refreshed`、`key_stale=0`、`message=已自动续期`；账号卡「上次检查 09:34 · 已轮换 Cookie」。轮换后观看仍原画 2560×1440，16 秒录制 29,871,072 B / 50 条弹幕。

### 7.3 旧 Cookie 失效后的自愈

把轮换前的数据库恢复后启动新 bundle：

```text
09:54:58 WARN 斗鱼登录态已失效，已标记过期
09:54:59 INFO 斗鱼 Cookie 自动续期完成
```

DB 从 `status=expired` 回到 `status=ok`，`credential_refresh` 为 `refreshed/key_stale=0`；账号卡「上次检查 09:54 · 已轮换 Cookie」。随后观看原画 2560×1440，12 秒录制 26,463,856 B / 13 条弹幕。

这说明 LTP0 不只是「提前换票」：旧 `acf_*` 被轮换作废后，仍可用 LTP0 + 设备号换回一套有效 Cookie，不需要重新扫码。网页 Cookie 没有 LTP0，旧 `acf_*` 失效后只能重新粘贴或扫码。

### 7.4 网页 Cookie 只校验、不轮转

网页 Cookie 保存时走 `validate_cookie`（follow/top3 探针），成功后 `credential_secret` 为 0；账号卡显示「自动续期不可用」，没有「立即检查」。保存后 `credential_refresh` 为 `not_refreshable`，维护循环不会每 30 分钟刷新「上次检查」时间。这是 154 已定的策略，本次未改。

### 7.5 设备号

- 扫码 Cookie 没有 `dy_did`，但保留 `acf_devid`；`existing_device` 优先取 `dy_did` / `acf_did` / `acf_devid`，因此轮换请求的设备号稳定。
- 适配器自身 `device_id() = md5("hivecast/douyu/{HOSTNAME}")`，本机为 `9ecb0ea4aaaf1ed95471df846bb89828`；这是观看/录制 CDN 请求使用的派生 did，不是账号 Cookie。
- 网页 Cookie 有 `dy_did`/`acf_did`，但没有 LTP0；保存时不会把旧 LTP0 套到新 Cookie 上（154 的约束）。

---

## 8. 泄露审计

### 8.1 方法

- 从扫码 Cookie 明文与网页 Cookie 明文提取 24 个长度 ≥12 的高熵值（`acf_auth`、`jwt`、`PHPSESSID`、`SESSDATA`、LTP0 等），逐个 `grep -F` 扫描；
- 扫描 15 个日志文件：应用日志 2026-11-08 至 11-14、观看 `ffmpeg.log`、`/tmp/hc-dy-audit.log`、`/tmp/hc-dy-web2.log`、`/tmp/hc-dy-anon.log` 等；
- 从日志提取 16,260 条 URL，检查是否含 `acf_auth` / `token` / `jwt` / `PHPSESSID` / `SESSDATA` / `LTP0` / `acf_uid` 查询参数；
- 检查日志中是否出现 `Cookie:` 头、明文 `acf_*`、`LTP0=`。

### 8.2 结果

| 检查 | 结果 |
| --- | --- |
| 24 个敏感值 grep | 0 命中 |
| 16,260 条 URL 敏感 query | 0 |
| 日志 `Cookie:` 头 | 0 |
| 日志明文 LTP0 / acf_auth | 0 |
| CDN 收到账号 Cookie | 未发现；固定只发派生 `dy_did` |
| 无效 Cookie 错误文案回显值 | 未发现 |

### 8.3 代码层保证

- `http.rs` 的注入按域名白名单，CDN 域名不匹配；
- `redact_url` 对 query 整体脱敏，日志不打印签名；
- 斗鱼 `stream_plan` 的 CDN headers 是显式构造的，不继承平台请求头；
- `douyu_auth` 的 `safeAuth` 只把 LTP0 与设备号发给 `passport.douyu.com`，回调链限制 HTTPS + `douyu.com` / `*.douyu.com`。

---

## 9. 代码改动与测试

本次只改了 2 个文件：

| 文件 | 改动 |
| --- | --- |
| `src-tauri/src/credential.rs:66` | `is_ok` 去掉 `#[cfg(test)]`，供平台适配器判断登录态 |
| `src-tauri/src/platform/douyu.rs:315` | `authed` 判断；登录态不封顶，匿名态按实际 `rate` 封顶 |
| `src-tauri/src/platform/douyu.rs:564` | 新增 `quality_available` |
| `src-tauri/src/platform/douyu.rs:744` | 新增单测 `quality_available_follows_actual_rate` |

验证：

- `cargo test --lib`：**318 passed / 0 failed / 17 ignored**；
- `cargo check --all-targets`：0 警告；
- `pnpm exec tsc -b`、`check:i18n` 807 keys、`check:docs` 156 files、`pnpm build`、`pnpm app:build` 通过；
- release bundle 31.73 MiB，含上述画质修复；
- 真机复验：扫码/网页/匿名三态观看与录制、扫码轮换、旧 Cookie 自愈、匿名原画置灰、登录态切低画质后原画仍可选。

---

## 10. 边界与未覆盖

1. **没有做抓包级验证**：泄露审计基于应用/ffmpeg 日志与代码路径，未用 tcpdump 解 TLS；CDN 只发 `dy_did` 的结论来自代码与日志。
2. **没有构造「手机同时确认两次」**：确认幂等由 154 的单测与 15 秒轮询窗口覆盖，未在本轮真实并发扫码。
3. **没有测试直播中途撤销授权**：只验证了「启动时旧 Cookie 已失效 → 自动换票」；正在播放/录制时平台强制踢下线的行为未构造。
4. **平台画质策略可能变化**：匿名 `rate=0 → 4` 是本次实测结果；若斗鱼调整 `multirates` 或 `rate` 语义，`quality_available` 需要重新核对。
5. **网页 Cookie 仍不支持自动续期**：这是产品约束，不是缺陷；过期后必须重新粘贴或扫码。
6. **匿名态只修正了画质入口**：实际码率/分辨率仍由平台决定；如果平台把 4 也降成 3，`best_qn` 会随之变化，但 UI 只保证不展示高于实际能力的档位。
7. **未覆盖付费/特权画质**：斗鱼房间 9999 没有 4K/8M 等更高档；本次只验证 0/4/3/2 四档。
8. **未做长时间稳定性**：单次录制最长 75 秒；未验证数小时录制中的 Cookie 轮换/失效。

---

## 11. 测试数据与清理

- 测试房间：`dy-9999`（斗鱼 9999，auto_record=0），已删除；
- 测试会话：5 条手动录制，已随 `record_session` 清空；
- 录制文件：`Recording/斗鱼/yyfyyf` 共 198 MB，已删除，目录 0 B；
- 缓存：`view`、`thumbs` 已删除，仅保留 WebKit；
- 数据库：恢复 `/tmp/hivecast-dy-qr-final.db` 后清空 room / record_session / media_file / task_history；`ui.theme=light`、`ui.language=zh-CN`、`monitor.interval_sec=30`、`post.mode=source`；bilibili / douyu 均 `status=ok`，斗鱼为轮换后的 18 项扫码 Cookie；
- 应用与 ffmpeg 均已退出；无 `hivecast` 进程；
- `/tmp/bili-db-cookie.txt`、`/tmp/dy-qr-cookie.txt`、`/tmp/dyprobe.mjs` 与测试截图在收尾删除，并执行 `pbcopy </dev/null`。

---

## 12. 复现路径

1. 备份数据库，准备三态：扫码（真实扫码或恢复含 LTP0 的备份）、网页（`Cookie.txt` 斗鱼行 trim 后粘贴）、匿名（删除 `douyu` credential/secret/refresh）。
2. 启动 release bundle，添加 `https://www.douyu.com/9999`，记录监控页房间数据。
3. 观看：默认 qn=0，记录「观看起播 quality / 分辨率」；打开画质菜单记录每档是否可用；逐档切换并截图。
4. 录制：在详情抽屉手动开录 10–20 秒，停止后查 `record_session.quality / actual_qn / danmaku_count`，用 `ffprobe` 核对分辨率/帧率。
5. 轮转：扫码态点「立即检查」，比较 Cookie 长度/哈希/LTP0/设备号，再观看/录制一次。
6. 自愈：恢复旧扫码数据库启动，查日志「已标记过期 → 自动续期完成」与 `credential_refresh`。
7. 泄露：解出 Cookie 敏感值 grep 日志与 URL。
8. 收尾：恢复备份、删测试房间/录制/缓存、退出应用。

---

## 13. 最终判断

斗鱼三态的直播流、录制、登录识别、房间数据、线路都可用；差异只在最高画质与长期凭证。扫码 Cookie 与网页 Cookie 在「能不能播、能不能录、能录多大」上一致，扫码多出的是自动轮转与失效自愈。匿名态能播能录，但最高只有蓝光 4M，且此前 UI 会误导用户选择原画；本次已修复并把「不可用档位置灰」纳入单测。未发现 Cookie 泄露，剩余风险集中在平台策略变化与直播中途授权撤销，两者都不阻塞当前使用。
