# 132 · 登录、Cookie 与续期

> 适用范围：账号如何进入 HIVECAST、如何被使用、何时失效、如何不泄露。
> 参考：bililive-go 的 B 站/斗鱼扫码与 LTP0 续期、biliLive-tools 的登录路由、streamlink 的「无登录」反例。
> 核心原则：**登录态只用于平台 API 与明确需要的 CDN；按域名注入，绝不全局透传。**

---

## 1. 能力矩阵

| 能力 | B 站 | 斗鱼 | 虎牙 | 抖音 |
| --- | --- | --- | --- | --- |
| 扫码登录 | ✅ HIVE 已实现 | ✅ 参考实现完整，HIVE 未接 | ❌ | ❌ |
| Cookie 粘贴 | ✅ | ✅ | ✅ | ✅ |
| 登录态校验 | `nav` 接口 | 登录探针（follow/top3，`error==0`） | 无稳定接口 | 进入房间接口 |
| 复验 / 续期 | 30 分钟 nav 复验 + 12 小时 Web 续期（HIVE 已实现，见 135）；参考实现见 134 | LTP0（参考实现） | ❌ | ❌ |
| 关键 Cookie | `SESSDATA / bili_jct / DedeUserID / sid` | `acf_auth / acf_uid / acf_stk / acf_ltkid / dy_did` | `yyuid / udb_uid` | `ttwid / sessionid / passport_csrf_token` |
| 过期表现 | `-101` 未登录、画质降档 | 匿名流约 5 分钟被切 | 403/降档 | 签名失败/无 stream |

**streamlink 的定位**：四家插件都只解析公开页面，没有扫码、没有登录态。它证明了「无登录也能取到低画质」，但也解释了为什么 HIVECAST 需要自己的账号层——高画质、付费场、弹幕身份都依赖登录。

---

## 2. 账号模型

```rust
pub struct Account {
    pub platform: PlatformId,
    pub uid: String,          // 平台用户 ID（展示用，不参与鉴权）
    pub display_name: String,
    pub avatar: Option<String>,
    pub cookies: Vec<Cookie>, // name/value/domain/path/expires
    pub logged_in_at: i64,
    pub last_checked_at: i64,
    pub state: AccountState,  // ok | expired | unchecked | error
}
```

存储要求：

- 加密后落库（HIVE 用 AES-256-GCM，密钥文件 0600，位于应用数据目录）。
- 列表接口只返回 `display_name/avatar/state/last_checked_at`，**永不返回 Cookie 值**。
- 日志、错误、崩溃报告统一脱敏：`SESSDATA=***`、`acf_auth=***`、`ttwid=***`。

---

## 3. B 站扫码

### 3.1 流程

```text
GET https://passport.bilibili.com/x/passport-login/web/qrcode/generate?source=main-fe-header
  → { url, qrcode_key }
        │
用户用 B 站 App 扫码
        ▼
GET https://passport.bilibili.com/x/passport-login/web/qrcode/poll?qrcode_key=...&source=main-fe-header
  code=86101 未扫码
  code=86090 已扫码待确认
  code=0     成功（登录态可能在 Set-Cookie，也可能在 data.url 查询串；HIVE
              当前只读 Set-Cookie，待真机确认，见 docs/134）
  code=86038 已过期
        ▼
合并 Set-Cookie → GET https://api.bilibili.com/x/web-interface/nav
  code=0 且 isLogin=true → 保存账号
  否则 → 标记失败，不落库
```

### 3.2 细节

- 轮询间隔 2s，二维码有效期以本地 180s 为准；poll 不保证返回 `expires_in`（2026-11-10 实测未扫码态没有该字段）。
- `Set-Cookie` 可能分多条、带 `Domain=.bilibili.com`；按 name 合并，保留 `expires/max-age`。
- `nav` 校验同时拿 `uname/face/mid`，作为账号展示信息。
- 扫码成功后要立刻做一次 `getRoomPlayInfo` 冒烟，确认登录态真的提升画质；失败只告警，不回滚登录。
- 二维码内容要转成图片给 UI；过期后允许「刷新二维码」，不要复用旧 `qrcode_key`。

