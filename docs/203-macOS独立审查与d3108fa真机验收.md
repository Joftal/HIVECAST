# macOS 独立审查与 d3108fa 真机验收

2026-11-26 · macOS 27.2.1（26B131）· arm64 Mac17,5 · `d3108fa` · 候选版本 `0.1.0`

**结论：d3108fa 声称的主要修复在 macOS 上成立，但不能据此给出“macOS 已可发布”的结论。** 本轮完成 478 项库测试、10 项被忽略专项、严格 Clippy、app/DMG 构建和真实 B 站录制；又用“录制中 `kill -9` 应用进程”复现崩溃遗留 FFmpeg，重启后 11 个分片被合并、遗留进程被回收、会话被标成 `aborted/app_exit`。正常收尾和崩溃收尾都能产出可解码的 FLV。

同时确认两项发布阻塞和三项可靠性缺口：应用是未绑定 Info.plist 的 ad-hoc 签名，Gatekeeper 会拒绝；崩溃恢复只补跑后处理，不恢复弹幕条数、不导出 ASS；PID 标记缺失时的恢复路径仍可能边写边合；无文件路径的失败片段会让整房合并永久失败；文档 202 的基线描述与当前提交不一致。本轮未修改源码，只新增本报告。

## 一、审查对象与判据

被审查提交：

```text
d3108fad75e420ef21a1e53aa7bc89f48b90a710
fix: 修复200审查缺陷，完成Windows测试
```

审查范围是 d3108fa 的逻辑修复，不包含对 Windows 真机的复跑。重点判据：

1. 修复代码是否真实进入当前二进制，而不是只改单测或文档；
2. 崩溃恢复能否识别本场 FFmpeg、等待或终止后收尾，而不是误杀或边写边合；
3. 正常停止与异常退出两条路径产出的文件是否可解码、数据库是否一致；
4. macOS 的构建、签名、DMG 与启动是否达到候选包要求；
5. 上一轮报告列出的未完成项中，哪些已关闭、哪些仍开放。

## 二、质量门与构建

| 项目 | 结果 |
| --- | --- |
| Rust 格式 | `cargo fmt --check` 通过 |
| 严格 Clippy | `cargo clippy --all-targets --locked -- -D warnings` 通过，0 warning |
| 默认库测试 | `cargo test --locked --lib`：**478 passed / 0 failed / 24 ignored**，9.17 秒 |
| 忽略专项 | 选取 10 项实跑：账号扫码、CSV 截断、61 段合并重试、本地 HLS、真实自定义转码、真实 FFmpeg、FFmpeg 管线、分片切分、百万条弹幕定位、片段合并可播放；**10/10 通过** |
| 前端/文档 | `pnpm check:i18n` 981 键；`pnpm check:docs` 203 份（本报告加入前） |
| app | `HIVECAST.app` 约 35 MB，Mach-O thin arm64 |
| DMG | 16,507,209 字节，SHA-256 `6e678183c5ff4197f0c3a52250eddea1d6845dfa09a9906126e8ef0ca864f67a`，`hdiutil verify` VALID |

构建产物与测试均来自 d3108fa 工作树，未在报告中把 Windows 的 476/25 与 macOS 的 478/24 混算。

## 三、真机录制：正常停止

通过 UI 添加 `https://live.bilibili.com/545068`（德云色，开播中），默认自动录制。录制约 423 秒后点“停止”。

| 字段 | 实测值 |
| --- | --- |
| 会话 | `4895f84e-2ac3-4548-a067-624ad8e4b3e6` |
| 状态/原因 | `finished / stopped` |
| 数据库大小 | 30,202,095 字节 |
| 会话时长 | 423,000 ms |
| 弹幕条数 | 109 |
| 后处理 | `done` |
| 旁挂文件 | `.jsonl` 6.5 KB、`.ass` 12 KB |
| ffprobe | 429.218 秒；H.264 1280×720；AAC |

JSONL 行数 109，与数据库 109 一致；ASS 由收尾阶段导出。随后又完成一场 675 秒、57,394,628 字节、171 条弹幕的正常录制，说明崩溃测试后正常路径仍可用。

## 四、崩溃恢复：真实 `kill -9`

### 4.1 复现

第二场录制开始后，记录：

- 会话 `60c15a2c-f526-4d76-aa5f-4675e149b871`，状态 `recording`；
- 应用 PID 83211，FFmpeg PID 83438；
- 旁挂 PID 标记 `...00-39-45.ffmpeg.pid` 内容为 83438；
- 已写入 `part0000`–`part0004`。

对应用执行 `kill -9 83211`。3 秒后应用退出，FFmpeg 83438 仍存活且 `PPID=1`，命令行完整包含本场 `-f segment` 输出模板，证明崩溃遗留进程真实存在，不是只存在于单测里的分支。

