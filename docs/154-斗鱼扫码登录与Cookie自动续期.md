# 154 · 斗鱼扫码登录、Cookie 校验与自动续期

> 范围：`src-tauri/src/douyu_auth.rs`、`src-tauri/src/account.rs`、`src-tauri/src/commands.rs`、`src-tauri/src/state.rs`、`src-tauri/src/db.rs`，以及设置页账号卡与 `src/lib/ipc.ts`。
> 结论：**斗鱼现在有三条账号路径：扫码登录（带 LTP0，可自动续期）、手动粘贴 Cookie（只校验、不续期）、过期后的重新扫码。** 斗鱼不再接受任意非空 Cookie；B 站原有扫码/续期链路未改动。
> 时间：2026-11-13；2026-11-14 补充扫码请求生命周期复审。基线：`cabe840` + 真实账号验收与文案修正。
> 真机：真实扫码登录、强制续期、原画 2K60 录制均通过（见 §7.5）；二维码过期/刷新、隐藏窗口暂停轮询见 §11。

---

## 1. 改造前的状态

斗鱼在账号体系里只是一个「能粘贴 Cookie 的平台」：

- `account::save_cookie` 对非 B 站平台走兜底分支，不请求平台接口，任意非空字符串都能写入；
- `info_from_row` 只给 B 站计算 `refreshable`，斗鱼永远是 `false`；
- 没有二维码入口，没有续期循环，也没有登录态复验；
- 设置页斗鱼卡只有「粘贴 Cookie」，没有「扫码登录」。

这条路径的问题不是「不能录」，而是**错误不可见**：Cookie 过期后仍显示已登录，直到录制/弹幕请求失败才暴露；用户也没有比手动抓 Cookie 更简单的恢复方式。

B 站已经定稿的扫码、Cookie 校验、自动续期、失效通知链路保持不变；斗鱼使用独立模块和独立维护循环，避免两边节奏互相影响。

---

## 2. 公开资料与协议结论

参考的公开实现与讨论：

- `douyu-qrcode-login`：扫码生成、轮询、确认后跟随回调拿 `acf_*` 的完整链路；
- `bililive-go` PR 1210：斗鱼登录态与录制画质的关系、Cookie 失效后的降级行为；
- `biliup` PR 1807：斗鱼扫码登录与 Cookie 持久化的工程取舍。

这些实现共同指向同一个结论：

1. 斗鱼网页登录态的核心是 7 个 `acf_*` Cookie：`acf_uid`、`acf_auth`、`acf_stk`、`acf_ltkid`、`acf_username`、`acf_biz`、`acf_ct`；
2. `acf_*` 本身寿命较短，网页端不会被动轮换；
3. 扫码确认时 passport 会下发长期凭证 `LTP0`（数月有效），配合设备号 `dy_did` 可以通过 `safeAuth` 换一套新的 `acf_*`；
4. 手动粘贴的 Cookie 通常没有 `LTP0`，只能校验，不能自动续期。把旧 `LTP0` 套到新 Cookie 上会污染账号，必须明确禁止。

因此本实现把「能否续期」定义为「是否持有该账号扫码时保存的 LTP0」，而不是「是否有一个看起来非空的 Cookie」。

---

## 3. 扫码链路

### 3.1 生成二维码

`douyu_auth::qr_start`：

```text
POST https://passport.douyu.com/scan/generateCode
form: client_id=1&isMultiAccount=0
Referer: https://passport.douyu.com/index/login
Origin:  https://passport.douyu.com
X-Requested-With: XMLHttpRequest
```

响应要求 `error == 0`，并从 `data` 取：

- `code`：轮询键，只允许 8–64 位 ASCII 字母数字、`-`、`_`；
- `url`：二维码内容；
- `expire`：秒数，缺省 300，夹在 60–900 秒。

二维码 SVG 由 `account::render_qr` 生成，前端拿到的 `QrTicket` 与 B 站同构。

### 3.2 轮询

`douyu_auth::qr_poll` 请求：

```text
GET https://passport.douyu.com/japi/scan/auth?time=<毫秒>&code=<code>
```

`error` 映射：

| error | state | 含义 |
| --- | --- | --- |
| -2 | `pending` | 等待扫码 |
| 1 | `scanned` | 已扫码，等待手机确认 |
| 0 | `confirmed` | 已确认，开始跟随回调 |
| -1 | `expired` | 二维码过期 |
| 其他 | `error` | 展示平台 `msg` 或通用提示 |

