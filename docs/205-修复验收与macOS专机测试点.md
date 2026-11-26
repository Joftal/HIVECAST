# 205 · 录像恢复修复、Windows 验收与 macOS 专机测试点

2026-11-26 · HIVECAST 0.1.0 · Windows x64 · 基线 `main@7e5f00f927185a75e2eb9694eba842823966f2b0`（后续入库提交 `c9503e453e9c49e86d8004f9ebd6b5e91d258800`）

**本报告记录 `c9503e4` 的 Windows 验收。后续提交 `0d7708f` 的三项主要修复已复核通过，但 Windows 实测发现整房合并可能漏掉后续有效录像，当前候选版应暂缓正式上线。** 本报告的分片保护与缺 PID 回收结果对应原基线。202 号的安装升级与四路并录也有各自的历史包哈希。新提交的实测数据、剩余问题和 macOS 公开分发条件见 [206 号报告](206-c9503e4修复复核与macOS专机实机验收.md)。

[206 号 c9503e4 复核与 Mac 实机验收](206-c9503e4修复复核与macOS专机实机验收.md) · [204 号缺陷与原复现数据](204-上线前独立代码审查与Windows复验.md) · [203 号 Mac 实测](203-macOS独立审查与d3108fa真机验收.md) · [202 号 Windows 历史验收](202-Windows上线前最终验收报告.md)

本轮有 16 份变更的产品源码、配置、构建脚本和测试文件。以基线提交为首行，按路径排序并在换行归一化后计算各文件 SHA-256；本机清单为 `build/accept-205/source-manifest.sha256`，清单 SHA-256 为 `cca2f98be4ab2930130e49e2accdeba5a1e2eac968c9a889036fe9bd543bd203`。Mac 专机可用此清单核对复测版本。

## 一、修复及 Windows 证据

| 原问题 | 改动与验收结果 |
| --- | --- |
| N1 · 同名旧 TS 被当作本场产物 | 恢复只读清单登记的路径及本场独占的计划路径。降级拼接先写临时文件，完整落盘后排他发布；重名另取名。预置旧 TS 和 752 字节本场分片，旧 TS 逐字节保留，新索引指向本场 752 字节。开录还会避开已有清单或遗留分片的基名。 |
| N2 · 清单损坏后删掉后续分片 | 已登记清单的组优先恢复；清单缺失、损坏或归属不符时保留登记，整组跳过普通恢复，保留策略排除待收尾组。缺失和截断两次启动均报告恢复 0，后续 250 字节未变；恢复正确清单后登记两段共 460 字节，再启动为 0。 |
| N3 · 自动收尾覆盖同名文件 | FFmpeg 的输入清单和输出暂存文件放入本任务独占目录。预置旧 `.concat.txt` 和 `.merging.mp4`，真实 FFmpeg 合成成功及无效输入失败两条路径均验证哨兵内容不变；失败输入仍在。删除及保留策略也移除了对旧公共临时文件名的猜测。 |
| N4 · 写库失败仍报成功 | 普通恢复沿用分段清单机制。SQLite 拒绝登记时返回 0，保留已生成的成品、分片和实际路径。解除故障后复用成品；真实媒体夹具还核对原修改时间、可探测时长、后处理状态和登记后清理。第二次启动计数为 0。 |
| N5 · 第 201 条无法恢复 | 使用会话主键游标分批查询全部失败及中止记录，跳过仍在录制的会话。以 200 条新失败记录覆盖旧场次，在保留策略关闭时仍找回第 201 条的 188 字节，第二次启动返回 0。 |
| N6 · 卡片重试目标不对 | Windows 上执行实际 TSX 组件的按钮回调，IPC 使用测试替身。首片 `c0` 后处理失败、母带在、主文件缺失，次片 `c1` 完好。点击重试只调用 `postRetry("c0")`，点击播放调用 `mediaPlay("c1")`。运行中的取消按钮也按显示进度所属片段绑定。 |
| N7 · 跨页汇总未标范围 | 61 片夹具加载前 60 片时，卡片显示 1:00、7.0 KB、120 条，并标明仅统计 60/61 片。触发补组回调后显示 1:01、7.1 KB、122 条，局部提示消失。中英文均通过。 |
| 203 · 缺 PID 时边写边合 | 首录及续录检查 PID 标记写入结果。启动遇标记缺失时，枚举 FFmpeg，核对可执行文件、segment 参数和本场完整输出模板，确认进程退出后关单；不能确认则延后。两个真实 FFmpeg 同时写不同模板，本场进程退出，另一进程继续写。原有 PID 归属专项和连续两次启动的延后夹具也通过。 |
| 203 · 崩溃后有聊天、无计数和 ASS | 单段恢复按 JSONL 可解析且内容非空的记录回填，缺少 ASS 则排他生成。两条完整消息加截断尾行得到两条 Dialogue 和数据库计数 2，媒体页“只看有弹幕”查询可见；重试保留已有 ASS。另覆盖旧版本已经登记成品、计数为零的单段会话。多段历史仅有整场母轨时，缺少切分依据，本轮没有给每段重建弹幕时间轴。 |
| 203 · 整房合并与短锁名 | 整房合并跳过无路径、无母带、大小为零的失败或中止尝试。已完成记录、明确所选片段仍做缺文件检查。互斥锁使用完整摘要；两组选段的旧摘要前缀同为 `B7`，同组选段仍互斥，另一组可进入文件检查。61 段实际合并、登记出错后重试和输出帧数一并回归。 |
| 203 · 历史验收基线与 Mac 签名 | 202 号已补记入库修复提交 `d3108fa` 及历史包对应关系。Mac 封装对暂存 app 做严格签名校验；内部包在需要时重签，公开包要求 Developer ID、hardened runtime、公证、stapling、系统评估。CI 从实际 DMG 挂载启动 app。Windows 已做 Bash 语法和工作流结构检查；Apple 系统验收见第三节。 |