### 4.2 重启恢复

重新启动应用后，日志依次出现：

```text
收尾上次异常退出的录制记录 count=1
开始合并分片 ... parts=11 bytes=60988704
分片合并完成 ... parts=11 bytes=55194544
恢复失败会话遗留的分片 id=60c15a2c-...
恢复上次崩溃遗留的录像 recovered=1 recovered_bytes=55194544
```

恢复后：

| 字段 | 实测值 |
| --- | --- |
| 会话状态 | `aborted / app_exit` |
| 数据库大小 | 55,194,544 字节 |
| 会话时长 | 646,000 ms |
| 弹幕条数 | **0** |
| 后处理 | `done` |
| 成品 | 单个 FLV，11 个分片已清理 |
| 遗留 FFmpeg | 已不存在 |
| ffprobe | 652.271 秒；H.264 1280×720；AAC |
| 抽帧 | 0 秒、300 秒、640 秒三帧均解码成功 |

这说明 d3108fa 的核心恢复链路在 macOS 上有效：通过 PID 标记和命令行确认本场进程，重启时终止遗留 FFmpeg，合并残留分片，按 `app_exit` 收尾，且成品可播放。正常标记存在时，没有观察到误杀无关进程或边写边合。

## 五、发现

### P1 · ad-hoc 签名无法通过 Gatekeeper

当前 app 的签名信息：

```text
Identifier=hivecast
Format=app bundle with Mach-O thin (arm64)
flags=0x20002(adhoc,linker-signed)
Signature=adhoc
Info.plist=not bound
```

`codesign --verify --deep --strict --verbose=2` 的失败原文：

```text
code has no resources but signature indicates they must be present
```

把 app 复制出来重新执行 `codesign --force --deep --sign -` 后，`codesign --verify` 可以通过，但 `spctl` 仍拒绝，因为 ad-hoc 签名没有公证。DMG 本身校验有效，但这不改变 Gatekeeper 结论。

这是 macOS 分发阻塞，不是 d3108fa 的逻辑回归。自用或本机开发可以继续；对外分发必须补 Developer ID、notarization 与 stapling，或明确只提供源码构建。

### P2 · 崩溃恢复丢失弹幕条数与 ASS

这是本轮唯一在真实崩溃恢复中发现的成品缺陷。

正常收尾路径在 FFmpeg 退出后立即摘掉弹幕 writer，取得本次条数，并在后处理完成后导出 ASS：

