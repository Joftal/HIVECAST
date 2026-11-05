# 01 · 参考项目 bililive-go 架构解析

> 源码位置：`bililive-go/`。行号引用均相对该目录，便于直接跳读。
> 本项目是一个"常驻服务 + Web UI"形态的直播录制器：进程启动后按配置逐房间监听，开播自动录制，下播后跑后处理流水线，全程通过 HTTP API + SSE 暴露给前端。

## 1. 顶层目录职责

| 目录 | 职责 |
| --- | --- |
| `src/cmd/bililive` | 入口：解析 flag/配置、初始化各子系统、按顺序 Start、处理信号退出（`bililive.go` 898 行） |
| `src/instance` | 全局实例容器（`Instance`），持有 Ctx、Lives、ListenerManager、RecorderManager、PipelineManager、Cache、EventDispatcher 等 |
| `src/configs` | 配置结构、YAML 读写、三层覆盖解析、平台键映射、热更新与回调（`config.go` 2054 行） |
| `src/live` | 平台无关的直播抽象（`Live` 接口、`Info`、`StreamUrlInfo`、`Options`、`WrappedLive` 装饰器）+ 各平台适配器子包 |
| `src/live/internal` | `BaseLive`：URL/日志/请求会话/options 的公共实现 |
| `src/listeners` | 监听器：轮询房间状态、产生开播/下播/改名事件 |
| `src/recorders` | 录制器：取流、起下载器、分段重启、状态上报；`danmaku/` 为弹幕录制 |
| `src/pkg/parser` | 下载器抽象（ffmpeg / native-flv / bililive-recorder / mock） |
| `src/pipeline` | 录制后处理流水线（阶段、任务队列、SQLite 存储、执行器） |
| `src/livestate` | 直播间状态持久化（SQLite：房间、会话、改名历史、可用清晰度） |
| `src/servers` | HTTP 服务：REST API、SSE、静态文件、扫码登录、更新、指标 |
| `src/pkg/events` | 进程内事件总线（同步/异步派发） |
| `src/webapp` | React 前端（CRA），构建产物以 embed FS 形式打包进二进制 |
| `src/tools` | 外部工具管理（FFmpeg 下载/状态、bililive-tools 本地服务） |

## 2. 启动顺序（`src/cmd/bililive/bililive.go`）

关键顺序（顺序本身就是设计的一部分，重排容易引入竞态）：

1. **launcher 检查**（:206）：若存在新版本待切换，本进程转为"启动器"，拉起目标版本子进程后退出。环境变量 `BILILIVE_LAUNCHER`/`_PID`/`_EXE`/`BILILIVE_INSTANCE_ID` 传递上下文。
2. **加载配置**（:213）：`--conf` 指定则读文件，否则由 flag 生成；无效时回退到可执行文件旁的 `config.yml`。`configs.SetCurrentConfig(config)`。
3. **元数据库** `AppDataPath/db`（:222）→ **Sentry/遥测**（:239/:252，失败不阻断）。
4. **事件总线** `events.NewDispatcher(ctx)`（:336）—— 之后所有子系统靠事件解耦。
5. **Pipeline**（:482-493）：`AppDataPath/db/pipeline.db` → `pipeline.NewManager` → `stages.RegisterBuiltinStagesToManager`。
6. **livestate**（:496）：`lives.db` → `livestate.NewManager` → `Start()`。
7. **listeners/recorders Manager 先构造后启动**（:509-510）：因为 server 依赖它们。
8. **HTTP Server 尽早就绪**（:514-528）：房间还没初始化完成时前端就能打开页面，看到"初始化中"。
9. **FFmpeg 异步初始化**（:560）`tools.FFmpegAsyncInit`；**平台就绪检查**（:569）`live.SetPlatformReadinessChecker(tools.PlatformDependenciesReady)`，让依赖外部工具的平台（如抖音）在工具就绪前不发请求。
10. **Manager Start**（:572-582）：listeners → recorders → pipeline。
11. **逐房间创建 `InitializingLive`**（:683-）：先占位可见，再异步取真实信息；成功后通过 `RoomInitializingFinished` 同步事件把占位对象替换成 `WrappedLive`。

> 设计要点：**"先让 UI 可见、后补真实数据"**。直播间列表不需要等所有平台请求完成才渲染；这也是桌面端启动体验的关键手段。

## 3. 核心抽象

### 3.1 Module

```go
// src/interfaces/interfaces.go:13
type Module interface {
    Start(ctx context.Context) error
    Close(ctx context.Context)
}
```

所有子系统（listener/recorder/pipeline/server）都是 Module，由 `instance` 统一持有，退出时逆序关闭。

### 3.2 Live：平台适配器

