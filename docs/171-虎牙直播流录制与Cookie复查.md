# 虎牙直播流、录制、画质与 Cookie 全链路复查

> 日期：2026-11-17
> 范围：虎牙带 Cookie / 匿名下的直播间数据、取流、录制、画质、线路、弹幕；扫码 Cookie 与网页 Cookie.txt 的差异；Cookie 轮转与请求泄露；对 B 站/斗鱼/抖音的回归判断。
> 结论先行：**扫码 Cookie、网页 Cookie、匿名三种来源在直播间数据、8 档画质、6 条线路上返回完全一致；1080P 与 2K HDR 录制稳定；4K HDR 目前只能写出约 4–5 秒就被对端结束，应用会重试 3 次后暂停 5 分钟，属于本次发现的主要缺口。**
>
> 后续（docs/172）：复查确认 660000 当时处于重播（`is_replay=1`），其 4K 源各线路均 403；上面的 4K 结论只适用于该重播房间，不代表正在直播的 4K 房间。现已把重播判为未开播，并对 4K 做实际取流探测。

## 1. 结论表

| 项目 | 扫码 Cookie | 网页 Cookie.txt | 匿名 |
| --- | --- | --- | --- |
| 登录账号 | uid 1199573670219 / hy_139432760 | 同一账号 | 无 |
| Cookie 数量 | 17 个 / 941 B | 36 个 / 1881 B | 0 |
| 直播间信息 | 正常 | 正常 | 正常 |
| 画质档位 | 8 档（与另两组一致） | 8 档 | 8 档 |
| 线路组 | 6 组 | 6 组 | 6 组 |
| 1080P 取流/录制 | 正常 | 探针一致，未单独跑整场 | 探针一致 |
| 2K HDR 录制 | 正常 | 探针一致，未单独跑整场 | 探针一致 |
| 4K HDR 录制 | 4–5 秒后 EOF | 未单独跑 | 未单独跑 |
| Cookie 轮转 | cookieExchange 成功 | 手动 Cookie 无长期凭证，不轮转 | 不适用 |

## 2. Cookie 来源对比

Cookie.txt 不是 Netscape 格式，而是四行「平台标签：请求头」的文本；虎牙行 1884 B，去掉标签后 36 个 Cookie。扫码登录落库为 1299 B 密文 / 941 B 明文，17 个 Cookie：

```text
PHPSESSID, __yamid_new, guid, sdid, udb_appid, udb_biztoken, udb_cred,
udb_deviceid, udb_guiddata, udb_origin, udb_passdata, udb_passport,
udb_status, udb_uid, udb_version, username, yyuid
```

网页 Cookie 多出 19 个浏览器侧字段（节选）：

```text
SoundValue, __yamid_tt1, __yaoldyyuid, __yasmid, _qimei_fingerprint,
_qimei_uuid42, _yasids, game_did, h_unt, huya_ua, huya_web_rep_cnt,
huyawap_rep_cnt, isInLiveRoom, null_rep_cnt, rep_cnt, udb_accdata,
udb_anobiztoken, udb_anouid, udb_other
```

共享的 17 个名字里有 9 个值不同：`PHPSESSID`、`__yamid_new`、`guid`、`sdid`、`udb_biztoken`、`udb_cred`、`udb_deviceid`、`udb_guiddata`、`udb_origin`；`udb_uid` 与 `yyuid` 相同。两组最终解析到同一个账号，且 `prepare_cookie` 都不会改写它们。

**行为差异：没有观察到。** 用同一套 `platform::huya` 适配器分别加载扫码库、网页 Cookie 库和删除凭据后的匿名库，房间 660000 的 host/title/area/cover、8 档画质、6 条线路、候选 host 与 query key 完全一致。网页 Cookie 只是多了浏览器埋点字段，没有解锁额外画质或线路。

## 3. 直播间数据

房间 660000 实测：

- 主播：虎牙英雄联盟赛事
- 标题：【预告】29日4点 S16瑞士轮第五日
- 分区：英雄联盟
- 直播中：是
- 在线：136 万左右（页面 `totalCount`）
- 开播：17:07（页面 `startTime`）

修复前 `online` 与 `started_at` 永远是 `None`。原因不是接口缺失，而是 `platform/huya.rs` 的 `field()` 只匹配 `"key":"value"`，页面里的这两个字段是裸数字：

```json
"totalCount":1362495,
"startTime":1793092036
```

已新增：

```rust
fn number_field<T: std::str::FromStr>(body: &str, key: &str) -> Option<T>
```

并让 `room_info` 使用 `number_field::<u64>(&body, "totalCount")` 与 `number_field::<i64>(&body, "startTime")`。单测 `parses_numeric_room_fields` 覆盖裸数字、引号数字、0 和缺失键。真机监控行显示「130.1万」，抽屉显示「在线 136.1万 / 开播 17:07」。

## 4. 画质与线路

页面 `vMultiStreamInfo` 实际有 9 档，应用当前输出 8 档：

| 页面档位 | 码率 | 编码 | 应用 qn | 应用是否显示 |
| --- | --- | --- | --- | --- |
| 4K HDR | 20100 | HEVC | 30000 | 是 |
| 4K | 20000 | HEVC | 20000 | 是 |
| 2K HDR | 14100 | HEVC | 15100 | 是 |
| 2K | 14000 | HEVC | 15000 | 是 |
| HDR(10M) | 4200 | HEVC | — | **否** |
| 蓝光10M | 0 | H264 | 10000 | 是 |
| 蓝光4M | 4000 | H264 | 400 | 是 |
| 超清 | 2000 | H264 | 250 | 是 |
| 流畅 | 500 | H264 | 80 | 是 |

`HDR(10M)` 被 `hevc_bitrate_for_qn` 的 4K/2K 映射跳过，是已知缺口；不影响 2K/1080P。