- 摘 writer 与计数：[recorder.rs:2454](../src-tauri/src/recorder.rs#L2454)
- 写回会话：[recorder.rs:2866](../src-tauri/src/recorder.rs#L2866)
- 导出 ASS：[recorder.rs:3001](../src-tauri/src/recorder.rs#L3001)

崩溃恢复走的是另一条路径：

- [recorder.rs:3743](../src-tauri/src/recorder.rs#L3743) 的 `post_recovered_file` 只调用 `post::run`，随后把 `existing.danmaku_count` 原样写回，见 [recorder.rs:3803](../src-tauri/src/recorder.rs#L3803)；
- [recorder.rs:4337](../src-tauri/src/recorder.rs#L4337) 的失败/中止会话恢复只合并分片，不读取 JSONL 行数，也不调用 `danmaku_to_ass`。

实测证据：

```text
会话 60c15a2c...  status=aborted  db_count=0  jsonl_lines=15
会话 4895f84e...  status=finished db_count=109 jsonl_lines=109
会话 4422e17c...  status=finished db_count=171 jsonl_lines=171
```

恢复会话的 JSONL 有 15 行真实聊天记录，但数据库为 0，目录里也没有 `.ass`。用户看到的是“异常结束、0 条弹幕、无字幕”，而弹幕数据其实还在。录像本体不受影响。

建议在恢复收尾里补一个幂等的“弹幕二次收尾”：按 JSONL 行数回填 `danmaku_count`，用现有 `postprocess::danmaku_to_ass` 生成 ASS，再与文件大小一起提交；失败时保留 JSONL 并记录 `post_error`，不能因为字幕失败丢掉录像。单测应覆盖“有 JSONL、无 ASS、数据库计数为 0”的恢复会话。

### P2 · PID 标记缺失时仍可能边写边合

正常首录的顺序是“spawn → 写 PID 标记 → 建会话”，所以本轮 `kill -9` 后标记仍在，恢复正确。但续录的会话行已经存在，写标记失败只记 debug：

- spawn：[recorder.rs:1918](../src-tauri/src/recorder.rs#L1918)
- 写标记：[recorder.rs:1926](../src-tauri/src/recorder.rs#L1926)
- 写失败只 debug：[recorder.rs:3291](../src-tauri/src/recorder.rs#L3291)
- 标记 `NotFound` 且父目录存在时直接 `continue`：[recorder.rs:3322](../src-tauri/src/recorder.rs#L3322)

如果磁盘满、权限异常或标记被外部清理，重启时可能把仍在写的 FFmpeg 当成“安全”，随后 `close_orphan_sessions` 与 `recover_parts` 合并正在增长的分片。该窗口很窄，本轮没有做故障注入，因此仍按条件性 P2 记录，不把它算作本轮实测失败。

更稳的做法是标记缺失时不要直接放行：按会话输出模板扫描进程命令行，或至少把整组放进 `deferred`，等人工确认后再恢复。

### P2 · 无文件路径的失败片段阻断整房合并

`room_merge_sessions` 只排除 `recording` 和 `reason=merge`，没有排除 `file_path` 为空的失败/中止会话，见 [commands.rs:1729](../src-tauri/src/commands.rs#L1729)。`merge_plan` 会把空路径记进 `missing`，见 [commands.rs:2045](../src-tauri/src/commands.rs#L2045)，随后整次合并返回“缺少文件”。

只要一个房间曾出现过没有成品的失败片段，“合并本房”就会一直失败。建议整房合并过滤掉空路径/不存在文件的失败会话，或把它们列成可跳过的提示；指定片段合并仍应严格失败。

### P3 · 合并锁只用 2 个 base64 字符

锁文件名取 SHA-256 的 base64 前 2 个字符，见 [commands.rs:1837](../src-tauri/src/commands.rs#L1837)。这不是 64 个桶，而是 64²＝4,096 个桶；不同选段落进同一桶会误报“合并任务正忙”。概率不高，但提示会误导用户。可以改用完整摘要或更长的前缀。

### P3 · 文档 202 的基线不可复现

docs/202 写的是“`main@bb25bf1` 加本轮工作树修复 / 含未提交修复”，但当前 HEAD 已是 d3108fa，修复已经入库。用 docs/202 的基线单独检出无法重建同一候选包，削弱了验收溯源。发布前应把报告基线改成实际提交，或附上工作树差异摘要与构建产物哈希。

## 六、未完成与未覆盖

1. **Windows 未在本轮复跑。** 478 项是 macOS 结果；Windows 的 NSIS、WebView2、进程身份分支仍以用户侧结果为准。
2. **只实测 B 站。** 斗鱼、虎牙、抖音的 macOS 录制、弹幕、登录续期未在本轮重跑。
3. **只注入“录制中崩溃”。** 没有覆盖后处理中崩溃、分片合并中崩溃、分割清单恢复、磁盘满、目录只读。
4. **PID 标记缺失未做故障注入。** 第五节的条件性 P2 仍需真实复现或补代码防护。
5. **Gatekeeper 未做下载安装验证。** 只验证了本机 app 与 DMG 校验，没有从浏览器下载后首开。
6. **升级安装未做。** 没有用旧版覆盖安装并核对房间、设置、凭据、历史录像。
7. **播放与长录像未扩展。** 没有重跑 HLS 回放、61 段整组合并、四平台并录和百万级弹幕内存。
8. **弹幕恢复修复未实施。** 本轮只审查和验证，没有改源码；P2 需要后续提交解决。

## 七、清理与当前状态

测试结束后正常退出应用，删除测试房间、3 场会话、媒体登记、任务历史与任务事件，并删除 `build/.../Recording/哔哩哔哩` 下的测试录像与旁挂文件。复核：

```text
room=0  record_session=0  media_file=0
task_history=0  task_event=0  task_event_fts=0
credentials=4
ui.language=zh-CN  ui.theme=light
Recording files=0
```

应用已退出，未留下测试 FFmpeg。源码未改，工作树只多出本报告。

## 八、放行判断

- **自用/内部验证：可以继续。** 正常录制、崩溃回收、分片合并、成品解码都在 macOS 真机通过。
- **对外分发：不可以。** ad-hoc 签名和 Gatekeeper 是硬阻塞；P2 弹幕恢复缺口也应先修，否则“崩溃恢复”在用户视角仍是半成品。
- **代码审查：主要修复成立。** 恢复链路确实处理了遗留进程和分片，不是只加日志；但恢复的语义只覆盖“视频文件”，没有覆盖“弹幕旁挂文件”。

建议下一步顺序：先修 P2 弹幕恢复并补单测，再补 PID 标记缺失的防护，最后处理签名/公证与文档基线。完成这些之前，docs/202 的“Windows 候选包通过”不能外推为“macOS 发布通过”。