```go
// src/live/lives.go
type Builder interface{ Build(*url.URL) (Live, error) }
func Register(domain string, b Builder)            // init() 时按域名注册
type Live interface {
    GetLiveId() types.LiveID                        // 默认 md5(host+path)
    GetRawUrl() string
    GetInfo() (*Info, error)                        // 房间名/主播名/是否在播
    GetStreamInfos() ([]*StreamUrlInfo, error)      // 所有可选流
    GetPlatformCNName() string
    GetOptions() *Options
    UpdateLiveOptionsbyConfig(ctx, *configs.LiveRoom) error
    // ...
}
```

- `StreamUrlInfo`：`Url/Name/Quality/Format/Codec/Width/Height/Bitrate/FrameRate/AudioCodec/AttributesForStreamSelect/HeadersForDownloader/IsPlaceHolder`。
- `IsPlaceHolder`：标记"元信息里存在、但当前 qn 拿不到直链"的占位项——用户切换清晰度时再按需解析。
- `DeferredStreamResolver`：给"每档清晰度都要单独申请凭证"的平台（SOOP）用，避免一次全量解析。

### 3.3 WrappedLive：把"请求节流 + 缓存 + 退避"做成装饰器

这是整个项目最值得抄的设计之一（`src/live/internal/wrapped_live.go` 一带）：

- 同一直播间的 `GetInfo` 用 `requestMu` 串行化，天然防抖。
- `gcache` 缓存 Info；`GetInfoWithInterval(interval)` 让同一轮的多个调用方共享一次请求结果（waiter 队列）。
- 失败按 2 的幂退避，上限 `maxFailureBackoff = 5min`；成功清零。
- 平台级限流：`ratelimit.GetGlobalRateLimiter().AcquirePlatformWithContext(platformKey)`，保证同平台最小访问间隔与在途并发。
- 工具未就绪返回 `ErrPlatformToolsNotReady`，监听器每 2s 重试而不打平台接口。

> 迁移到 HIVECAST 时，这套"装饰器 + 共享请求 + 平台级令牌桶"应作为平台层的基础设施，而不是每个适配器各写一遍。

### 3.4 Options：一次录制周期的选项快照

`Options{Cookies *cookiejar.Jar, Quality int, AudioOnly bool, NickName string, recordingVersion *byte}`。
`SameRecordingOptions` 用于判断"录制过程中选项是否被改"：改了就让本轮取流作废、重新取流，避免半途换清晰度导致文件损坏。

## 4. 监听器（`src/listeners`）

- 状态机：`begin → pending → running → stopped`。
- `run()` 循环：`GetInfoWithInterval(interval)` → `processInfo`。
- `processInfo` 用前后两次 Info 做 diff，产出事件：
  - `LiveStart`：记录 `LastStartTime`、发开播通知；
  - `LiveEnd`：发下播通知；
  - `RoomNameChanged`：仅当 `VideoSplitStrategies.OnRoomNameChanged` 打开时触发（用于切分段）。
- `NotifyOnly` 房间只提醒、不录制。

**生命周期接管**：`RoomInitializingFinished` 事件里，把 `InitializingLive` 换成 `WrappedLive`、替换 `inst.Lives`、用 `SeedInfo` 注入已取到的 Info、`SetLiveRoomId` 写回配置、最后 `replaceListener`：先 `CloseSync` 旧监听器再 `StartWithInfo` 新监听器。

> 这里的注释写得很直白：**必须同步完成替换再让 `InitializingLive.GetInfo` 返回**，否则会出现"新 LiveStart 判断录制器已存在 → 旧 ListenStop 又把录制器删了"的竞态，表现为"正在直播但没在录"。这类"事件顺序导致的状态机错乱"是录制器最典型的线上 bug，HIVECAST 必须在设计期就用"同步事件 + 单线程状态转移"规避。

## 5. 录制器（`src/recorders/recorder.go` 2163 行）

### 5.1 主循环

`run()` 是**单 goroutine** 循环调用 `tryRecord`；`tryRecord` 返回后按需 sleep 再试：

- 连续"秒退"（运行时长 < `fastFailThreshold = 5s`）会逐步拉长重试间隔（`recordRetryInterval`），防止"取流失败 → 立刻重试 → 再失败"的风暴。
- 分段重启（MaxDuration/MaxFileSize/改名）时，通过 `redirect` 链把 `pipelineSharedState`（待处理计数、文件明细、摘要是否已发）传给下一段，保证"一次直播只发一条录制摘要"。

### 5.2 tryRecord 步骤（顺序即正确性）