HTTP 429 单独返回「请求过于频繁」，不污染扫码状态。

### 3.3 确认后的回调链

确认响应里的 `data.url` 不能只看最后一跳：**登录 Cookie 会分散在中间 302 的 `Set-Cookie` 里**。

`follow_chain` 手动跟随最多 9 跳：

- 每一跳都收集 `Set-Cookie`，只取 `name=value`，丢弃 `Path/Expires/HttpOnly` 等属性；
- 空值不覆盖已有值，避免平台用 `name=; Max-Age=0` 清 Cookie 时误删登录态；
- 只允许 `https` 且主机为 `douyu.com` 或 `*.douyu.com`；
- 中间跳转缺 `Location`、跳数过多、跳到外部域名都直接报错。

回调结束后：

1. 合并所有 `Set-Cookie`，要求 7 个 `acf_*` 齐全；
2. 用 `acf_uid` 做账号 ID，`acf_nickname`（URL 解码后）或 `acf_username` 做显示名；
3. `LTP0` 从 Cookie 串中移除，单独加密存 `credential_secret`；
4. 设备号优先取 `dy_did`，其次 `acf_did`、`acf_devid`；
5. 保存凭据，清空旧的续期失败状态，发送 `account://refresh`。

没有 `LTP0` 时不报登录失败，但会清掉旧 secret 并提示「自动续期不可用」；这是平台的真实降级路径，不应伪装成可续期。

---

## 4. Cookie 校验

手动粘贴路径 `account::save_cookie` 对斗鱼调用 `douyu_auth::validate_cookie`：

```text
GET https://www.douyu.com/wgapi/livenc/liveweb/follow/top3
Referer: https://www.douyu.com/
Cookie: <用户粘贴的 Cookie>
```

分类规则在 `classify_probe`：

- HTTP 401/403 → `Invalid`；
- HTTP 2xx 且 JSON `error == 0` → `Ok`；
- HTTP 2xx 且 JSON `error != 0` → `Invalid`；
- 非 2xx、非 JSON（风控页/HTML 登录页）、网络错误 → `Transient`。

**风控页不能当成确定失效**，否则一次网络抖动就会把用户的登录态踢掉。`Transient` 只提示「接口暂时不可用，无法校验」，不写库。

保存成功后：

- 斗鱼手动 Cookie 会清空旧 `credential_secret`，防止旧账号的 LTP0 污染新 Cookie；
- 返回的 `CredentialInfo.refreshable` 由 `has_refresh_token` 决定，手动 Cookie 为 `false`。

---

## 5. 自动续期

### 5.1 safeAuth 换票

`safe_auth` 只带 `LTP0`（以及配套设备号）：

```text
GET https://passport.douyu.com/lapi/passport/iframe/safeAuth
    ?client_id=1&t=<毫秒>&_=<毫秒>&callback=cb
Cookie: LTP0=<...>; dy_did=<...>
Referer: https://www.douyu.com/directory/myFollow
```

同样手动跟随回调链，收集新的 `acf_*`。拿到完整 Cookie 后，再用 `follow/top3` 探针确认；只有探针 `error == 0` 才算续期成功。这样不会把「safeAuth 返回 200 但其实是登录页」当成成功。

### 5.2 合并与写回

`merge_refreshed_cookie` 以旧 Cookie 为基底，合并新 `acf_*`：

- 不把 `LTP0` 写回普通 Cookie 串；
- 如果旧 Cookie 已有设备号，保留旧设备号，避免每次续期都换设备；
- 旧 Cookie 没有设备号时，才从新响应里补一个并回写 secret。

写回前再次读取当前 `LTP0`，与本次换票使用的值比较；不一致说明用户在这期间重新扫码/删除，本次结果直接丢弃（`RefreshOutcome::Discarded`）。

### 5.3 节奏与退避

| 场景 | 下次尝试 |
| --- | --- |
| 续期成功 | 3 天后 |
| 临时失败（网络/风控/不完整 Cookie） | 6 小时后 |
| LTP0 明确失效（401/403/业务 error） | 24 小时后，并提示重新扫码 |
| Cookie 提前过期但 LTP0 仍可用 | 下一轮维护立即尝试恢复，不受 3 天限制 |
| 手动 Cookie（无 LTP0） | 只写一次 `not_refreshable`，不反复刷新检查时间 |

