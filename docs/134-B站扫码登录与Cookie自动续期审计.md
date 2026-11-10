# 134 · B 站扫码登录与 Cookie 自动续期审计

> 适用范围：审计 HIVECAST 当前 B 站登录实现，判断扫码、Cookie 保存、自动续期是否完整，并给出可落地的实现方案。
> 结论先行：**扫码主流程可用，但自动续期完全未实现；扫码成功时的取 Cookie 方式存在一个必须在真机确认的高风险疑点。**
> 调研日期：2026-11-10。本文写作时只做审计与方案；对应实现已在同日落地，见 [135](135-B站扫码取态与Cookie自动续期实现.md)。

---

## 1. 结论摘要

| 能力 | 现状 | 判断 |
| --- | --- | --- |
| 二维码生成 | `bilibili_qr_start` 调 generate，本地渲染 SVG | ✅ 正确 |
| 扫码状态映射 | 0 / 86101 / 86090 / 86038 四态 | ✅ 正确 |
| 扫码成功取登录态 | 只从响应 `Set-Cookie` 取，再合并旧 Cookie | ⚠️ 高风险，需真机确认 |
| 登录态校验 | `nav` 校验，`-101` 归为用户失效 | ✅ 正确 |
| Cookie 加密落库 | AES-256-GCM，密钥 0600 | ✅ 正确 |
| Cookie 按域注入 | 按 host 后缀匹配，CDN 不带 | ✅ 基本正确 |
| 定时健康检查 | 每 30 分钟 `nav` 复验 | ✅ 有，但不是续期 |
| `refresh_token` 保存 | 完全丢弃 | ❌ 缺失 |
| Web Cookie 自动续期 | 没有实现 | ❌ 缺失 |
| 续期失败回退 | 没有 | ❌ 缺失 |

一句话：**现在的「自动续期」实际上是「30 分钟复验」**。复验只能发现失效，不能延长有效期；真正续期需要扫码时拿到的 `refresh_token`，而当前代码在反序列化时把它静默丢掉了。

---

## 2. 当前实现链路

### 2.1 扫码

实现集中在 `/Library/dshproject/HIVECAST/src-tauri/src/account.rs`：

- `account.rs:99` `bilibili_qr_start()`：GET `https://passport.bilibili.com/x/passport-login/web/qrcode/generate?source=main-fe-header`，只反序列化 `url` 与 `qrcode_key`。
- `account.rs:143` `bilibili_qr_poll()`：GET `/x/passport-login/web/qrcode/poll`，用 `get_json_with_headers` 同时拿 JSON 与 `Set-Cookie`。
- 状态映射：`code=0` confirmed、`86101` pending、`86090` scanned、`86038` expired。
- confirmed 分支（`account.rs:184-198`）：`cookies_from_set_cookie` → 与旧 Cookie 合并 → `bilibili_nav` 校验 → `store_account` 落库。
- `account.rs:209` `cookies_from_set_cookie`：每条 `Set-Cookie` 只取第一个分号前的 `name=value`，丢弃 `Path/Expires`，不做 URL 解码。
- `account.rs:229` `bilibili_nav`：GET `https://api.bilibili.com/x/web-interface/nav`，`-101` → `AppError::User`，其他错误 → `AppError::Platform`。
- `account.rs:268` `store_account`：`account_id` 固定空串，`cookie` 为合并结果，`status="ok"`。

### 2.2 存储与分发

- `db.rs:31` credential 表：`platform / account_id / cookie / uid / account_name / avatar / status / updated_at`。
- **没有** `refresh_token`、`ac_time_value`、Cookie 过期时间字段。
- `secret.rs:48` `seal`：AES-256-GCM，nonce + 密文 base64，前缀 `enc:v1:`；密钥文件 0600。
- `credential.rs:51` `cookie_for_url` 按 URL host 找平台；`credential.rs:57` 只匹配 bilibili.com / b23.tv、douyu.com、huya.com、douyin.com；CDN `bilivideo.com` 不带 Cookie。
- `credential.rs:102` `normalize` 兼容 `Cookie:` 前缀、平台中文前缀、多行、去重保留最后一个；`credential.rs:150` `merge` 后者覆盖同名。