### 3.3 过期与复验

HIVE 每 30 分钟做一次 `nav` 健康检查（`account.rs`），每 12 小时做一次 Web Cookie 续期：`cookie/info` 判断是否需要轮换，需要时走 `correspondPath → cookie/refresh → confirm/refresh`，成功后原子替换整组 Cookie 并清流缓存。协议与实现见 [135](135-B站扫码取态与Cookie自动续期实现.md)，审计背景见 [134](134-B站扫码登录与Cookie自动续期审计.md)。

- `nav` 返回 `isLogin=false` → 标记 `expired`，UI 显示「登录已失效」，不自动删除 Cookie（用户可能只是临时风控）。
- 网络错误 → 保持 `ok`，只更新 `last_checked_at`，避免误判。
- 连续 3 次 `expired` → 停止复验，等用户重新登录；后台续期仍会按 1h/6h/24h 退避尝试，token 仍有效时可自动恢复。
- 续期缺 `ac_time_value` 或 `bili_jct` → 不发请求，账号卡提示重新扫码。

---

## 4. 斗鱼扫码（参考实现，HIVE 待接）

### 4.1 流程

```text
POST https://passport.douyu.com/scan/generateCode
  → { expire: 300, url, code }
        │
用户扫码
        ▼
GET https://passport.douyu.com/japi/scan/auth?time={ts}&code={code}
  error = -2 未扫码
  error =  1 已扫码待确认
  error =  0 成功
  error = -1 已失效
        ▼
换票：跟随跳转（最多 8 跳），累积每一跳的 Set-Cookie
        ▼
登录探针：follow/top3，要求 error == 0
        ▼
保存 acf_* 与 dy_did
```

### 4.2 换票细节

- 跳转链可能跨 `passport.douyu.com / www.douyu.com / m.douyu.com`，只接受白名单域名，防止把 Cookie 发给第三方。
- 每一跳都合并 `Set-Cookie`，不要只取最后一跳。
- 最多 8 跳；超过视为失败，避免死循环。
- 换票完成后必须用登录探针验证，不能只看「拿到了 Cookie」。

### 4.3 LTP0 续期

参考实现（bililive-go）的链路：

```text
safeAuth（带 acf_ltkid）
  → POST /api/passport/login
  → 新的 acf_auth / acf_stk / acf_ltkid
```

节奏：

- `acf_*` 有效期约 6 天（529200s）。
- 每 3 天尝试续期一次；失败退避 6 小时；连续失败/明确失效则 24 小时内不再尝试。
- 续期成功要原子替换整组 Cookie，不能只换其中一个。
- 续期失败不影响已保存账号，只把状态标为 `expired`。

---

## 5. 虎牙与抖音

### 5.1 虎牙

- 没有稳定扫码接口；UI 只提供「粘贴 Cookie」。
- 必要字段：`yyuid`、`udb_uid`、`udb_passport` 等；不同登录方式字段不同，保存整组而不是挑字段。
- 校验：请求一次房间页/播放配置，若返回登录态相关字段则视为有效；否则只标记 `unchecked`，不要直接判定失效。

### 5.2 抖音

- 扫码需要 `passport/web/…` 与验证码，维护成本高；v1 只做粘贴 Cookie。
- 必要字段：`ttwid`、`sessionid`、`sessionid_ss`、`passport_csrf_token`。
- `ttwid` 是匿名设备标识，**即使用户不登录也应持久化**；否则每次启动都像新设备，风控概率上升。
- 签名 UA 必须与 Cookie 所属浏览器一致；HIVE 统一使用一个固定 UA，并在账号页提示「不要混用其他客户端」。

---

## 6. Cookie 的使用边界

### 6.1 按域名注入

HIVE 在 `src-tauri/src/credential.rs` 做域名分发：