`maintenance_loop` 启动后等 30 秒，然后每 30 分钟：

1. 先 `revalidate`：用当前 Cookie 打 `follow/top3`，5 分钟内已检查则跳过；失败只标记过期，不删凭据；
2. 再 `refresh_once`：按上面的节奏决定是否 safeAuth；若 `credential.status=expired` 且上次续期不是 `failed`，则忽略 3 天周期，立即用 LTP0 尝试恢复。失败后仍按 6 小时/24 小时退避，避免风控期间每 30 分钟重试。

顺序不能反：`refresh_once` 对手动 Cookie 会写 `checked_at`，如果先跑，`revalidate` 的 5 分钟闸门会永远跳过手动 Cookie 的探活。

### 5.4 失效处理

`mark_expired` 在凭据写锁内把 `credential.status` 改为 `expired`，同步内存状态，并发送 `credential_expired` 通知。设置页显示：

- 「登录已失效」；
- 「需要重新登录」；
- 「斗鱼长期凭证已失效，请重新扫码登录」。

手动 Cookie 过期同样会被 `revalidate` 标记，不依赖 LTP0。

---

## 6. 代码落点

| 文件 | 内容 |
| --- | --- |
| `src-tauri/src/douyu_auth.rs` | 二维码、轮询、回调链、Cookie 校验、safeAuth、续期节奏、复验、维护循环、单测 |
| `src-tauri/src/account.rs` | `info_for_row` 按平台计算 `refreshable` 与续期状态；斗鱼保存时校验 Cookie 并清旧 secret |
| `src-tauri/src/commands.rs` | `douyu_qr_start` / `douyu_qr_poll` / `douyu_refresh_now`；扫码确认后清取流缓存并重检画质 |
| `src-tauri/src/state.rs` | 启动独立的斗鱼 `maintenance_loop` |
| `src-tauri/src/db.rs` | `credential_secret` 的加密存取与删除测试 |
| `src/lib/ipc.ts` | 三个斗鱼 IPC 封装 |
| `src/features/settings/AccountSection.tsx` | 斗鱼扫码面板、轮询、立即检查、未登录/无 LTP0 文案 |
| `src/i18n/{zh-CN,en-US}.json` | 斗鱼专属扫码与续期提示 |

关键函数：

- `qr_start() -> Result<QrTicket, AppError>`
- `qr_poll(db, app, code) -> Result<QrPoll, AppError>`
- `validate_cookie(cookie) -> Result<Account, AppError>`
- `refresh_once(db, app, force) -> RefreshOutcome`
- `revalidate(db, app)`
- `maintenance_loop(db, app)`

---

## 7. 真机验证

环境：macOS，release bundle，真实数据目录 `/Users/joftal/Library/Application Support/com.hivecast.desktop/hivecast.db`；测试前备份为 `/tmp/hivecast-before-douyu.db`。

### 7.1 二维码生成与取消

- 设置 → 账号 → 斗鱼「扫码登录」；
- 日志 `INFO 斗鱼扫码二维码已生成 expires_in=300`；
- UI 显示二维码、倒计时「4:57 后失效」、等待扫码；
- 修正后标题为「用斗鱼 App 扫码」，提示「登录后可自动续期，录制高清画质」；
- 取消后二维码面板关闭，不影响其他平台。

### 7.2 无效 Cookie

- 输入 `acf_uid=1; acf_auth=fake` 保存；
- 日志 `ERROR 界面错误 context=ipc:credential_save 斗鱼 Cookie 已失效或未登录`；
- toast「操作失败 / 斗鱼 Cookie 已失效或未登录」；
- DB 未写入斗鱼凭据，`credential_secret` 仍为 0 行。

### 7.3 伪造 LTP0 的失效分类

为了在不接触真实斗鱼账号的前提下验证续期失败路径，测试库中写入了：

```text
credential:        platform=douyu, uid=1, account_name=测试斗鱼, status=ok
cookie:            acf_uid/acf_auth/acf_stk/acf_ltkid/acf_username/acf_biz/acf_ct
credential_secret: refresh_token=fake-ltp0, device_id=dy_did=fake-device
```

启动约 30 秒后：