### 2.3 复验

- `account.rs:342` `revalidate`：只遍历 `platform=="bilibili"`；`nav` 成功才更新资料；只有 `AppError::User` 才标 `expired`；标过期时发 `AppNotice kind=credential_expired`。
- 请求返回后比对 `credential::get`，避免覆盖用户刚粘贴的新 Cookie；成功后清流缓存。
- `state.rs:118` 启动时 `load_from_db`；`state.rs:123-128` 启动后立即 `revalidate`，之后每 30 分钟一次。
- `account.rs:24` `REVALIDATE_INTERVAL = 30 * 60s`。

**注意**：`revalidate` 没有调用 `cookie/info`，也没有调用任何刷新接口；它只判断「现在还能不能用」。

---

## 3. 发现的问题

### P0-1 扫码成功可能不只靠 Set-Cookie（必须先真机确认）

`account.rs:5` 的模块注释写的是「登录态只出现在响应的 Set-Cookie 里」。这句话目前没有实证，而且与公开实现不一致：

- `bilibili-api-python 17.4.1` 的扫码登录从 poll 返回的 `data.url` 查询串里解析 `SESSDATA / bili_jct / DedeUserID`，不是只读 `Set-Cookie`。
- 如果确认态只给 `data.url` 而不给 `Set-Cookie`，当前 `cookies_from_set_cookie` 会得到空串，`merge` 后退化成旧 Cookie；`nav` 可能登到旧账号，或者直接失败。
- 更隐蔽的情况：`Set-Cookie` 只给部分字段（例如只有 `SESSDATA`），缺 `bili_jct` 时 `nav` 仍可能通过，但后续自动续期和投喂类接口会失败。

**处置**：用一次性测试账号真实扫一次，抓 poll 的响应头与响应体，只记录字段名、不记录值；确认后补两条取态路径：

1. 先取 `Set-Cookie`；
2. 为空或字段不全时，解析 `data.url` 的查询串，取 `SESSDATA / bili_jct / DedeUserID / DedeUserID__ckMd5 / sid`。

合并后必须校验至少存在 `SESSDATA + bili_jct + DedeUserID`，再进 `nav`。

### P0-2 `refresh_token` 与 `timestamp` 被 serde 静默丢弃

`account.rs:41-50` 的 `PollData` 只有 `code / message / expires_in`。真实 poll 响应（2026-11-10 实测未扫码态）是：

```json
{"code":0,"message":"OK","ttl":1,"data":{"url":"","refresh_token":"","timestamp":0,"code":86101,"message":"未扫码"}}
```

确认态会在 `data` 里带非空 `refresh_token` 与 `timestamp`。当前结构体没有这两个字段，serde 默认忽略未知字段，所以 token 在内存里就被丢掉了，后面不可能续期。

### P1-3 「30 分钟复验」不是「自动续期」

`revalidate` 的价值是发现失效并提示用户，不是延长登录。文档 `docs/132-登录与Cookie.md:16` 的能力矩阵把「30 分钟复验（HIVE）」放在「自动续期」行，容易让人误判已经支持续期。

**处置**：文档与 UI 都拆成两个词：

- **复验 / 健康检查**：30 分钟一次 `nav`；
- **续期 / 刷新**：按 `cookie/info` 的 `data.refresh` 决定是否轮换 Cookie。

### P1-4 poll 响应没有 `expires_in`，现有注释和单测是错的

真实未扫码响应没有 `expires_in`；`account.rs:157-161` 把它当可选字段、缺省 0，行为上没错（前端用本地 180 秒倒计时），但：

- `account.rs:5` 附近与 `account.rs:502-511` 的单测都在暗示 poll 会返回剩余秒数；
- 这会让后续维护者误以为服务端会驱动过期时间。

**处置**：把注释改成「poll 不保证返回 expires_in，二维码有效期以本地 `QR_TTL_SECONDS=180` 为准」；单测保留「缺字段按 0」的兼容性断言即可，不要写成接口事实。

### P1-5 「能过 nav」不等于「能自动续期」

