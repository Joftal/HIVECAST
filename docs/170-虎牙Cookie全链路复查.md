# 虎牙 Cookie 全链路复查

- 日期：2026-11-17
- 范围：虎牙扫码登录请求流程、二维码生命周期、关闭/刷新清理、登录落库、Cookie 自动续期真实性与异常/性能边界。
- 结论：扫码轮询是单飞 1.6 秒、后端 15 秒超时，正常网络不会叠请求；二维码 225 秒过期，过期/取消/关闭面板现在会显式释放后端 session。Cookie 自动续期是真实调用虎牙 `cookieExchange`，不是本地假刷新；本次真机两次返回 `write_flag=false changed=false`，历史真机一次返回 `write_flag=true changed=true`、9 个 Set-Cookie，说明“有新 Cookie 就落库、没有就保持”两条路径都真实存在。

## 1. 本次复查方法

1. 静态审查 `src-tauri/src/huya_auth.rs` 的扫码、换票、校验、续期、复验、失效标记全部函数。
2. 用真实 DB 的虎牙登录态单独调用 `checkLogin`，确认 Cookie 明文、登录态和 Set-Cookie 行为。
3. 用 release bundle 真机生成二维码、取消面板、立即检查，记录运行日志与界面状态。
4. 补单测覆盖取消、Cookie 合并、二维码状态。

## 2. 扫码请求流程与性能

### 2.1 生成二维码

`qr_start`（`huya_auth.rs:439`）只做两次网络请求：

1. `POST /qrLgn/getQrId`（uri 70001）拿 `qrId`；
2. `GET /qrLgn/getQrImg` 拿 PNG，再转成 SVG 返回前端。

设备 Cookie 由 `new_device_jar()` 本地生成，不先打开网页，也不走 WebView。二维码有效期 `QR_TTL=225` 秒（`huya_auth.rs:40`），会话存在内存 `QR_SESSIONS`，最多 16 条。

### 2.2 轮询

`qr_poll`（`huya_auth.rs:529`）每次只发一个 `POST /qrLgn/tryQrLogin`（uri 70003），返回 stage：

| stage | 含义 | 处理 |
| --- | --- | --- |
| 0/4 | 等待扫码 | pending，继续轮询 |
| 1/10 | 已扫码待确认 | scanned，继续轮询 |
| 2/3 | 登录成功 | 换票 + checkLogin + 落库 |
| 5 | 二维码过期 | 删除 session，expired |
| 6 | 二维码失效 | 删除 session，expired |
| 7 | 用户取消授权 | 删除 session，expired |
| 8 | 扫码账号异常 | error，不落库 |

前端每 1.6 秒轮询一次，`inFlight` 单飞；后端 HTTP 超时 15 秒。即使网络慢到 15 秒，同一时刻也只有一个在途请求，不会因为 `setInterval` 叠出并发风暴。轮询本身不写日志，避免每 1.6 秒刷一行。

### 2.3 本次补上的取消/清理

原实现只有 `expire_session` 内部清理，没有对前端的取消入口：用户点“取消”、刷新二维码、关闭设置覆盖层时，前端只是停止轮询，后端 session 要等 10 分钟清理阈值才消失。

本次新增：

- 后端 `pub fn qr_cancel(qr_id)`（`huya_auth.rs:521`）：幂等删除指定 session，不访问网络；已确认结果仍保留在 `QR_CONFIRMED`。
- Tauri 命令 `huya_qr_cancel`（`commands.rs:2279`，注册在 `lib.rs:321`）。
- 前端 `huyaQrCancel`（`ipc.ts:587`），并在 `AccountSection.tsx` 的以下时机调用：
  - `closeQr` 点取消（`:181`、`:187`）；
  - `openQr` 刷新/换平台前释放旧二维码（`:195`）；
  - 生成请求返回时序号已过期（`:207`）；
  - 组件卸载/切换二维码（`:248`）；
  - 本地倒计时先到点（`:264`）。

另外 `qr_poll` 在 stage 2/3 网络返回后会再查一次 session（`huya_auth.rs:586` 附近）；`finish_login` 落库前也会再查（`:719` 附近）。用户取消后，即使换票请求已经在途，也不会在检查点之后继续落库。

### 2.4 异常与边界

- 网络失败：`post_huya`/`get_huya` 返回“虎牙接口暂时不可用”，前端显示错误，不把二维码误判为过期。
- 二维码 stage 6/7：直接删除 session 并返回 expired，用户可重新生成。
- 服务端时间与本地倒计时：服务端是权威，每轮用 `expires_in` 重新对齐本地倒计时；本地到点只先展示过期，同时释放 session。
- 关闭应用：`QR_SESSIONS` 是进程内存，进程退出即释放，不会留下后台线程。

## 3. 登录成功后的请求链

`confirm_login`（`huya_auth.rs`）有两条分支：

- 普通分支：`loginTime` + `sign` → `huyaCallback` 吸收 Set-Cookie；
- 快速登录分支：真实扫码返回 `uid` + `biztoken`，直接写 `udb_uid`/`udb_biztoken`，不再请求 `quickTicket`（该接口对已消费的 biztoken 返回 10052）。

`finish_login` 最多 3 次 `checkLogin`，每次间隔 300ms；每次把 checkLogin 的 Set-Cookie 合并回当前设备 jar，然后才落库。成功后：