1. `ResolveConfigForRoom` 得到该房间的生效配置；固定 `preference` 与 `Options` 快照。
2. 取流：优先 `DeferredStreamResolver`，否则 `GetStreamInfos`；失败按退避重试。
3. 渲染文件名模板（默认 `{{ .Live.GetPlatformCNName }}/{{ .HostName | filenameFilter }}/[{{ now | date "2006-01-02 15-04-05" }}][{{ .HostName }}][{{ .RoomName }}].flv`）；**模板渲染失败直接停止录制请求**，避免重试风暴。
4. `selectPreferredStream`：按 `StreamPreference` 打分选流（清晰度/属性/编码）。
5. 扩展名修正：m3u8 → `.ts`，`AudioOnly` → `.aac`。
6. 检查 FFmpeg 可用（`GetFFmpegPathForLive` / `WaitFFmpegAsyncInitDone`）。
7. FLV 用本地代理 `streamprobe` 探测头部信息；HLS 异步 `ProbeHLS`；SOOP 用 `hlsproxy` 过滤 preloading 分片。
8. `newParser(originalURL, downloaderType)` 选下载器。
9. 启动弹幕录制（B 站/抖音/斗鱼），ASS 与视频同名。
10. `parser.ParseLiveStream(ctx, streamInfo, live, fileName)` 阻塞到本次录制结束。
11. 收尾：清理空文件/累积文件 → 交给 Pipeline（未配置 `CustomCommandline` 时）或执行 legacy 自定义命令。

### 5.3 分段与"多文件"

一次"直播"可能产出多个文件：MaxDuration 到点、MaxFileSize 到点、房间改名都会切段。切段不是"停止录制再开始"，而是**同一个 Recorder 内的重启**，共享 `pipelineSharedState`。

## 6. 下载器（`src/pkg/parser`）

```go
type Parser interface {
    ParseLiveStream(ctx context.Context, info *live.StreamUrlInfo, l live.Live, file string) error
    Stop() error
}
```

内置：`ffmpeg`（最通用，追加 `-headers`、`-user_agent`、`-c copy`）、`native/flv`（自研 FLV 解析，省一次转封装）、`bililive_recorder`（外部工具）、`mock`。
注册表 `Register(name, builder)` / `New(name, cfg, logger)`。

> 教训：ffmpeg 作为子进程时，"停止"要靠关 stdin / 发信号 + 等待；否则会留下孤儿进程并锁住输出文件（Windows 上尤其明显）。HIVECAST 需要统一的"子进程管理器"。

## 7. 后处理流水线（`src/pipeline`）

- 阶段：`fix_flv`（修时间戳）→ `convert_mp4`（转封装/转码）→ `extract_cover`（首帧封面）→ `burn_subtitles`（ASS 烧录）→ `cloud_upload`（OpenList 上传）→ `custom_command`。
- `StageConfig{Name, Enabled *bool, Parallel []StageConfig, Options map[string]any}`：`Enabled` 用指针区分"未设置"（默认 true）。
- `FileInfo.Deletable`：只有"整个流水线全部成功"才删中间文件；任一阶段失败/取消则**保留全部文件**。
- 任务持久化在 `pipeline.db`，重启时 `ResetRunningTasks` 把 running 拉回 pending，可续跑/重试（含 `RetryTaskFromStage` 从指定阶段重试）。

## 8. 数据与状态（`src/livestate`）

表：`live_rooms`、`live_sessions`、`name_history`、`available_streams`。
要点：用**心跳**判定异常退出（`EndSessionByHeartbeat`），`EndReason` 区分 `normal/user_stop/crash/error/unknown`。

## 9. HTTP 层与前端

- REST 路由见 `src/servers/server.go`（约 60 条），核心：`/api/lives`（列表/增删/启停/切流）、`/api/config`（结构化 GET/PUT/PATCH + raw YAML）、`/api/file`（浏览/改名/删除）、`/api/cookies`、扫码登录、`/api/sse`。
- SSE 推送：录制状态、弹幕、日志、FFmpeg 状态、内存告警、更新进度。
- 前端组件：`live-list`、`live-info`、`config-info`、`stream-selector`、`danmaku-config`、`danmaku-panel`、`file-list`、`history-panel`、`pipeline-task-list`、`task-page`、`io-stats`、`update-page`、`log-panel`、`batch-add-room-dialog`、`TemplateBuilder`。

## 10. 可直接复用的经验清单

1. **装饰器式限流/缓存/退避**（WrappedLive）比在每个适配器里手写要可靠。
2. **单 goroutine 状态机 + 同步事件**是避免录制器竞态的关键；异步事件只用于通知类（日志、UI）。
3. **"秒退"检测 + 指数退避**必须内置，否则平台风控下会自伤。
4. **配置三层覆盖 + 指针区分未设置**，让"全局默认 / 平台 / 房间"三档共存。
5. **后处理失败保留全部文件**，宁可留垃圾也不能丢录像。
6. **先渲染 UI、后补数据**（InitializingLive）显著改善启动观感。
7. **文件名模板**用 Go template + 自定义函数（`filenameFilter` 过滤非法字符），并提供预览接口。
