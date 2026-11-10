# 135 · B 站扫码取态修正与 Cookie 自动续期

> 状态：**已实现并完成真机扫码验证**（2026-11-10）。承接 [134](134-B站扫码登录与Cookie自动续期审计.md) 的审计结论，本文记录实际落地、协议细节、测试与真机验证进度；实际 Cookie 轮换仍待 B 站返回 `refresh=true`。登录唤醒、手动检查、最近结果持久化与 key_stale 提示见 [136](136-B站Cookie续期状态与手动检查.md)。
> 关联：[132-登录与Cookie](132-登录与Cookie.md)、[39-M14-Cookie加密存储](39-M14-Cookie加密存储.md)。

---

## 1. 结论先行

| 能力 | 改动前 | 改动后 |
| --- | --- | --- |
| 扫码成功取登录态 | 只读响应 `Set-Cookie` | Set-Cookie + poll 的 `data.url` 双路径，合并后强校验三项 |
| `refresh_token` | serde 静默丢弃 | 扫码写入 `ac_time_value`，随 Cookie 加密落库 |
| 自动续期 | 无 | 后台 12 小时检查 `cookie/info`，需要时走 refresh/confirm |
| 续期失败 | 无 | 指数退避 1h → 6h → 24h，单飞锁 + 写回前快照比对 |
| 账号卡 | 无续期状态 | B 站卡显示「自动续期可用/不可用」 |
| 数据库迁移 | — | **不需要**，refresh token 直接放在已有 Cookie 字段里 |

一句话：**「30 分钟复验」仍然保留，但现在 B 站 Web Cookie 真正能自己续期了；扫码也不再赌 Set-Cookie 一定完整。**

---

## 2. 扫码取态修正

### 2.1 poll 响应不再丢字段

`PollData` 现在包含：

- `code / message / expires_in`：原有扫码状态；
- `url`：登录态落地页；
- `refresh_token`：后续续期凭据；
- `timestamp`：服务端签发时间，仅用于日志排查。

对应位置：`src-tauri/src/account.rs` 的 `struct PollData`。

### 2.2 双路径合并

确认扫码后依次执行：

1. `cookies_from_set_cookie(&fetched.set_cookies)` 读响应头；
2. `cookies_from_poll_url(&poll_data.url)` 读落地 URL 的查询串；
3. 两段用 `credential::merge` 合并，后者覆盖同名项；
4. 与本地旧 Cookie 合并（保留 `buvid3` 等设备字段）；
5. `has_login_cookies` 必须同时存在 `SESSDATA / bili_jct / DedeUserID`，否则直接报错「登录信息不完整」；
6. 通过后才 `bilibili_nav` 校验身份并落库。

`cookies_from_poll_url`（`account.rs:303`）有三个刻意选择：

- **不做 URL 解码**：`SESSDATA` 里的 `%2C` 是服务端原值，解码后反而可能和 Set-Cookie 不一致；
- 只接受 `SESSDATA / bili_jct / DedeUserID / DedeUserID__ckMd5 / sid`，避免把无关查询参数写进请求头；
- 同名参数保留最后一个，和 `merge` 的覆盖语义一致。

### 2.3 换号不继承旧 token

扫码返回的 `refresh_token` 为空时，先把旧 Cookie 里的 `ac_time_value` 去掉，再合并新登录态。否则新账号会带着旧账号的续期凭据，轻则续期失败，重则把新 Cookie 写坏。

非空时用 `with_refresh_token` 覆盖为新值，不会追加出两个同名项。

---

## 3. 自动续期协议

实现入口：`account.rs:402` `refresh_bilibili_cookie(&Db, &AppHandle) -> Result<RefreshOutcome, AppError>`。

### 3.1 四步流程

1. **检查**：`GET https://passport.bilibili.com/x/passport-login/web/cookie/info`，带当前完整 Cookie。
   - `code=0, data.refresh=false` → 本轮 `Fresh`，不轮换；
   - `code=-101` → `AppError::User`；
   - 其他 code → `AppError::Platform`。
2. **生成 correspondPath**：`hex(RSA-OAEP-SHA256("refresh_" + 当前毫秒))`，公钥为 B 站固定的 1024 位 PEM（`REFRESH_PUBLIC_KEY`）。
3. **取一次性 csrf**：`GET https://www.bilibili.com/correspond/1/{path}`，从 `<div id="1-name">…</div>` 中取 `refresh_csrf`。请求带原 Cookie 并确保有 `buvid3`（没有就临时生成 UUID，避免设备指纹干扰）。
4. **换 Cookie**：`POST https://passport.bilibili.com/x/passport-login/web/cookie/refresh`，表单为 `csrf=旧 bili_jct`、`refresh_csrf`、`refresh_token=旧 ac_time_value`、`source=main_web`。响应 JSON 给新 `refresh_token`，`Set-Cookie` 给新的 `SESSDATA / bili_jct / DedeUserID`。
5. **确认**：`POST .../confirm/refresh`，必须使用**新 bili_jct + 旧 refresh_token**；顺序错会失败。
6. **写回**：新 Cookie 先与旧 Cookie 合并，再用新 token 替换 `ac_time_value`；确认失败也保留新 Cookie，只记 warn。