| 目标域 | 是否带登录 Cookie | 原因 |
| --- | --- | --- |
| `api.live.bilibili.com` | ✅ | 房间信息、取流 |
| `*.bilivideo.com / *.akamaized.net` | ❌ | CDN 不需要，避免泄露 |
| `www.douyu.com / /lapi/` | ✅ | 签名与取流 |
| `*.douyucdn.cn` | ✅ | 匿名流会被切，需登录态 |
| `www.huya.com / `*.huya.com` | ✅ | 页面与配置 |
| `*.huya.com CDN` | ⚠️ 仅 Referer/UA | 按实测决定，默认不带 |
| `live.douyin.com / webcast` | ✅ | 进入房间、取流 |
| `*.douyinpic.com / CDN` | ❌ | 静态资源 |

实现要求：匹配用「后缀 + 点边界」，不能用 `contains`，否则 `evil-bilibili.com` 也会命中。

### 6.2 不进入候选 headers

平台适配层返回的 `StreamCandidate.headers` 只放 UA/Referer/Origin 这类**公开**头；Cookie 由下载层按目标 URL 动态注入。这样：

- 切换 CDN 不会把 Cookie 带过去；
- 日志里不会打印带 Cookie 的完整头；
- 观看与录制共用同一套注入逻辑。

---

## 7. 安全清单

- 密钥文件 0600，目录 0700；不要把密钥写进 DB 或配置文件。
- 加密使用随机 nonce；同一明文每次密文不同。
- 导出/诊断包默认不含 Cookie；如必须包含，需二次确认并脱敏。
- 错误信息过滤 `Cookie/Authorization/token/ttwid/SESSDATA`。
- 前端不持久化 Cookie；刷新后重新从后端取「是否登录」。
- 退出登录要同时删除内存、DB、日志中的引用，并清空该平台的 HTTP Cookie Jar。
- 不要把 Cookie 放进 URL query（会进浏览器历史/代理日志）。

参考实现里的反模式（不要学）：

- biliLive-tools 的 `/user/export` 与 `/recorder/:id` 明文回传登录态；
- 抖音把 Cookie 打进 `console.log` 与错误信息；
- 账号加密密钥硬编码在源码里；
- 本地 HTTP 服务在鉴权前就暴露 webhook/assets。

---

## 8. UI 状态

账号卡至少六态：

| 状态 | 展示 | 操作 |
| --- | --- | --- |
| 未登录 | 「未登录」 + 扫码/粘贴 | 扫码、粘贴 Cookie |
| 等待扫码 | 二维码 + 倒计时 | 刷新、取消 |
| 已扫码待确认 | 「请在手机上确认」 | 取消 |
| 已登录 | 头像 + 昵称 + 最近校验 | 重新登录、退出 |
| 已过期 | 「登录已失效」 + 时间 | 重新登录 |
| 校验失败 | 「暂时无法校验」 | 重试 |

不要把「网络错误」显示成「登录失效」；用户会重复扫码。

---

## 9. 实测清单

用 ego-browser + 项目根 `Cookie.txt`：

1. B 站：扫码 → nav 校验 → 取流画质是否从 250 提升到 10000。
2. B 站：手动删 `SESSDATA` → 30 分钟内是否标记 expired。
3. 斗鱼：扫码 → 换票 → 探针 → 取流是否不再 5 分钟被切。
4. 斗鱼：LTP0 续期后旧 Cookie 是否全部替换。
5. 虎牙/抖音：粘贴 Cookie → 重启 app → 是否仍有效。
6. 任意平台：录制日志中是否出现 Cookie 明文（应为 0 次）。
7. 任意平台：CDN 请求头中是否带登录 Cookie（按 §6.1 预期）。

---

## 10. 变更记录

| 日期 | 版本 | 变更 |
| --- | --- | --- |
| 2026-11-10 | v1.0 | 首版：能力矩阵、B 站扫码、斗鱼扫码/LTP0、Cookie 边界、安全与 UI 状态 |