- 日志 `WARN 斗鱼登录态已失效，已标记过期`；
- 卡片「登录已失效 / 需要重新登录」；
- 点「立即检查」后显示「上次检查 11/13 22:30 · 需要重新登录」「斗鱼长期凭证已失效，请重新扫码登录」；
- DB `credential_refresh`：`outcome=failed`、`key_stale=1`、`message=斗鱼长期凭证已失效，请重新扫码登录`；
- DB `credential.status=expired`。

这证明：safeAuth 失败分类、状态落库、过期标记、前端呈现和手动检查是同一条真实链路，不是只在前端写死。

### 7.4 B 站回归

- 点 B 站「立即检查」：`INFO 手动检查 Cookie 续期 platform="bilibili"`，随后「暂不需要续期」；
- toast「续期检查完成：暂不需要续期」；
- B 站卡片仍显示 7 个 Cookie、已登录、自动续期可用；
- 未出现斗鱼凭据导致的 B 站状态串台。

### 7.5 真实扫码登录与续期

2026-11-13 由用户手机真实扫码完成「生成二维码 → 手机确认 → 回调写入凭据」全链路，随后在 release bundle 上验证：

- 斗鱼卡显示真实昵称「番茄炒芝士」、已登录、18 个 Cookie、自动续期可用（每 3 天自动续期，失败后自动重试）；
- DB `credential`：`platform=douyu`、`uid=100921751`、`status=ok`、Cookie 长度 1791；
- DB `credential_secret`：LTP0 长度 327、设备号长度 87，两列均以 `enc:v1` 加密落库；普通 Cookie 串由 `merge_login_cookies` 主动跳过 LTP0；
- DB `credential_refresh`：`outcome=refreshed`、`key_stale=0`、`message=已自动续期`。

真实续期：22:51 首次登录后，22:59 点击「立即检查」：

- Cookie 密文 SHA-256 从 `aef8ab5a…` 变为 `acb78133…`，证明 `acf_*` 已实际轮换；
- LTP0 密文 SHA-256 保持 `ec29669c…`，证明长期凭证未丢失；
- 日志 `DEBUG hivecast_lib::broker: 账号凭据变更，已清空取流与房间信息缓存`、`INFO hivecast_lib::douyu_auth: 斗鱼 Cookie 自动续期完成`；
- 卡片更新为「上次检查 11/13 22:59 · 已轮换 Cookie」。

真实录制：添加 `https://www.douyu.com/9999` 并开启自动录制：

- 取流 `qn=40000`，实际 `actual_qn=10000`，画质 `原画2K60 · FLV`，线路 13；
- ffmpeg 请求头只带 `dy_did`，因为 CDN 域是 `huos1a.douyucdn2.cn`，账号 Cookie 不跨域发送；登录态在签名 URL 的 token 中（含 `uid 100921751`），这是预期设计，不是漏带 Cookie；
- 录制 42 秒后退出应用：`status=aborted`、`reason=app_exit`、23,260,607 B、78 条弹幕、ASS 78 行，分片合并成功。

尚未等待 3 天让定时任务自然触发；本次「立即检查」与定时任务共用 `refresh_once`，已验证换票、合并写回、缓存失效与状态更新。

---

## 8. 边界与限制

1. **完整扫码需要用户手机确认**：自动化只能验证到二维码与轮询，不能代替手机端确认；本次已由用户真实扫码完成并验证。
2. **手动 Cookie 不自动续期**：这是明确降级路径；卡片会说明「没有长期凭证」，不会假装可续期。
3. **LTP0 与账号绑定**：保存新手动 Cookie 时会清空旧 secret，避免旧账号票据污染新账号。
4. **设备号不主动轮换**：续期时优先保留旧设备号，减少平台侧设备变化；只有旧 Cookie 没有设备号时才补写。
5. **风控页不当失效**：HTML、非 JSON、5xx 都归为临时失败，6 小时后重试；只有明确的 401/403 或业务 `error != 0` 才标记过期。
6. **手动 Cookie 的「上次检查」不空转**：`not_refreshable` 只写一次，避免每 30 分钟刷新时间、掩盖真实过期。
7. **B 站独立**：斗鱼使用独立模块、独立锁、独立维护循环；B 站的 `refresh_loop`、扫码、Cookie 校验未改。
8. **短效 Cookie 提前过期**：维护循环先 `revalidate` 再 `refresh_once`；若短效 Cookie 已过期但 LTP0 仍有效，会先标记一次「登录失效」，同一轮立即续期并恢复为 ok。若续期本身失败，才按 6 小时/24 小时退避。
9. **手动 Cookie 的「立即检查」不探测**：没有 LTP0 时只返回「不可续期」，不会用当前 Cookie 请求接口；这是为了避免把无长期凭证的账号误报成可自动续期。
10. **扫码回调落库前不再探测**：7 个必填 Cookie 齐全即落库，回调来源可信；若账号恰好被风控，要等下一次 5 分钟探测才会转为失效。
11. **斗鱼账号无头像**：界面用「DY」缩写占位；平台接口未返回稳定头像地址，暂不伪造。