### 3.2 关键安全点

- 续期成功后清空平台流缓存（`clear_stream_cache`），避免继续用旧 Cookie 签出的 CDN 地址。
- 写库前重新读取内存中的 B 站 Cookie，与本次开头的快照比较；不一致说明用户刚扫码/退出登录，本轮结果直接丢弃，返回 `Discarded`。
- 全流程由 `static REFRESH_LOCK: tokio::sync::Mutex<()>` 串行化，避免启动检查、手动操作和定时任务同时轮换。
- Cookie 仍是 AES-256-GCM 加密落库，日志只记录 `refreshable` 布尔值，不打印 token。
- 续期不新增数据库列：`ac_time_value` 只是 Cookie 串里的一项，`credential::has_refresh_token` 判断 `ac_time_value + bili_jct` 是否齐全。

### 3.3 状态机

`RefreshOutcome`：

| 值 | 含义 | 调度行为 |
| --- | --- | --- |
| `NoCredential` | 本机没有 B 站账号 | 按成功间隔继续等 |
| `NotRefreshable` | 缺 `ac_time_value` 或 `bili_jct` | 不做请求，继续等 |
| `Fresh` | 服务端说还不需要刷新 | 12 小时后再说 |
| `Refreshed` | 已轮换并写回 | 12 小时后再说 |
| `Discarded` | 期间用户改了 Cookie | 本轮丢弃，不覆盖 |

已标记 `expired` 的账号也仍会尝试：`cookie/info` 若明确 `-101` 会失败并按退避重试；若 refresh token 仍有效，就有机会把账号救回来。

---

## 4. 调度与失败

`refresh_loop`（`account.rs:575`）在应用启动时随复验循环一起启动（`state.rs:125`）：

- 正常间隔：**12 小时** + 0–30 分钟随机抖动；
- 连续失败：第 1 次 1 小时、第 2 次 6 小时、第 3 次起 24 小时，另加 0–5 分钟抖动；
- 任意一次成功把失败计数清零；
- 没有账号或没有 token 时也会空转，但不会发网络请求。

选择 12 小时而不是每小时：B 站 `cookie/info` 的 `refresh` 通常按天级窗口变 true，频繁检查没有收益，还会增加风控暴露。

---

## 5. 账号卡展示

`src/features/settings/AccountSection.tsx` 在 B 站卡片的提示行下增加一行状态：

- 已连接且 `refreshable=true`：灰点 + 「自动续期可用（每 12 小时检查，失效时自动轮换 Cookie）」；
- 已连接但 `refreshable=false`：警告色点 + 「自动续期不可用：缺少 refresh_token，建议重新扫码」；
- 未连接：不显示。

`CredentialInfo` 新增 `refreshable: boolean`，由后端计算，前端不接触 Cookie 值。i18n 新增 `account.autoRefreshOn / autoRefreshOff`。

---

## 6. 改动文件

| 文件 | 内容 |
| --- | --- |
| `src-tauri/Cargo.toml` | 新增 `rsa 0.9`（sha2 feature）、`sha2 0.10`、`rand 0.8` |
| `src-tauri/src/account.rs` | 双路径取态、token 保存、RSA correspondPath、refresh/confirm、调度循环、7 个单测 |
| `src-tauri/src/credential.rs` | `value / without / has_refresh_token` |
| `src-tauri/src/platform/http.rs` | `post_form_json_with_headers`：显式 Origin/Cookie 并读 Set-Cookie |
| `src-tauri/src/state.rs` | 启动后台续期循环 |
| `src-tauri/ipc-types/src/lib.rs` | `CredentialInfo.refreshable` |
| `src/lib/bindings/CredentialInfo.ts` | 同步绑定 |
| `src/features/settings/AccountSection.tsx` | B 站续期可用性一行 |
| `src/i18n/zh-CN.json / en-US.json` | 两条文案 |
| `docs/132 / docs/134` | 修正「复验≠续期」的旧结论，标注实现入口 |

---

## 7. 测试与检查

### 7.1 单测

新增 7 条纯逻辑测试，均在 `cargo test --lib`：

