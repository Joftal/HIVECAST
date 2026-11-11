# 140 · 监控/录制与 Cookie 联动修复（P1/P2）与 P3 评估

> 状态：**已实现并完成单测/构建级验证**（2026-11-11）。对应 [139](139-监控录制与Cookie状态全场景分析.md) 的 R1–R7；本轮修 R1、R2、R3、R4、R6、R7，R5 只评估、不改行为。
> 关联：[131-录制状态机与恢复](131-录制状态机与恢复.md)、[136-B站Cookie续期状态与手动检查](136-B站Cookie续期状态与手动检查.md)、[138-B站三态Cookie与取流审计](138-B站三态Cookie与取流审计.md)、[139-监控录制与Cookie状态全场景分析](139-监控录制与Cookie状态全场景分析.md)、[141-录制画质目标与自动升档](141-录制画质目标与自动升档.md)。

---

## 1. 结论先行

| 139 风险 | 本轮处理 | 结果 |
| --- | --- | --- |
| R1 续录不更新画质元数据 | `respawn` 成功后更新 `active.qn`、`active.recording.quality/line`，并写回 `record_session.quality/line/quality_history` | 已修 |
| R2 默认最高档静默降档 | 比较“上次实际 qn / 本次实际 qn”，变化即 WARN + 复用 `quality_fallback` notice；`40000` 降档不再静默 | 已修 |
| R3 过期 Cookie 让低档画质清单虚标 | 凭据状态进入内存；`is_ok()` 同时要求 Cookie 存在且 status=ok，过期时 B 站画质按匿名 `current_qn` 封顶 | 已修 |
| R4 `clear_groups` 与在途 plan 回填竞态 | Broker 新增 `epoch`；`clear_groups` 递增，plan/info 成功后比对，旧结果不写缓存；旧请求的失败也不计入新冷却 | 已修 |
| R5 `clear_groups` 保留 failures | 保持现状：继续保留失败冷却 | 评估后不改，见 §5 |
| R6 续录画质变化没有通知 | 实际档位变化时发 `quality_fallback` notice，文案改为中性「录制画质已调整」/「Recording quality changed」 | 已修 |
| R7 一场内混合画质无法表达 | 新增 `QualitySegment { at, qn, quality, line }` 与 `record_session.quality_history` | 已修数据层 |

一句话：**Cookie 生命周期仍然与录制生命周期解耦；这次补上的是“续录后元数据跟着实际档位走、变化可见、旧凭据结果不污染新缓存”。**

---

## 2. 修复后的行为

### 2.1 同一场从 1080p 降到 720p

时间线：有效 Cookie 开录 `actual_qn=10000`，两小时后 Cookie 过期，断流续录拿到 `actual_qn=250`。

1. 开录写 `record_session.quality="原画"`，`quality_history=[{at:t0,qn:10000,quality:"原画",line:"线路1"}]`。
2. 断流后 `respawn` 用当前 Cookie 重新解析，得到 `candidate_qn=250`。
3. 锁内比较 `active.qn=10000` 与 `actual_qn=250`，判定变化：
   - `active.qn=250`；
   - `active.recording.quality="超清"`；
   - `active.recording.line` 更新为本次线路；
   - `quality_history` 追加 `{at:t1,qn:250,quality:"超清",line:"线路2"}`。
4. 锁外写 DB：`quality`、`line`、`quality_history` 三列一起更新；同一 `session_id` 不变。
5. 若房间开启开播通知，发 `quality_fallback`：标题「录制画质已调整」，正文「主播 的实际录制画质为 原画 → 超清，录像不会中断」。
6. 监控行/任务卡的实时画质来自 `RecordingInfo`，随 `rooms://changed` 刷新；媒体库当前不展示 quality，历史值只在 DB/IPC 中可读。

反向（720p 升 1080p）走同一条路径，也会追加 history、发通知；文案不再写“降档”。

### 2.2 只是换线路、画质没变

`respawn` 仍会更新 `active.recording.line` 并写回 DB 的 `line`，但不追加 `quality_history`、不发通知。这样不会因为 CDN 轮换制造大量“画质变化”噪音。

### 2.3 开录解析途中换 Cookie

旧请求返回后：

- 成功：`inner.epoch != epoch`，直接返回结果给本次调用，但不写 groups/infos 缓存，也不清 failures。
- 失败：同样比对 epoch，不把旧凭据的失败写进新冷却。

