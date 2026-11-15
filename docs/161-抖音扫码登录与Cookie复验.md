# 161 抖音扫码登录与 Cookie 复验

- 日期：2026-11-15
- 范围：抖音扫码登录、Cookie 校验、Cookie 自动更新可行性、账号落库与设置页入口
- 环境：macOS 14，release bundle，1166×749 主窗口；真实账号仅哔哩哔哩 uid 352130563、斗鱼 uid 100921751，抖音未登录
- 结论：**抖音扫码登录与 Cookie 校验已落地，自动更新按“定期复验 + 会话保活轮换”实现，不承诺长期 refresh token**。完整手机扫码需要用户本人完成，本轮验证到官方登录页、窗口生命周期、取消/关闭、无效 Cookie 拒绝。

## 1. 为什么没有复刻扫码协议

公开实现（Java 版 `douyin-qrcode-login`、Go 版 `douyin-web-qr-login`）的链路是：

1. 取 ttwid；
2. `GET https://login.douyin.com/passport/web/get_qrcode/` 拿二维码；
3. 每 3 秒 `POST .../check_qrconnect/`；
4. status=3 后跟随 `redirect_url?ticket=...`，收集 Set-Cookie；
5. 用 `user/profile/self` 校验身份。

本轮实测（macOS 出口）：

| 探测 | 结果 |
| --- | --- |
| 旧 `https://sso.douyin.com/get_qrcode/?need_logo=true` | 返回 10168 字节 HTML 风控页，不再是 JSON |
| 旧 `https://sso.douyin.com/check_qrconnect/?token=...` | 同样 HTML |
| 新 `https://login.douyin.com/passport/web/get_qrcode/` 只带 aid | `error_code=4031`「网站存在安全风险」 |
| 新接口 + 完整 web 参数（passport_jssdk_version、p_ui、p_js_v、p_zt、p_bd、p_ts、p_no、biz_trace_id 等） | **HTTP 200 成功**，返回 token 与 base64 二维码；生成二维码不需要 a_bogus |
| 同一套参数请求 `check_qrconnect` | 仍 `4031`；必须带 `a_bogus` 与 `x-tt-session-dtrait` |
| `user/profile/self` 匿名 | `{"status_code":8,"status_msg":"用户未登录"}`，可稳定用于登录态判断 |

`x-tt-session-dtrait` 由 `bdms.1.0.1.20.js` + `dtrait-core.1.0.29.js` 在浏览器环境生成，内含 RSA/AES 与设备采集，体积约 8 MB 且随前端发版变化。复刻它只为了轮询二维码状态，维护成本高、失败面大。

因此改为：**打开 Tauri 子窗口加载抖音官方登录页，让用户在真实 WebView 里扫码/短信验证；登录后由 Rust 通过原生 CookieStore 读取 HttpOnly 的 sessionid**。真实浏览器环境天然通过风控，也自动覆盖扫码、短信二次验证和后续协议变化。

## 2. 后端实现

新增 `src-tauri/src/douyin_auth.rs`：

| 位置 | 作用 |
| --- | --- |
| `douyin_auth.rs:34` | 登录页 `https://www.douyin.com/login_page?next=...` |
| `douyin_auth.rs:37` | 登录窗口有效期 600 秒 |
| `open_login` (`:164`) | 创建/复用 label=`douyin-login` 的 980×760 子窗口；已是旧尺寸时重设大小并导航回登录页 |
| `cancel_login` (`:176`) | 关闭窗口并清空登录会话时间 |
| `poll_login` (`:202`) | 每 1.6 秒由前端调用；`cookies_for_url("https://www.douyin.com/")` 过滤 `douyin.com` 域并拼 Cookie；检测到 `sessionid`/`sessionid_ss` 后过 `profile/self`，成功才 `store_account` 并关窗返回 confirmed |
| `validate_cookie` (`:327`) | 粘贴 Cookie 与已存凭据的统一校验：缺少 sessionid、status_code=8、无 user 都返回用户可读错误 |
| `refresh_now` (`:621`) | 设置页「立即检查」：强制复验并尝试吸收主页 Set-Cookie |
| `revalidate` (`:633`) | 30 分钟一次只读复验；失效才标记过期并发通知 |
| `maintenance_loop` (`:685`) | 启动 20 秒后进入 30 分钟循环，先复验再按 6 小时节奏同步 Cookie |