- `refreshable_flag_needs_bilibili_and_both_tokens`：IPC 只在 B 站且 token 齐全时标可续期；
- `parses_poll_refresh_fields`：poll 的 `url / refresh_token / timestamp` 能解析；
- `keeps_raw_percent_encoding_from_poll_url`：保留 `%2C`、忽略白名单外参数、无查询串/空值返回空；
- `poll_cookie_keeps_last_duplicate`：同名取最后一个；
- `refresh_token_is_replaced_not_appended`：替换不追加，`without` 能清掉 token；
- `parses_refresh_csrf_from_correspond_page`：从 `id="1-name"` 提取；
- `correspond_path_encrypts_refresh_timestamp`：生成 1024 位测试密钥，验证 hex 长度 256、私钥解出 `refresh_1700000000123`、真实公钥 PEM 可解析；
- 原有扫码/合并/过期解析测试继续通过。

结果：**261 passed / 0 failed / 17 ignored**。

### 7.2 静态检查

- `cargo check --all-targets`：0 warning；
- `cargo fmt --check`：通过；
- `pnpm exec tsc -b`：通过；
- `pnpm run check:i18n`：740 keys；
- `pnpm run check:docs`：136 files；
- `pnpm build` / `pnpm app:build`：通过（见提交记录）。

---

## 8. 真机验证进度

### 8.1 已完成（2026-11-10）

真机扫码账号：**执事只食芝士**（uid `352130563`），`credential` 行 `status=ok`，账号卡显示「已登录 / 7 个 Cookie / 自动续期可用」。Cookie 名称为：

`SESSDATA / bili_jct / DedeUserID / DedeUserID__ckMd5 / sid / default_sites / ac_time_value`

其中 `ac_time_value` 长度 32；没有 `buvid3 / buvid4`，与 bilibili-api-python 的做法一致：correspond 请求时临时生成 UUID `buvid3`，不要求登录 Cookie 自带。

启动后台检查结果：

- `GET /x/passport-login/web/cookie/info` 返回 `code=0`、`refresh=false`，判定为 `Fresh`，没有轮换；
- 同一轮 `nav` 复验 `checked=1`，账号状态正常；
- 账号卡显示续期可用，说明 `CredentialInfo.refreshable` 在真实数据上为 true。

另外用本机已加密存储的 Cookie 做了一次**只读** correspond 探测（不写库、不打印 Cookie/token）：

- `GET https://www.bilibili.com/correspond/1/{path}` 返回 HTTP 200；
- 页面包含 `<div id="1-name">`，内容为 32 位 token-like 字符串；
- 临时 UUID 形式的 `buvid3` 同样被接受。

这验证了 RSA-OAEP 公钥、`refresh_{ms}`、hex correspondPath 和 `parse_refresh_csrf` 在真实响应上可用。

### 8.2 尚未触发

当前 B 站明确返回 `refresh=false`，所以以下分支仍未真实执行：

1. `cookie/info` 的 `refresh=true` 分支；
2. `cookie/refresh` 返回的 `Set-Cookie` 是否补齐 `SESSDATA / bili_jct / DedeUserID`；
3. `confirm/refresh` 的成功与失败分支；
4. 续期成功后的写回、清缓存、账号卡更新时间变化。

不能为了测试强行轮换真实 token：refresh 会作废旧 token，失败时可能影响当前登录。等 Cookie 临近过期、B 站返回 `refresh=true` 时，后台循环会自动走完整链路；日志关键字为 `B 站 Cookie 续期成功` 或 `自动续期暂时失败`。

### 8.3 验收步骤

- 打开设置 → 账号，确认 B 站卡显示「自动续期可用」；
- 启动日志中搜索 `cookie/info`、`登录态复验完成`；
- 到期窗口内搜索 `B 站 Cookie 续期成功`，确认账号仍是同一 uid；
- 若续期失败，按 1h → 6h → 24h 退避，原 Cookie 不会被未完成的轮换覆盖。

---

## 9. 后续可做

- **登录/更换 Cookie 后唤醒续期循环**：当前循环只在启动时立即跑一次，之后按 12 小时窗口；运行中扫码最坏要等一个窗口才会做第一次 `cookie/info`。新 token 不会立刻过期，但不利于扫码后马上确认续期链路。
- **手动「立即检查续期」**：便于排查，也可避免为了验收而临时改 `REFRESH_INTERVAL`。
- **公钥轮换兜底**：`REFRESH_PUBLIC_KEY` 目前硬编码；若 B 站换公钥，correspond 会失败，需要能识别并提示重新登录。
- 续期成功后主动刷新账号资料（当前只更新 Cookie 与 `updated_at`，昵称/头像变更要等 30 分钟复验）；
- 把「最近一次续期时间/结果」暴露到账号卡，便于用户判断；
- 斗鱼 LTP0、虎牙 Cookie 续期另立文档，不复用 B 站这套 RSA 流程。