下一次解析重新走平台接口，不会在 60 秒 TTL 内继续命中旧档位。

---

## 3. 实现位置

### 3.1 IPC 与 DB

- `/Library/dshproject/HIVECAST/src-tauri/ipc-types/src/lib.rs`
  - `QualitySegment { at: i64, qn: u32, quality: String, line: String }`，camelCase，TS 导出。
  - `RecordSession.quality_history: Vec<QualitySegment>`，旧记录反序列化为空数组。
- `/Library/dshproject/HIVECAST/src-tauri/src/db.rs`
  - SCHEMA 增加 `quality_history TEXT`；`migrate` 对旧库执行 `ALTER TABLE record_session ADD COLUMN quality_history TEXT`。
  - `create_session` / `create_clip` 写 JSON；`SESSION_SELECT`/ `map_session` 读第 20 列，解析失败退化为空数组。
  - 新增 `update_session_quality(id, quality, line, history)`，只更新这三列。
- `/Library/dshproject/HIVECAST/src/lib/bindings/QualitySegment.ts`、`RecordSession.ts` 由 `pnpm gen:bindings` 生成。

### 3.2 录制器

- `Active`：
  - `qn` 改为“当前实际档位”；
  - 新增 `plan_qn`：续录请求锚点。自动档位固定首段实际值，显式档位保留用户所选；
  - 新增 `quality_history`。
- 开录：`qn=actual_qn`，`plan_qn=pinned_qn(record_qn, actual_qn)`，history 首段用开录实际档位。
- `respawn`：
  - 请求用 `plan_qn`，选线用 `respawn_prefs(plan_qn, ...)`；
  - `pick_candidate` 同时返回 candidate 与 group，保留实际线路名；
  - 成功后按实际档位更新元数据，必要时追加 history、写 DB、发通知。
- `SplitManifest` 增加 `#[serde(default)] quality_history`；实时片段与崩溃恢复都带 history。

### 3.3 Broker 代次

- `Inner.epoch: u64`，`clear_groups` 清除 groups/infos 后 `wrapping_add(1)`。
- `plan_inner` / `room_info_inner` 在 adapter 调用前捕获 epoch；成功写回前若已变化，只返回结果，不写缓存；失败也不记入 `failures`。
- 代价：本次调用仍可能拿到旧凭据解析出的结果（单次），但不会影响后续 60 秒。

### 3.4 凭据状态与 B 站画质

- `/Library/dshproject/HIVECAST/src-tauri/src/credential.rs`
  - 新增内存 `STATUS`，`load_from_db` 同步灌入；`set` 写 ok，`remove` 清除；新增 `set_status` / `is_ok`。
  - `is_ok` = 有 Cookie 且 status == "ok"。
- `/Library/dshproject/HIVECAST/src-tauri/src/account.rs`：标记过期、复验成功、复验失败写 DB 成功后同步内存状态。
- `/Library/dshproject/HIVECAST/src-tauri/src/platform/bilibili.rs`：`anonymous = !credential::is_ok("bilibili")`，过期态按匿名封顶 `granted_qn`。

### 3.5 文案

`/Library/dshproject/HIVECAST/src/i18n/zh-CN.json` 与 `en-US.json` 的 `notice.qualityFallback` 改为中性：

- zh：「录制画质已调整」/「{{host}} 的实际录制画质为 {{detail}}，录像不会中断」
- en：「Recording quality changed」/「Recording for {{host}} is now at {{detail}}. The recording will continue」

---

## 4. 测试与验证