主要实现与回归入口见[录像恢复](../src-tauri/src/recorder.rs)、[回归夹具](../src-tauri/src/recorder/recovery_tests.rs)、[分片合成](../src-tauri/src/segment.rs)、[进程归属](../src-tauri/src/orphan.rs)、[整房合并](../src-tauri/src/commands.rs)及[媒体页](../src/features/media/RoomRecordingsPage.tsx)。

## 二、Windows 执行记录

| 项目 | 结果 |
| --- | --- |
| Rust | Rust 1.100.0 格式检查及全目标严格 Clippy 通过；默认库测试 **483 通过、0 失败、27 忽略**。 |
| Windows 专项 | 从忽略项中单独实跑 **12/12 通过**。覆盖真实 FFmpeg 进程归属、有无 PID 标记、数据库拒写后复用可播放成品、真实封装及转码、分片、61 段合并、HLS、50,001 条 CSV 和百万条弹幕窗口。 |
| 前端及文档 | 冻结锁文件安装、TypeScript 和 Vite 生产构建通过，中英文 982 个键；Vite 仍提示超过 500 kB 的产物块。两种语言的媒体卡片通过组件回调测试。文档检查 通过，共 206 份。 |
| B 站公开弹幕 | 使用项目匿名 WebSocket 探针。房间 545068 在 35 秒内收 0 条，随后查询为离线。换用查询确认为在播的三个房间，各观测 50 秒：66688 收 10 帧、1 条可展示消息；13331433 收 3 帧、1 条；1918666795 收 4 帧、0 条。三者认证返回码均为 0。本轮验证了公开消息接收与解析；单个零消息样本的原因仍未知。 |
| Windows 候选包 | Node 24.22.3、pnpm 12.23.0 构建 NSIS。10,191,606 字节，SHA-256 `bbdc08d2395065f425fe18cee2654ca813fb4d6b160b71b665de40098dc929a8`，Authenticode 为 `NotSigned`。压缩包完整性测试通过；解包主程序为 x64、版本 0.1.0，仅 Tauri 的三字节 NSIS 类型标记与封装前不同。安装器保存在 `build/accept-205/HIVECAST_0.1.0_x64-setup.exe`。 |
| 测试范围 | Windows 使用临时 SQLite、合成媒体、本机 FFmpeg、组件及匿名直播样本。本轮没有对真实用户库执行删除、升级覆盖或强退；完整安装升级、账号登录和四路并录的历史范围见 202 号。用户约定的外盘异常插拔不在本次验收范围。 |

本机保留 `build/test-205.log`、`build/special-205.log`、`build/clippy-205.log`、`build/package-205.log`、`build/bili-*-205.log` 及组件夹具 `build/ui-205.mjs`。本机复跑组件夹具可执行 `node --experimental-vm-modules build/ui-205.mjs`。Rust 夹具已随源码保存，以下命令在仓库根目录执行。

```powershell
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib -- --ignored --test-threads=1 real_media_registration_retry missing_marker_reaps_only_the_matching_live_process merge_61_clips_retries_registration local_hls_lifecycle verify_live_owned_segment_process real_ffmpeg_source_convert real_custom_transcode_handles merge_makes_playable split_ranges_cut_real ffmpeg_pipeline_on_synthetic csv_export_reports_truncated_total million_message_seek
pnpm build
pnpm check:i18n
pnpm check:docs
```

## 三、macOS arm64 专机验收清单

在专机使用同一工作树或包含本轮修复的提交。记录提交、工作树状态、macOS 版本、机型、FFmpeg 路径及版本，使用隔离测试账号、房间和录像目录。复用旧库前退出 HIVECAST，备份整个应用数据目录（含 SQLite 旁挂文件）。以下操作只对测试数据执行。