关键细节：

- **窗口 Cookie 读取**：`window_cookie` 只取 domain 为空 / `douyin.com` / `*.douyin.com` 的项，其他域一律丢弃；HttpOnly 不影响原生 CookieStore。
- **确认串行**：`CONFIRM_LOCK` 防止连续轮询重复校验/重复落库；等锁后重新读一次窗口 Cookie，避免用户已换号。
- **半登录态不落库**：已有 sessionid 但 profile 仍返回未登录时，在有效期内继续等，不写半套凭据。
- **Cookie 轮换**：`merge_set_cookies` 只接受 `name=value`，空值不覆盖（平台清理 Cookie 时不能误删登录态）；最多吸收主页响应的 Set-Cookie。
- **并发写保护**：复验/轮换写回前持有 `account::credential_write_guard()`，并比较内存中的 Cookie 是否仍是本次读到的值；用户重新扫码或手动换 Cookie 时丢弃旧结果。
- **过期**：写入 `credential.status=expired`，发 `credential_expired` 通知，并刷新账号卡；不会删除 Cookie，重新扫码即可覆盖。
- **老 Cookie 兼容**：没有 sessionid 的旧抖音 Cookie 不参与账号复验，也不会被误标过期；它仍可用于取流预热。

## 3. 自动更新的真实边界

抖音没有公开的 refresh_token 端点，登录 Cookie 里也没有类似 B 站 `ac_time_value`、斗鱼 `LTP0` 的长期凭证。因此实现的是：

| 能力 | 行为 |
| --- | --- |
| 定期复验 | 每 30 分钟请求 profile/self，确认登录态仍有效 |
| 会话保活 | 复验本身持续访问 douyin.com，避免长时间不活跃 |
| Cookie 轮换 | 每 6 小时访问一次主页，吸收服务端下发的 Set-Cookie（ttwid 等） |
| 失败退避 | 网络/接口异常写 failed，30 分钟后重试；不影响已存 Cookie |
| 失效处理 | 明确标记 expired 并提示重新扫码，不伪造“已续期” |
| 手动 Cookie | 只有含 sessionid 才被视为可复验；否则只校验一次，不进入自动循环 |

「自动更新」在这里的准确含义是**同步平台轮换的 Cookie + 保持会话活跃**，不是无限期续签。若后续证实 sessionid 存在刷新端点，可在 `refresh_once` 内增强，不需要改 UI 与落库。

## 4. 取流侧接入

原先 `src-tauri/src/platform/douyin.rs` 的 `enter` 只读取页面响应里的 ttwid，不读账号凭据，所以“登录了但取流仍匿名”。本轮在 `douyin.rs:66` 起：

1. 用 `credential::cookie_for_url(page_url)` 取已登录且状态为 ok 的抖音 Cookie（过期态自动不注入）；
2. 请求 `https://live.douyin.com/{web_rid}` 时带上；
3. 把响应 Set-Cookie 里的新 ttwid 与账号 Cookie 用 `credential::merge` 合并；
4. enter 请求带合并后的完整 Cookie。

这样登录态才会真正影响清晰度；匿名路径（无凭据）行为不变。

## 5. 前端与 IPC