---

## 9. 测试

- `cargo test --lib`：**317 passed / 0 failed / 17 ignored**；
- 斗鱼新增单测覆盖：轮询状态映射、二维码 code 白名单、`Set-Cookie` 属性过滤与空值、7 个必填 Cookie、三种设备号、昵称 URL 解码、3 天/6 小时/24 小时节奏、回调只允许斗鱼 HTTPS、风控页不判失效、续期保留旧设备号、确认 code 只处理一次；
- `db.rs` 新增 `credential_secret_is_sealed_and_deleted_with_the_credential`：secret 落库加密、读回明文、删除凭据后 secret 一并删除；
- 前端：`tsc -b` 通过，`check:i18n` 807 keys，`check:docs` 155 files，`pnpm build` 通过；release bundle 31.74 MiB；
- 真实账号验收：扫码登录、强制续期、原画 2K60 录制、78 条弹幕全部通过。

---

## 10. 测试数据清理

早期伪造 LTP0 的测试库已用 `/tmp/hivecast-before-douyu.db` 恢复，随后才进行真实扫码；真实凭据必须保留，不能用旧备份覆盖。真实账号验收结束后的清理：

1. 退出 app，确认无 `hivecast` / `ffmpeg` 进程；
2. 删除测试房间 `douyu:9999` 与对应 `record_session`；
3. 删除 `Recording/斗鱼/yyfyyf` 下的测试 flv/jsonl/ass 与空目录；
4. 保留 `credential`、`credential_secret`、`credential_refresh` 中的真实斗鱼与 B 站记录；
5. 确认 `room=0`、`record_session=0`，DB 无 `-wal` / `-shm` 残留。

---

## 11. 扫码请求生命周期复审（2026-11-14）

### 11.1 复审范围

本轮只审查扫码登录的请求生命周期，不改变 B 站链路：

- 打开二维码后，谁在轮询、多久一次、并发时会不会叠加；
- 关闭窗口到托盘、离开账号页、取消二维码时，请求是否停止；
- 二维码过期、刷新轮换后，旧 code 是否还会继续请求；
- 手机确认后重复轮询同一个 code，会不会重复走回调、重复写账号；
- 异常（网络错误、HTTP 429/5xx、回调失败）之后的停表与恢复。

### 11.2 请求模型

扫码轮询是**前端驱动**的，后端 `douyu_auth::qr_poll` 本身不保存定时器：

- 打开二维码后，前端每 1.6 秒调用一次 `douyu_qr_poll`；
- 单飞标志 `inFlight` 保证上一次没回来时不会叠出第二个请求；后端 `dy_client` 超时 15 秒，即使网络挂起，最多只有一个在途请求；
- 每次请求约 615 B 入 / 423 B 出（`nettop -p <pid>` 实测），5 分钟二维码生命周期合计约 190 KB；
- `confirmed`、`expired`、后端 `error` 三种终态会卸载定时器；网络异常不会立即停表，下一轮会自动重试。

### 11.3 已修的三个生命周期问题

**问题 1：关闭窗口后仍空转轮询。**

关闭到托盘只调用 `window.hide()`，WebView 不卸载，React 的 `setInterval` 继续跑。修复：

- `lifecycle::show_main` / `hide_main` 统一发送 `app://window-visibility` 事件；
- macOS `RunEvent::Reopen` 不再自己 `show/set_focus`，与托盘一起走 `lifecycle::show_main`，避免两条唤回路径只有一条发事件；
- `AccountSection` 订阅该事件，`!windowVisible` 时轮询 effect 直接返回；重新显示后 effect 重新挂载、自动续上。

真机证据（pid 15463，二维码可见时）：