1. `store_account` 写 `credential`（Cookie 在 DB 里是 AES-256-GCM 密文）；
2. `save_secret` 单独加密保存 `udb_cred`；
3. `remember_confirmed` 保留成功结果；
4. `expire_session` 删除二维码 session；
5. 清空取流缓存并广播账号刷新。

## 4. Cookie 自动更新真实性

### 4.1 链路

`refresh_once`（`huya_auth.rs:1066`）：

1. 读 `credential` + `credential_secret`；
2. 判断 `refresh_due`（每 20 分钟，失败按 6 小时/24 小时退避）；
3. 取 `REFRESH_LOCK` 后重读凭据，避免扫码/删除插队；
4. `POST /cookieExchange`（uri 70015）；
5. `checkLogin` 验证；
6. 缺 `udb_cred` 时补回长期凭证；
7. 仍失败则 `credLogin`（uri 70017）再试；
8. 保存 Cookie + secret + `credential_refresh` 状态。

### 4.2 真机证据

- 2026-11-17 12:38：`cookieExchange` 返回 `write_flag=true changed=true cookies=9`，日志“虎牙 Cookie 自动续期完成 changed=true rotated=true udb_cred_len=152”，DB `credential_refresh` 为 `refreshed`。这是真实轮换。
- 2026-11-17 15:06 与 15:15：启动后自动续期、手动“立即检查”各一次，均返回 `write_flag=false changed=false cookies=1`，日志 `changed=false rotated=false`，界面“上次检查 15:15 · 暂不需要续期”。说明服务端这次没有下发新 Cookie，应用没有伪造“已轮换”。
- 真实 `checkLogin` 单独探测返回 `{"isLogined":true,"userName":"hy_139432760","uid":"1199573670219"}`，本次没有 Set-Cookie；登录态有效。

### 4.3 这次修掉的链路完整性缺口

- **响应体限流**：原 `response_json/get_huya/fetch_qr_png` 先 `resp.text()/bytes()` 全读再检查 2MB，实际限不住；现在 `read_body_limited`（`huya_auth.rs:221`）先看 Content-Length，再按 chunk 累加，超限立即中断。
- **日志脱敏**：解析失败不再打印 body 前 200 字符，避免换票响应里的 `biztoken` 进日志。
- **checkLogin Set-Cookie**：原来直接丢弃；现在 `check_login_with_cookies`（`:803`）返回 jar，登录确认、手动 Cookie、续期都会合并。
- **手动 Cookie 降级**：如果 `udb_uid/udb_biztoken` 已过期但 `udb_cred` 仍有效，原实现直接判失效；现在 `prepare_cookie`（`:922`）会先 `credLogin` 再校验。
- **复验计时**：新扫码/手动保存后重置 `REVALIDATED_AT`（`:1383`），新账号不再沿用旧账号的 6 小时窗口。
- **续期状态竞态**：写 `credential_refresh` 前重新持写锁并比较当前 Cookie；扫码登录插队时丢弃旧续期结果，避免旧状态覆盖新登录。

## 5. 真机与单测

- release bundle：32.48 MiB，进程 71701。
- 真机生成二维码：日志“虎牙扫码二维码已生成 expires_in=225”，界面显示二维码、等待扫码、3:42 后失效、取消按钮正常。
- 真机点取消：面板立即关闭，旧二维码不再显示。
- 真机等到 15:26:55 自然过期：面板保留并显示“二维码已过期，请刷新”，出现“刷新二维码”按钮，没有把面板吞掉。
- 点“刷新二维码”：15:36:29 生成新二维码，日志再次“虎牙扫码二维码已生成 expires_in=225”，倒计时从 3:42 重新开始；旧 session 只经 `qr_cancel`+单测验证，未在调试器里直接读 `QR_SESSIONS`。
- 真机点虎牙“立即检查”：日志 `cookieExchange 完成 write_flag=false changed=false`，卡片“上次检查 15:15”。
- `cargo test --lib`：382 passed / 0 failed / 19 ignored；huya_auth 模块 12 passed / 1 ignored。
- `cargo fmt`、`cargo check --all-targets` 0 警告；`tsc -b`、i18n 837、docs 171 通过。

## 6. 仍然不能证明的边界

1. 没有做“旧 Cookie 停止续期后必然失效”的破坏性实验；只能证明服务端真实返回过新 Cookie 并落库，以及本次两次返回“无变化”。
2. 本轮没有用户手机再次扫码；上一轮已完成真实扫码登录与落库，本轮只验证生成/取消/过期/刷新展示。
3. `checkLogin` 通常会重发同一组 Cookie；Set-Cookie 删除语义（Max-Age=0）仍按插入空值处理，未做专门删除建模。
4. `QR_CONFIRMED` 保留最近 32 个成功 key，用于重复 poll；进程退出才释放。
5. 取消发生在 `store_account` 之后、`remember_confirmed` 之前的极小窗口，仍可能完成落库；已在换票后、落库前两个检查点降低概率。
6. 旧二维码 session 是否立刻从内存消失，只通过 `qr_cancel` 单测、刷新后不再轮询旧 key、后端检查点保证；没有在运行中的进程里直接 dump `QR_SESSIONS`。

## 7. 文件清单

- `src-tauri/src/huya_auth.rs`：`read_body_limited`、`qr_cancel`、`check_login_with_cookies`、`prepare_cookie`、续期状态竞态、复验计时。
- `src-tauri/src/commands.rs`、`src-tauri/src/lib.rs`：`huya_qr_cancel` 命令与注册。
- `src/lib/ipc.ts`、`src/features/settings/AccountSection.tsx`：取消/刷新/卸载清理。