- `src-tauri/src/commands.rs` 新增 `douyin_login_open`、`douyin_login_poll`、`douyin_login_cancel`、`douyin_refresh_now`，确认登录后清空分组缓存并重检画质。
- `src-tauri/src/lib.rs:318` 注册四个命令；`src-tauri/src/state.rs:151` 启动独立维护循环，与 B 站/斗鱼循环互不影响。
- `src/lib/ipc.ts:554` 新增四个 invoke 包装。
- `src/features/settings/AccountSection.tsx`：抖音卡增加「扫码登录」；面板标题「在抖音登录窗口完成登录」，128px 区域显示 DOUYIN 与窗口状态，提示「会打开抖音官方登录页；扫码或短信验证后自动读取登录态」；到期/关窗后按钮为「重新打开登录窗口」。
- 抖音不做本地倒计时判失效：二维码由官方页面自己刷新，必须让后端轮询到点关闭窗口；否则窗口会留在前台却不再被轮询。
- i18n 新增 11 个 key（中英），当前 828 keys。

## 6. 真机验证

| 场景 | 操作 | 结果 |
| --- | --- | --- |
| 打开登录窗口 | 设置 → 账号 → 抖音「扫码登录」 | 日志 `打开抖音官方登录窗口`；新窗口 980×760，加载官方页，左侧扫码、右侧验证码/密码登录完整可见 |
| 取消 | 面板「取消」 | 子窗口立即关闭，主窗口回到账号卡；前端停止轮询 |
| 手动关窗 | 点登录窗口红色关闭按钮或 Cmd+W | 下一轮 poll 返回 expired；面板左侧状态改为「窗口已关闭 / 请重新打开登录窗口」，右侧红字「登录窗口已关闭；如需继续请重新点击扫码登录」，按钮变为「重新打开登录窗口」 |
| 无效 Cookie | 粘贴 `acf_uid=1; acf_auth=fake` 保存 | 日志 `ERROR ui: 界面错误 context=ipc:credential_save 抖音 Cookie 缺少 sessionid，请通过扫码重新登录`；DB 未新增 douyin 凭据，B 站/斗鱼凭据未变 |
| 窗口尺寸 | 实测 | 460 宽时官方页右侧被裁；980 宽后完整显示，最小 820×600 |

未完成：**手机扫码并确认登录**需要用户本人操作；因此“登录成功 → sessionid 读取 → profile 校验 → 落库 → 取流升档”的完整闭环本轮没有真实账号证据。代码路径与失败分支已覆盖，建议首次使用时按面板提示完成一次扫码。

## 7. 测试与构建

- `cargo fmt --check`、`cargo check --all-targets`（0 警告）、`cargo test --lib`：**340 passed / 0 failed / 17 ignored**。
- 新增单测：Cookie 合并覆盖/新增/忽略空值、sessionid 识别、profile uid/nickname/嵌套头像解析、缺 uid 拒绝、失败退避与 6 小时周期。
- `pnpm exec tsc -b`、`check:i18n`、`check:docs`、`pnpm build`、`pnpm app:build` 通过。

## 8. 对 B 站 / 斗鱼 / 全局的影响

- B 站、斗鱼代码路径未改；只在 `account.rs` 增加 `"douyin"` 分支，新增独立 `douyin_auth` 模块与维护循环。
- 抖音维护循环启动 20 秒后执行，B 站/斗鱼循环各自独立；Cookie 写锁为全局，但只在写回时短暂持有，和既有 B 站/斗鱼写回共用同一把锁，语义一致。
- 抖音取流侧只在有有效凭据时附加 Cookie；无凭据/过期时与原逻辑一致。
- 设置页账号卡仍是一平台一个账号；抖音卡未登录时只显示扫码与粘贴 Cookie。

## 9. 后续

1. 用真实抖音账号完成一次扫码，确认 profile 返回 uid/nickname/头像、账号卡显示已登录、录制画质是否升档。
2. 观察 6 小时后主页是否下发新的 ttwid，以及 UI「上次检查 / 上次轮换」是否正确。
3. 若需要“真正的长期续期”，先抓登录后 Cookie 集合与 passport 静默登录请求，确认是否存在 refresh 端点；没有就保持当前“复验 + 重新扫码”语义。