线路 6 组（页面 `gameStreamInfoList`）：腾讯云、华为云24、腾讯云15、华为云、阿里云、阿里云13。每组候选的请求头只有：

```text
Origin: https://www.huya.com
Referer: https://www.huya.com/660000
User-Agent: ...
```

**没有 Cookie。**

8 档画质与 6 条线路在扫码/网页/匿名三组完全一致。实际取流抽样：

- qn=15100（2K HDR）：HTTP 200，HEVC 2560×1440
- qn=15000（2K）：HTTP 200，HEVC 2560×1440
- qn=10000（蓝光10M）：HTTP 200，H264 1920×1080
- qn=400/250/80：HTTP 200，H264 1920×1080 / 1280×720 / 800×450

## 5. 录制实测

### 5.1 1080P（蓝光10M）

- 时长约 25 秒；4 个分片 56,750,620 B，合并 54,011,419 B
- DB：`finished`、`duration_ms=150000`、`danmaku_count=39`、`quality=蓝光10M · FLV`、`post_state=done`
- 弹幕 JSONL 与 ASS 均为 39 行；缩略图生成成功

### 5.2 2K HDR

- `record_qn=15100`，日志 `quality=2K HDR · FLV requested_qn=15100 actual_qn=15100 line=腾讯云`
- 约 81 秒；5 个分片 94,248,671 B，合并后 ffprobe：HEVC 2560×1440 + AAC，时长 212.9 秒
- DB：`danmaku_count=3`、`quality=2K HDR · FLV`；无中途重试

### 5.3 4K HDR（本次主要缺口）

- `record_qn=30000`，日志 `quality=4K HDR · FLV actual_qn=30000 line=腾讯云`
- 每次写出约 4.6 秒、3.8–5.5 MB 的 `.mkv`（HEVC 3840×2160 + AAC）后 ffmpeg 退出
- 应用连续重启 3 次（start_number 0/1/2），随后日志 `连续未确认收尾，暂停自动重试 5 分钟`
- 同一时刻用探针取 4K FLV：AL/AL13/HS/HS24 返回 403，TX/TX15 返回 200；4K HLS 在 TX/TX15/HS/HS24 可拿到 m3u8，AL/AL13 403
- 因此不是「4K 完全不可用」，而是 **4K FLV 在当前账号/边缘上只能维持数秒**；应用固定选腾讯云 FLV，没有 4K HLS 候选，也没有在短 EOF 后换线

结论：**4K/4K HDR 被 UI 标为可用，但当前无法稳定录制**；2K HDR 与 1080P 可用。页面没有可直接读取的 VIP/4K 权益字段，不能简单用登录态判断，需要后续决定是隐藏 4K、加 HLS 候选，还是短 EOF 自动降级到 2K。

## 6. 登录与 Cookie 轮转

- 扫码 Cookie 落库后账号卡显示：18 个 Cookie（脱敏计数）、hy_139432760、已登录、自动续期可用
- 启动时 `虎牙 cookieExchange 完成 write_flag=false changed=false`，没有伪造轮换
- `credential_refresh`：`platform=huya outcome=fresh message=虎牙登录态有效 key_stale=0`，`last_refreshed_at` 与 `checked_at` 均写入
- 手动「立即检查」走同一 `refresh_now`；失败会回退 `credLogin`（uri 70017），手动 Cookie 没有 `udb_cred` 时只校验、不续期
- 网页 Cookie 有 `udb_cred`，`prepare_cookie` 会走 `credLogin` 补票；实测账号与扫码一致

## 7. 请求泄露检查

- 页面请求 `https://www.huya.com/660000` / `m.huya.com/660000` 由 `http::send_inner` 按域名注入虎牙 Cookie；这是必要的登录态请求
- CDN 候选 headers 只有 Origin/Referer/User-Agent，没有 Cookie
- 录制 ffmpeg 命令行只带上述三个头，不带 Cookie；URL 里只有 `wsSecret/wsTime/...` 流签名，没有账号 Cookie
- 平台接口日志使用 `redact_url`，房间参数以 `***` 输出；未发现 Cookie 值写入日志
- 需要强调：`wsSecret` 仍是可播放凭据，日志/进程列表能看到 URL；这是流签名，不是账号 Cookie，但分享进程命令行仍可能泄露单条流

## 8. 对 B 站/斗鱼/抖音的影响

本次代码只改：

- `src-tauri/src/platform/huya.rs`：新增 `number_field` 并用于 `room_info`
- `src/i18n/{zh-CN,en-US}.json`：添加房间提示补上虎牙

没有改共享的 `http`、`credential`、`recorder`、`monitor`、弹幕或 B 站/斗鱼/抖音适配器。全量单测与文档检查通过；本轮未做三平台真机回归，但从 diff 与调用路径上没有共享行为变化。

## 9. 已修 / 待办

已修：

- 房间在线人数、开播时间永远为空
- 添加房间提示遗漏虎牙

待办（需要产品决策）：

- 4K/4K HDR 在 UI 标为可用但只能录 4–5 秒；可选方案：把 4K 标为不可用、为 4K 增加 HLS 候选、短 EOF 自动降级到 2K
- 页面 `HDR(10M)` 4200 kbps 档未映射，应用画质列表少一档
- 4K HLS 已存在但 `huya.rs` 只生成 FLV 候选

## 10. 文件与验证

- `src-tauri/src/platform/huya.rs`
- `src/i18n/zh-CN.json`、`src/i18n/en-US.json`
- 单测：`cargo test --lib platform::huya` 8 passed / 1 ignored
- 真机：release bundle 32.48 MiB；1080P 与 2K HDR 各录制一场；4K HDR 失败样本 3 段