### 4.1 自动化

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --check` | 通过 |
| `cargo check --all-targets` | 0 错误 0 警告 |
| `cargo test --lib` | **274 passed / 0 failed / 17 ignored** |
| `pnpm exec tsc -b` | 通过 |
| `pnpm run check:i18n` | 753 keys |
| `pnpm run check:docs` | 141 files |
| `pnpm app:build` | 通过，bundle 30.73 MiB |

新增测试：

- `recorder::tests::respawn_quality_change_compares_actual_not_requested`：同档不比、升/降都比、qn=0 时退回画质名。
- `db::tests::session_quality_history_roundtrips`：两段 history 写读一致，quality/line 同步更新。
- `credential::tests::credential_status_gates_quality_access`：ok / expired / 删除三态。
- `broker::tests::credential_change_advances_epoch_and_keeps_cooldown`：epoch 递增、缓存清空、失败冷却保留。

### 4.2 真机 smoke（已完成）

2026-11-11 11:16–11:22，用真实 bundle、真实 B 站直播间 `bilibili:6`（LPL 赛事）跑通了一次完整链路：

1. 有效 Cookie 开录：`quality=原画 · FLV · H.264`、`actual_qn=10000`，DB history 一条。
2. 在设置页退出 B 站登录（`credential` 清空），录制进程不受影响。
3. `kill -9` 当前 ffmpeg 制造断流；11:17:20 第一次续录仍是原画，但线路从 `gotcha08b` 换到 `gotcha04b`，DB `line` 更新、history 仍只有一条（同画质换线不追加）。
4. 11:22:52 第二次续录在无 Cookie 下解析为 `超清 · FLV · H.264`、`actual_qn=250`：
   - 日志 `WARN 续录实际画质发生变化 previous_qn=10000 actual_qn=250`；
   - DB `quality=超清 · FLV · H.264`，`quality_history` 两条：`{qn:10000,quality:"原画…",line:"gotcha08b"}`、`{qn:250,quality:"超清…",line:"gotcha08b"}`；
   - 同一 `session_id`，没有新建场次。
5. 该房间未开启开播通知，因此没有 UI notice；通知发送条件在代码层是 `quality_changed && notify_live`，已有单测覆盖变化判定。

### 4.3 没做到的部分

- 媒体库/任务页尚未展示 `quality_history`；当前只落库和走 IPC，UI 展示是后续独立改动。
- 分割录制时每个片段会带当前完整 history（包含片段开始前的段），跨片段展示语义还未定。
- 没有专门验证“升档 + notice”的真机路径；本次只验证了降档 WARN 与 DB 写入。

---

## 5. P3 评估：凭据变更是否清 failures

### 5.1 现状

- `clear_groups` 清 groups/infos，保留 `failures`。
- 失败冷却只对 throttle 类错误生效，退避 5s → 10s → 20s → 40s → 60s 封顶。
- 触发 `clear_groups` 的路径：
  1. 保存 Cookie / 删除 Cookie / 扫码确认（身份变化）；
  2. 自动续期写回（同一账号轮换）；
  3. 标记过期（-101）；
  4. 复验成功/失败。
- `plan_for_respawn` 本来就 bypass 冷却，所以“已录制的续录”不会被 failures 挡住；会被挡的是监控/手动新开录。

### 5.2 清 failures 的收益与风险

| 方案 | 收益 | 风险 |
| --- | --- | --- |
| 全部清 | 换 Cookie 后立刻重试，不用等最多 60s | 自动续期每 12h 触发一次也会清；多房间同时重试可能造成请求尖峰；网络/限流失败与凭据无关，清了会放大 |
| 全部保留（现状） | 保守，不会因登录/轮换放大请求 | 登录后若旧 Cookie 刚被限流，新开录最多等 60s |
| 只对身份变化清 | 登录/退出/换号立即生效，续期轮换不影响限流 | 需要拆分 `clear_groups` 语义，调用点要传原因 |

### 5.3 结论

本轮**不改**。理由：

1. 最大延迟 60 秒，且监控会自行重试；respawn 已 bypass。
2. 自动续期是高频路径（每 12h 一次，且可能多房间），无条件清 failures 会把“Cookie 轮换”当成“解除限流”，与 139 的设计取向相反。
3. 真正需要“换号立即重试”的体验问题，应该用单独方法只清身份变化路径的 failures，而不是改 `clear_groups` 的全局语义。

后续若要做，建议新增 `clear_credential_cache(clear_failures: bool)` 或在 `credential_save/delete/qr confirmed` 调 `broker.clear_identity()`：清 groups/infos/failures；自动续期与复验仍走保留 failures 的 `clear_groups`。同时保留每房间 `MIN_INTERVAL=1.5s` 的闸门，避免瞬时尖峰。

---

## 6. 限制与后续

- 凭据状态只在 B 站画质封顶处使用；斗鱼/虎牙/抖音不使用该状态。
- 进程启动时 `credential.status` 可能仍是上次退出前的 ok，直到复验/请求发现过期；这与旧版一致，R3 修复的是“已知 expired 后仍按登录态算”。
- `quality_history` 的 UI 展示、按片段裁剪、导出/清理策略都未做。
- 当前 quality 通知与旧的降档通知共用 `quality_fallback` kind；如果以后要区分“升档/降档/换线路”，需要新增 kind 或 detail 结构。