- 可见时 `nettop` 1 秒增加约 615 B in / 423 B out；
- 点窗口关闭后，6 次采样字节数完全不变（63,557 / 29,555），日志 `关闭窗口，继续后台运行`；
- `open -a HIVECAST.app` 触发 Reopen 后，日志 `收到 macOS Reopen，重新显示主窗口`，字节数恢复增长，二维码面板与倒计时仍在。

**问题 2：手机确认后的重复轮询会重复走回调。**

前端在 confirmed 后立即 `setQr(null)`；如果第一次响应在网络上丢失，前端会继续轮询同一个 code。原实现每次 `error == 0` 都会重新跟随回调链、重新 `store_account`、重新写 LTP0。修复：

- `QR_CONFIRM_LOCK` 串行化整个确认后处理（回调 → 7 个 `acf_*` 校验 → 落库 → secret）；
- `QR_CONFIRMED` 记录最近 32 个已处理 code；重复请求直接返回 `state=confirmed`、`credential=None`，不再触网回调；
- 只有完整成功后才 `remember_confirmed`；回调失败仍可重试，不会被错误地标记为已登录。

**问题 3：过期二维码的「刷新二维码」可被连点。**

`openQr` 会先 `qrSeq += 1` 并清空旧面板，但按钮未禁用时连续点击会并发调用 `generateCode`。现在刷新按钮加 `disabled={busy}`，与「扫码登录」共用同一个 busy 状态；旧响应仍由 `qrSeq` 代次丢弃。

### 11.4 过期、刷新与取消的真机验证

2026-11-14 08:03:13 生成二维码（`expires_in=300`）：

| 时刻 | 界面 | 网络 | 结论 |
| --- | --- | --- | --- |
| 08:03–08:08 | 「等待扫码」+ 倒计时 | 每 1.6 秒稳定增长 | 正常轮询 |
| 08:09:05 | 「二维码已过期，请刷新」+ 刷新按钮 | 5 次采样字节数不变 | 本地到点即停表 |
| 08:10:01 点刷新 | 新二维码「等待扫码」 | 日志再次 `二维码已生成 expires_in=300`，字节数恢复增长 | 旧 code 被替换，新 code 正常轮询 |
| 08:10 后点取消 | 二维码面板关闭，只剩「扫码登录」 | 5 次采样字节数不变 | 取消即卸载轮询 |
| 关闭窗口再 Reopen | 面板与倒计时保留 | 隐藏期间不变，显示后恢复增长 | 可见性事件生效 |

离开账号页到「关于」页同样会卸载 `AccountSection`，轮询停止；回到账号页二维码按设计不保留，需要重新扫码。

### 11.5 仍保留的边界

1. **HTTP 429 / 5xx 会停表**：后端把它们映射成 `state=error`，前端显示「请求过于频繁/接口异常」和刷新按钮，不会自动重试。这是有意保守：平台明确限流时继续 1.6 秒轮询只会加重风控，交给用户刷新更稳。
2. **网络层异常仍按 1.6 秒重试**：连接失败、超时没有退避；单飞保证不会叠加，但长时间断网会持续产生失败请求。后续如需省电，可在前端加指数退避，不影响当前正确性。
3. **重复确认只做了单测**：`confirmed_code_is_processed_only_once` 验证同一个 code 只记一次；真实「两次并发确认」需要手机在同一时刻重复确认，本轮未构造。锁与去重路径已在代码层覆盖。
4. **去重表是进程内、无 TTL**：最多 32 条，按插入顺序淘汰；应用重启后清空。二维码本身只有 5 分钟，重启后旧 code 也不会再出现在界面上。
5. **关闭瞬间仍在途的请求**：前端会丢弃迟到的响应，但后端可能已经完成登录并发出 `account://refresh`；账号卡仍会通过事件刷新，不会出现「已登录但界面不更新」。

### 11.6 本轮验证

- `cargo fmt`、`cargo check --all-targets` 通过；`cargo test --lib` **317 passed / 0 failed / 17 ignored**；
- 新增 `qr_code_validation_rejects_unsafe_input`，覆盖合法 code、过短、空格、路径穿越、超长与非 ASCII；
- 前端 `tsc -b`、`check:i18n` 807 keys、`check:docs` 155 files、`pnpm build` 通过；
- release bundle 31.74 MiB，真机验证见 §11.3–§11.4。