`account.rs:294` `save_cookie` 对 B 站只要求 `nav` 通过，不检查 `bili_jct` 是否存在，也不要求 `ac_time_value / refresh_token`。用户手工粘贴一条「能看视频但缺 csrf」的 Cookie 时，账号会显示正常，但刷新一定失败。

**处置**：保存 B 站 Cookie 时额外检查 `bili_jct`；缺少时允许保存，但账号卡标注「自动续期不可用，建议重新扫码」。

### P2-6 数据库没有续期字段

现有 credential 表没有 `refresh_token` / `ac_time_value` / 过期时间。

有两种落地方式：

| 方案 | 改动 | 优点 | 缺点 |
| --- | --- | --- | --- |
| A. 合成进 Cookie 字符串 | `ac_time_value=<token>` 作为普通 Cookie 字段，随现有 AES 加密保存 | 零迁移、复用现有存储与注入 | 语义混在 Cookie 里，需要注释清楚 |
| B. 新增 DB 列 | 加 `refresh_token`、`cookie_expires_at` | 语义清晰，可单独轮换 | 需要迁移、改 IPC 与序列化 |

建议 v1 用 A：`ac_time_value` 本来就是 B 站 Web Cookie 的一个字段，浏览器也会把它放在 Cookie 里发给 B 站；放在同一加密串里不增加暴露面。等续期稳定后再考虑拆列。

---

## 4. 自动续期机制（已实证的协议）

### 4.1 五个端点

来自 `bilibili-api-python 17.4.1` 的 `bilibili_api/data/api/credential.json`：

| 用途 | 方法 | 端点 | 关键参数 |
| --- | --- | --- | --- |
| 检查是否需要刷新 | GET | `https://passport.bilibili.com/x/passport-login/web/cookie/info` | 带当前 Cookie |
| 校验登录 | GET | `https://api.bilibili.com/x/web-interface/nav` | 带当前 Cookie |
| 取 refresh_csrf | GET | `https://www.bilibili.com/correspond/1/{correspondPath}` | 带当前 Cookie，buvid3 可随机 |
| 刷新 Cookie | POST | `https://passport.bilibili.com/x/passport-login/web/cookie/refresh` | `refresh_csrf`、`csrf`、`source=main_web`、`refresh_token` |
| 确认刷新 | POST | `https://passport.bilibili.com/x/passport-login/web/confirm/refresh` | `csrf`（新 bili_jct）、`refresh_token`（旧 ac_time_value） |

### 4.2 correspondPath 的生成

不是普通 URL 编码，而是 RSA：

1. 取当前毫秒时间戳；
2. 拼字符串 `refresh_{timestamp_ms}`；
3. 用 B 站内置公钥做 **RSA PKCS#1 OAEP + SHA-256** 加密；
4. 输出小写十六进制，作为 `/correspond/1/{hex}` 的路径。

公钥来自 B 站 Web 前端，长度为 1024 位；`bilibili-api-python` 把它硬编码在 `network.py` 里。Rust 侧需要 `rsa 0.9`（`sha2` feature）+ `sha2`，随机数可用现有 `rand` 或 `getrandom`。

### 4.3 刷新流程

```text
GET cookie/info
  → data.refresh == false：结束（本次不需要刷新）
  → data.refresh == true：
       correspondPath = hex(RSA_OAEP_SHA256("refresh_" + now_ms))
       GET /correspond/1/{correspondPath}
         → HTML 中 <div id="1-name">REFRESH_CSRF</div>
       POST /x/passport-login/web/cookie/refresh
         form: csrf=bili_jct
               refresh_csrf=REFRESH_CSRF
               refresh_token=ac_time_value
               source=main_web
         → Set-Cookie: 新 SESSDATA / bili_jct / DedeUserID
         → JSON data.refresh_token: 新 token
       POST /x/passport-login/web/confirm/refresh
         form: csrf=新 bili_jct
               refresh_token=旧 ac_time_value
       → 合并新 Cookie，替换 ac_time_value，落库
```

### 4.4 关键细节