| 测试点 | 操作及通过条件 |
| --- | --- |
| M1 · 共用代码门禁 | 执行第二节的格式、严格 Clippy、默认库测试及专项命令。Windows 专有的 CIM 用例在 Mac 不会匹配，单独确认 `missing_marker_reaps_only_the_matching_live_process` 真实执行。保留测试名称、数量和失败输出。 |
| M2 · 正常停录与强退 | 有持续弹幕的测试房间各录一场。正常停止第一场，记录时长、计数和旁挂文件。第二场产生至少两个分片后，仅强退该测试 HIVECAST，保留 FFmpeg，重新启动。核对 PID 归属、关单原因、分片清理、ffprobe 时长、首中尾抽帧、媒体库大小；第二次重启无重复成品。 |
| M3 · 缺失、无效及不可读的 PID | 在隔离录制中移走本场 PID 标记，再强退该应用；另开一个相同 FFmpeg 可执行文件，写到不同测试模板，路径含空格、中文和百分号。重启只回收本场进程，确认退出后才合并。另测正常停录已移除标记、PID 指向无关进程、标记格式错误或系统无法读取进程参数；未知归属时不得关单或清分片。 |
| M4 · 同名 TS、清单、文件归属 | 运行 N1、N2、N3 对应夹具。另在 APFS 测试目录预置同名 TS 和两个旧临时文件并记录哈希。让首片已完成、后片只有原分片，分别暂时移走清单、截断 JSON、恢复正确副本，再重启两次。前两次不得失去数据库清单引用和后片；修复后两个成品可正确登记，旧文件哈希不变。 |
| M5 · 写入失败、超过 200 条 | 运行 `real_media_registration_retry` 及 `pending_recording_after_two_hundred_candidates`。另在临时媒体目录撤去写权限测试收尾，再恢复权限。核对故障时成品、分片、错误提示；重试后只有一条登记且保留原成品修改时间。 |
| M6 · 弹幕与媒体卡片 | 对单段崩溃样本核对 JSONL 完整消息数、库中计数和 ASS 的 Dialogue 数，保留截断尾行、已有字幕和二次启动的结果。开启“只看有弹幕”后能找到该场。两片场次首片后处理失败且母带存在、次片可播时，在媒体卡上实际点击重试和播放，分别命中首片、次片。用 61 段检查 60/61 提示、补齐后的汇总及提示消失。 |
| M7 · 合并与回放 | 在临时库中加入一条无成品失败记录和两条有效录像，通过整房合并入口验证只合并有效输入；选中一条实际缺失的完成记录时应明确报错。验证 61 段登记重试、本地 HLS 播放及拖动、百万条弹幕窗口查询；记录峰值内存。 |
| M8 · 内部包 | 用仓库固定工具链执行 `pnpm install --frozen-lockfile`、`bash scripts/build-dmg.sh`。对最终 DMG 执行 `hdiutil verify`，只读挂载，对卷内的 HIVECAST.app 执行 `codesign --verify --deep --strict --verbose=2`。确认可启动。内部 ad-hoc 产物只计入内部验收。 |
| M9 · 公开分发 | 设置 `HIVECAST_PUBLIC_MACOS=true`、`APPLE_SIGNING_IDENTITY`、`APPLE_ID`、`APPLE_TEAM_ID`、`APPLE_PASSWORD`，使用已装入钥匙串的 Developer ID Application 身份；CI 可通过加密 secrets 提供 `APPLE_CERTIFICATE` 和 `APPLE_CERTIFICATE_PASSWORD`。凭据只在封装环境注入。公开模式缺少身份或公证失败时必须停止。对成功包验证下列命令，同时留存两个公证请求结果及最终 DMG 的 SHA-256。 |
| M10 · 下载首开和覆盖升级 | 从浏览器下载同一哈希的公开 DMG，在干净 macOS 用户环境保留下载隔离属性，拖至 Applications 后由 Finder 首开，记录 Gatekeeper 结果。用测试旧版配置、历史录像及已登录的测试账号做覆盖升级，核对房间、凭据状态、历史、媒体路径和回放。 |
| M11 · Mac 平台回归 | 分别录制 B 站、斗鱼、虎牙、抖音在播测试房间，检查画面、弹幕、停录、重开及四路并录；记录每路实录时长、ffprobe 时长和条数。账号续期和跨小时资源曲线分别记录，采用缩时样本时注明实际时长。 |

```bash
# 将 DMG 设为实际待分发文件路径，APP 设为从该文件只读挂载的应用路径。
hdiutil verify "$DMG"
codesign --verify --deep --strict --verbose=2 "$APP"
codesign -dv --verbose=4 "$APP"
xcrun stapler validate "$APP"
spctl --assess --type execute --verbose=4 "$APP"
xcrun stapler validate "$DMG"
spctl --assess --type open --context context:primary-signature --verbose=4 "$DMG"
shasum -a 256 "$DMG"
```

公开包的签名详情应显示正确的团队、Developer ID Application 身份及 runtime 标记。确认 app 与 DMG 的票据均有效、系统评估接受后，将命令结果、下载首开和升级结果与同一 DMG 哈希归档。[封装脚本](../scripts/build-dmg.sh) · [发布工作流](../.github/workflows/build-release.yml)