- 请求 `correspond` 页与刷新接口时，`buvid3` 可以覆盖成随机 UUID；这是公开实现的做法，用来避免设备指纹干扰。
- 刷新响应必须同时满足 **HTTP 200** 且 JSON `code==0`；只看 HTTP 状态不够。
- 新 Cookie 从刷新响应头取；新 `refresh_token` 从 JSON 的 `data.refresh_token` 取。
- 确认接口用的是 **新 bili_jct** + **旧 refresh_token**，这两个不能混。
- 扫码/密码登录返回的 `data.refresh_token` 就是后续的 `ac_time_value`，不是另一套 OAuth。

---

## 5. 可行性判断与公开项目

### 5.1 技术上可以实现

可以，而且不需要浏览器、不需要第三方服务：

- 用户本人扫码，拿到 `refresh_token`；
- 本地用 RSA 公钥生成 `correspondPath`；
- 调 B 站 Web 自己的三个接口完成轮换；
- 全过程只在用户机器上，Cookie 仍然 AES 加密。

### 5.2 参考项目对比

| 项目 | 做了什么 | 能否直接用于 HIVECAST |
| --- | --- | --- |
| `bililive-go` | poll 代理把 `refresh_token/timestamp` 透传给前端；`handler.go:4100-4116` 只保留 SESSDATA/bili_jct/DedeUserID 等 | ❌ 只保存，不自动刷新 |
| `biliLive-tools` | `packages/shared/src/task/bili.ts:1199-1280` 用 `TvQrcodeLogin.refresh` 每 24h 检查、到期 <10 天刷新 | ❌ 这是 TV 端 OAuth（accessToken/refreshToken），不是 Web SESSDATA |
| `bilibili-api-python 17.4.1` | Web Cookie 刷新流程完整：`check_refresh / refresh / _getCorrespondPath / _get_refresh_csrf / _confirm_refresh` | ⚠️ 只能作协议参考，**GPL-3.0-or-later，不能复制代码** |
| `pexcn/bilibili-cookie-refresher` | 脚本依赖 `bilibili_api` | ❌ 上游仓库已关停 |
| `yumoii789/bilibili-cookie-login` | 浏览器扩展，同账号 Cookie 变化时同步 | ❌ 不调用刷新接口 |

**结论**：没有成熟的 Rust 实现可以直接用；唯一完整的协议样本是 GPL 的 Python 包。HIVECAST 应当**独立实现**，只依据公开协议与自己的抓包结果，不移植其代码。

### 5.3 合规与风险

- 2026-07，B 站已对 `bilibili-API-collect`、`bilibili-api` 等逆向仓库发律师函并关停；公开资料在收缩。
- 自动续期使用用户本人扫码得到的 token、调用 B 站 Web 自身接口，不等于绕过付费/DRM，但仍可能违反平台用户协议或触发风控。
- 建议：**做成可关闭的本地能力，默认只在有 `refresh_token` 时启用；限频、失败退避、不共享账号、不内置任何 token。**
- UI 与文档要写清楚：续期失败时回退到重新扫码，不承诺永久在线。

---

## 6. 推荐实现方案

### Phase 1：修正扫码取态（P0）

1. `PollData` 增加 `refresh_token: String`、`timestamp: i64`。
2. confirmed 分支先取 `Set-Cookie`；为空或缺关键字段时解析 `data.url` 查询串。
3. 合并后校验 `SESSDATA / bili_jct / DedeUserID` 三项；缺一项就报「登录信息不完整」，不要落库成 ok。
4. 把 `refresh_token` 以 `ac_time_value=<token>` 追加进 Cookie 字符串（若已存在则替换）。
5. 保留 `timestamp` 仅用于日志/诊断，不参与刷新路径（刷新用当前时间）。

### Phase 2：刷新引擎（P1）

新增 `account::refresh_bilibili_cookie(db, app)`：

1. 从 Cookie 解析 `ac_time_value`、`bili_jct`；缺失直接返回 `NotRefreshable`。
2. GET `cookie/info`；`data.refresh==false` 返回 `Fresh`。
3. 生成 `correspondPath`；GET `correspond` 页；解析 `refresh_csrf`。
4. POST `cookie/refresh`；同时读响应头 `Set-Cookie` 与 JSON。
5. POST `confirm/refresh`；用新 `bili_jct` + 旧 token。
6. 写回前比对 `credential::get` 快照；用户已重新登录则丢弃本轮结果。
7. 成功后 `db.save_credential` + `credential::set` + `clear_stream_cache`。

需要新增 HTTP helper：现有 `platform/http.rs:330` 的 `post_form_json` 硬编码 `Origin=https://www.douyu.com`，不能复用；新增一个可传 `Origin/Referer/Cookie`、返回 `(json, set_cookies)` 的 POST 表单方法。

### Phase 3：调度与失败策略（P1）

- 启动：`load_from_db` → `refresh_if_needed` → `revalidate`。
- 周期：刷新检查每 12 小时一次，加 0–30 分钟抖动；`nav` 复验仍 30 分钟。
- 不要每 30 分钟打刷新接口，降低风控。
- 网络/5xx/风控：保留 `ok`，指数退避（1h → 6h → 24h）。
- `-101` 或明确 token 失效：标 `expired`，发一次通知，停止自动重试。
- 刷新 POST 成功但 confirm 失败：**保留新 Cookie 与新 token**，下次只重试 confirm；不能回滚到旧值，否则可能两边都失效。
- 单飞锁：定时与手动刷新共用一把锁，避免两个请求同时轮换 token。

### Phase 4：UI 与文档（P2）

- 账号卡增加一行状态：`自动续期：可用 / 不可用（缺少 refresh_token） / 上次续期时间`。
- 续期成功不弹 toast，只更新 `updated_at`；失败按严重度提示。
- 文档拆分「复验」与「续期」，修正 `docs/132` 的矩阵与 `account.rs` 注释。

### Phase 5：测试

单测：

1. `PollData` 能解析 `refresh_token/timestamp`；
2. `data.url` 查询串解析（含百分号编码、重复参数、缺字段）；
3. `correspondPath` 对固定时间戳的已知向量（用测试公钥或固定密文长度断言）；
4. Cookie 合并时 `ac_time_value` 替换而不是重复追加；
5. confirm 表单使用「新 bili_jct + 旧 token」；
6. 写回前快照不一致时丢弃结果。

真机：

1. 一次性测试账号扫码，确认 poll 实际返回的 Cookie 载体；
2. 等 `cookie/info` 返回 `refresh=true`，跑一次完整刷新；
3. 用旧 Cookie 请求 `nav` 应失败，用新 Cookie 应成功；
4. 手动断网/改错 token，确认状态不被误标过期；
5. 刷新过程中重新扫码，确认旧结果不覆盖新账号。

---

## 7. 验收清单

- [ ] 真实扫码确认 `Set-Cookie` 与 `data.url` 哪个承载登录态；
- [ ] `PollData` 保存 `refresh_token`，Cookie 内含 `ac_time_value`；
- [ ] 账号卡能显示「自动续期可用/不可用」；
- [ ] `cookie/info` 返回 false 时不发刷新请求；
- [ ] 刷新成功一次，新旧 Cookie 的 `nav` 行为符合预期；
- [ ] confirm 失败时保留新凭据并可重试；
- [ ] 用户重新登录时旧刷新结果不覆盖；
- [ ] 文档不再把「复验」写成「续期」。

---

## 8. 参考

- HIVECAST：`src-tauri/src/account.rs`、`credential.rs`、`secret.rs`、`db.rs`、`platform/http.rs`、`state.rs`。
- `bilibili-api-python 17.4.1`（PyPI wheel，仅协议参考，GPL-3.0-or-later）：`bilibili_api/data/api/credential.json`、`bilibili_api/utils/network.py`、`bilibili_api/login_v2.py`。
- `bililive-go`：`src/servers/handler.go:4100-4116`、`src/servers/bilibili_login_test.go:22`。
- `biliLive-tools`：`packages/shared/src/task/bili.ts:1199-1280`。
- 实测：2026-11-10 本机 curl，QR generate / poll 未扫码态 / cookie/info 无 Cookie。
