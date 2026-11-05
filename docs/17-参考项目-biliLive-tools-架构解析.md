# 17-参考项目-biliLive-tools-架构解析

> 参考仓库：`/Library/dshproject/biliLive-tools`（v4.0.0，GPL-3.0-only，Electron + Vue3 + TypeScript，pnpm monorepo）
> 官方文档：<https://docs.irenmu.com>
> 与另外两个参考项目的定位差异：bililive-go 是"服务端式录制器"，streamlink 是"取流库"，**biliLive-tools 是三者中唯一一个已经把「桌面端 + 多平台录制 + 弹幕 + 后处理 + 上传」全部做完的产品**。做 HIVECAST 时，它的**分层形状**比它的具体实现更值得抄。

---

## 1. 一句话概括

一个 Electron 应用壳里同时跑着：本地 HTTP 服务（Koa，前端通过 REST + SSE 调它）、录制管理器（每平台一个 provider，各自独立轮询）、取流库（StreamGet）、下载器（ffmpeg/mesio/bililive 三种可插拔）、SQLite 数据库、任务队列（ffmpeg 后处理/上传/同步）、以及一套 B 站弹幕链路（WS 监听 → XML 流式落盘 → DanmakuFactory 转 ASS → 压制进 MP4）。

关键判断：**它不是 Electron 应用直接操作业务，而是 Electron 里内嵌了一个"本地服务"**。渲染进程本质是个网页，通过 `http://127.0.0.1:<port>` 访问自己的后端，鉴权靠 `passKey`。这个选择让"桌面 GUI"和"无头服务器/CLI"共用同一套后端（仓库里确实另有 `build:cli` 与 `docker/`）。

---

## 2. 仓库结构

```text
biliLive-tools/
├── package.json            # pnpm@11.5.0，版本 4.0.0，scripts: dev / build:app / build:webui / build:cli / test
├── pnpm-workspace.yaml     # packages/*
├── .node-version
├── docs/                   # VitePress 文档站（guide / features / api / development）
├── docker/
├── patches/
└── packages/               # 15 个包
```

### 2.1 包与职责（按代码量排序）

| 包 | 文件/行数（ts+tsx+vue） | 职责 |
| --- | --- | --- |
| `app` | 160 / 39473 | Electron 主进程 + Vue3 渲染进程（UI 占了绝大部分） |
| `shared` | 118 / 28757 | 配置（`Config`/`AppConfig`）、SQLite（better-sqlite3 + awilix）、任务队列、弹幕 XML 解析/生成、枚举与默认配置、模板函数 |
| `http` | 46 / 16947 | Koa 服务：REST 路由 + SSE + 录像 WebSocket upgrade + `passKey` 鉴权 |
| `liveManager` | 22 / 6714 | **录制编排核心**：`Recorder` 抽象、`createRecorderManager`、三种下载器、Segment/StreamManager、XML 弹幕写入器 |
| `StreamGet` | 32 / 3183 | **多平台取流库**（可独立发布）：`PlatformParser` 抽象 + 注册表 + 各平台解析器 |
| `DouYuRecorder` | 8 / 2793 | 斗鱼 RecorderProvider |
| `DouYinRecorder` | 11 / 2718 | 抖音 RecorderProvider |
| `BilibiliRecorder` | 8 / 2099 | B 站 RecorderProvider（`stream.ts` 清晰度/流选择 + `danma.ts` 弹幕客户端） |
| `types` | 4 / 1903 | 跨包共享类型（`Recorder`、`LiveInfo`、`Message`…） |
| `HuYaRecorder` | 10 / 1853 | 虎牙 RecorderProvider |
| `TikTokRecorder` | 5 / 1187 | TikTok RecorderProvider |
| `DouYinDanma` | 5 / 805 | 抖音弹幕（走 btools 本地服务） |
| `XHSRecorder` | 4 / 557 | 小红书 RecorderProvider |
| `CLI` | 1 / 155 | CLI 入口 |
| `huya-danmu` | 1 / 43 | 虎牙弹幕 |

**读法建议**：不要从 `app` 开始读（4 万行 UI 噪音大）。按 **`StreamGet/types.ts` → `liveManager/recorder.ts` → `liveManager/manager.ts` → `BilibiliRecorder/{index,stream,danma}.ts` → `liveManager/downloader/*`** 这条链路读，一小时能拿到主干。

---

## 3. 启动链路

`packages/app/src/main/index.ts`（978 行）：

```text
app.requestSingleInstanceLock()  ──失败──> app.quit()
        │成功
        ▼
crashReporter(Sentry DSN, uploadToServer = false)
        │
app.whenReady()
        ├─ createWindow()        # 900x750，electron-store 记忆窗口位置/大小
        ├─ createMenu()          # 托盘/菜单
        ├─ genHandler(ipcMain)   # 注册全部 IPC handler
        └─ appInit()
              ├─ getConfigPath() → APP_CONFIG_PATH / FFMPEG_PRESET_PATH / VIDEO_PRESET_PATH
              │                    DANMU_PRESET_PATH / LOG_PATH / FFMPEG_PATH / FFPROBE_PATH
              │                    DANMUKUFACTORY_PATH / MESIO_PATH / BILILIVERECORDER_PATH
              │                    AUDIOWAVEFORM_PATH
              ├─ container = await init(globalConfig)     # awilix 依赖容器，顺带 initDB()
              ├─ serverStart({ port, host, auth: true, passKey }, container)
              ├─ nativeTheme.themeSource = ...
              ├─ menuBarVisible / setOpenAtLogin
              └─ powerSaveBlocker.start('prevent-app-suspension')
```

要点：

- **单实例锁 + 第二实例聚焦主窗口**（`second-instance`），桌面端必备。
- `crashReporter` 的 `uploadToServer` 硬编码 `false`——只本地留 dump，不偷偷上报。
- 服务端口来自配置 `appConfig.get('port')`，鉴权开关 `auth:true`，密钥是首次启动生成的 16 位随机 `passKey`。
- 所有外部二进制路径（ffmpeg/ffprobe/DanmakuFactory/mesio/bililive-recorder/audiowaveform）在启动时一次性解析出来，**不散落在业务代码里找 PATH**。这一点直接照抄。

---

## 4. 分层架构

```text
┌────────────────────────────── app（Electron + Vue3）──────────────────────────────┐
│  renderer: 页面/组件  ──fetch/EventSource──▶  http://127.0.0.1:port                 │
│  main: BrowserWindow / Tray / IPC(config、dialog、cookie、createSubWindow)          │
└───────────────────────────────────────┬───────────────────────────────────────────┘
                                        │ HTTP + SSE
┌───────────────────────────────────────▼───────────────────────────────────────────┐
│ http（Koa）: routes/{recorder,config,bili,task,video,danma,sync,webhook,sse,...}   │
│              authMiddleware(passKey) / upgrade → 录像 WS                            │
└───────────────────────────────────────┬───────────────────────────────────────────┘
        ┌───────────────────────────────┼───────────────────────────┐
        ▼                               ▼                           ▼
┌───────────────┐              ┌──────────────────┐        ┌──────────────────┐
│ liveManager   │              │ task（任务队列）  │        │ bili 上传/同步    │
│ Recorder 抽象 │              │ ffmpeg/上传/同步  │        │ （http + shared） │
│ Manager 轮询  │              └──────────────────┘        └──────────────────┘
│ Downloader    │
└───────┬───────┘
        │ 平台实现（每个 provider 一个包）
        ├─ BilibiliRecorder ──┐
        ├─ DouYuRecorder     │ 都用 StreamGet 取流
        ├─ HuYaRecorder      │
        ├─ DouYinRecorder ───┘
        ▼
┌───────────────────────────────┐        ┌─────────────────────────────┐
│ StreamGet（PlatformParser）    │        │ shared（Config / SQLite /   │
│ registry + http + per-platform│        │  danmu XML / 默认配置）      │
└───────────────────────────────┘        └─────────────────────────────┘
```

**两层平台抽象**是这套架构最重要的设计（详见 §6）：

1. `StreamGet.PlatformParser` —— **只负责"这个 URL 的直播间现在是什么状态、有哪些流"**，无状态、可独立测试、可单独发布。
2. `liveManager.RecorderProvider` + `Recorder` —— **负责"这个频道要不要录、录成什么样、什么时候停"**，有状态、挂在 manager 的轮询循环里。

HIVECAST 的 `PlatformAdapter` 如果只做一层，很快就会变成"取流 + 录制状态 + UI 字段"全塞一个类；这里的分层值得直接沿用。

---

## 5. StreamGet：可独立发布的取流库

### 5.1 契约（`packages/StreamGet/src/types.ts`）

```ts
// types.ts:23 起
export interface LiveInfo {
  platform: string;
  roomId: string;
  living: boolean;
  title: string;
  owner: string;
  avatar?: string;
  cover?: string;
  liveStartTime?: number;
  area?: string;
  raw?: any;              // 原始响应，调试/兜底用
}

// types.ts:38
export interface StreamInfo<Q = any> {
  url: string;
  quality: Q;             // 平台原始清晰度标识（B 站是 qn 数字）
  qualityDesc: string;    // 展示名（原画/蓝光/高清…）
  format: string;         // flv / ts / fmp4 ...
  [key: string]: any;     // 平台自定义字段（codec、host、cdn…）
}

// types.ts:47
export interface SourceInfo<Q = any> {
  name: string;           // "主线" / "备线1" / "线路2"
  streams: StreamInfo<Q>[];
}

// types.ts:60
export abstract class PlatformParser<Q = any> {
  platform: string;
  siteURL: string;
  options: RequestOptions;
  httpClient: HttpClient;

  abstract matchURL(url: string): boolean;
  abstract extractRoomId(url: string): Promise<string>;
  abstract getRoomInfo(roomId: string, opts?: RequestOptions): Promise<LiveInfo>;
  abstract getStreams(roomId: string, opts?: RequestOptions): Promise<SourceInfo<Q>[]>;

  // 基类提供：并行拿"房间信息 + 所有线路的流"
  async parse(url: string, opts?: RequestOptions): Promise<ParseResult> {
    const roomId = await this.extractRoomId(url);
    const [liveInfo, sources] = await Promise.all([
      this.getRoomInfo(roomId, opts),
      this.getStreams(roomId, opts),
    ]);
    return { liveInfo, sources };
  }
}

// RequestOptions: { cookie?, headers?, timeout?, proxy?: ProxyConfig|string, [key: string]: any }
// ParseResult:    { liveInfo, sources }
```

设计优点：

- `parse()` 用 `Promise.all` 并行，**房间信息和流信息互不阻塞**。
- `quality` 保留平台原值（B 站 qn=30000），`qualityDesc` 才给人看；跨平台比较清晰度靠上层映射表，不污染解析器。
- `StreamInfo` 允许任意扩展字段（`[key: string]: any`），B 站往里加了 `codec`/`host`/`cdn`，上层选流时能用。
- `raw` 保留原始响应，线上出问题能直接 dump 复现。

### 5.2 注册表（`registry.ts`）

```ts
// registry.ts:10
register(platform: string, factory: (options?: RequestOptions) => PlatformParser): void
// registry.ts:14
detectPlatform(url: string): string | null   // 遍历工厂实例调 matchURL
// registry.ts:24
create(platform: string, options?: RequestOptions): PlatformParser | null
```

`index.ts` 里注册：`bilibili / douyu / huya / xhs / xiaohongshu / tiktok`（`douyin` 被注释掉——抖音取流在 biliLive-tools 里走 btools 本地服务，不走 StreamGet）。

### 5.3 HttpClient（`src/http.ts`）

- 基于 **undici** `request` + `ProxyAgent`，不是 axios（取流路径轻量优先）。
- 默认 UA：`Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36`。
- `headersTimeout` 默认 10000ms。
- `getText()` **手动按 `Content-Encoding` 逆序解压** `br/gzip/deflate`（`decodeTextBody`）——undici 在部分站点不自动解压。
- `withRetry(fn, retries = 3)`，间隔 `1000 * (i + 1)` ms 线性退避。
- 所有网络异常统一包成 `NetworkError`。

### 5.4 错误模型（`src/errors.ts`）

```ts
StreamGetError(message, platform?, code?)
├─ UnsupportedPlatformError  code = 'UNSUPPORTED_PLATFORM'
├─ RoomNotFoundError         code = 'ROOM_NOT_FOUND'
├─ NotLivingError            code = 'NOT_LIVING'
├─ NetworkError              code = 'NETWORK_ERROR'
└─ ParseError                code = 'PARSE_ERROR'
```

**用 `code` 而不是靠 message 字符串判断**——上层 UI 要给"房间不存在/未开播/风控"不同文案与重试策略时，这非常关键。HIVECAST 的平台层从第一天就该有这套错误码。

### 5.5 B 站解析器（`src/bilibili/parser.ts` + `api.ts`）

| 步骤 | 端点 | 说明 |
| --- | --- | --- |
| 短号转真实房号 | `/room/v1/Room/room_init?id=` | `extractRoomId` 先取 `path.basename`，再换 `room_id`；顺带拿 `live_status`/`encrypted` |
| 房间信息 | `/room/v1/Room/get_status_info_by_uids?uids[]=` | title/uname/face/cover_from_user/live_time；失败回退 `/xlive/web-room/v1/index/getRoomBaseInfo` |
| 取流 | `/xlive/web-room/v2/index/getRoomPlayInfo` | 参数 `room_id, qn=30000, protocol=0,1, codec=0,1, format=0,1,2, only_audio=`，**必须带 Cookie 头** |
| buvid | `https://api.bilibili.com/x/frontend/finger/spi` | 弹幕/部分接口需要 |

取流结果处理：

```ts
// 三层遍历 stream → format → codec → url_info
url = urlInfo.host + codec.base_url + urlInfo.extra
// 按 accept_qn 生成 StreamInfo；qualityDesc 查 playurl.g_qn_desc
// 按 url_info 的下标分组线路：idx === 0 ? '主线' : ("备线" + idx)
```

注意 `living = roomInit.live_status === 1 && !roomInit.encrypted`——`encrypted` 是加密/付费直播，即使 `live_status=1` 也不该当作可录。

---

## 6. liveManager：录制编排核心

### 6.1 RecorderProvider 契约（`packages/liveManager/src/recorder.ts`）

```ts
export interface RecorderProvider<E extends AnyObject = UnknownObject> {
  id: string;                 // 'Bilibili' / 'DouYu' ...
  name: string;               // 展示名
  siteURL: string;
  matchURL(url: string): boolean;
  resolveChannelInfoFromURL(
    url: string,
    options?: E
  ): Promise<{ id: string; title: string; owner: string; uid?: string; avatar?: string }>;
  createRecorder(opts: RecorderCreateOpts<E>): Recorder<E> | undefined;
  fromJSON(json: any, opts: unknown): Recorder<E> | undefined;   // 从持久化恢复
  setFFMPEGOutputArgs(recorder: Recorder<E>): void;              // 允许平台改 ffmpeg 输出参数
}
```

`RecorderCreateOpts` 全字段（`recorder.ts:13`，**这张表就是"频道录制配置"的完整需求清单，HIVECAST 可以直接对照着设计自己的 schema**）：

```ts
{
  providerId, channelId, id?, remarks?, weight?,       // 身份与排序
  disableAutoCheck?,                                    // 不参与自动轮询
  disableProvideCommentsWhenRecording?,                 // 不录弹幕（但标题关键词检查仍会开 WS）
  quality, streamPriorities, sourcePriorities, formatPriorities?,  // 选流策略
  source?, segment?,                                    // 指定线路 / 分段
  saveGiftDanma?, saveSCDanma?, saveCover?,
  convert2Mp4?,                                         // 录完转封装
  auth?, cookieMode? ('all'|'stream'|'danma'),          // Cookie 分域使用
  uid?, qualityRetry?, doubleScreen?,                   // B 站画质重试/双屏
  useM3U8Proxy?, m3u8ProxyUrl?, customHost?,            // 拉流代理/自定义 CDN 域名
  formatName?, codecName?, api?,                        // 流格式与编码偏好
  titleKeywords?, segmentOnTitleChange?,                 // 标题门禁/标题变更分段
  videoFormat? ('auto'|'ts'|'mkv'|'flv'),
  recorderType? ('auto'|'ffmpeg'|'mesio'|'bililive'),   // 下载器后端
  onlyAudio?,
  handleTime? [start, end],                             // 时间段内才录
  useServerTimestamp?,                                  // 弹幕时间戳用服务端
  extra?, proxy?, debugLevel?,
}
```

### 6.2 Recorder 状态机与事件

```ts
type RecorderState =
  | 'idle'              // 空闲，等待开播
  | 'recording'
  | 'stopping-record'
  | 'check-error'       // 检查出错（网络/风控）
  | 'title-blocked'     // 标题命中关键词
  | 'charge-skipped'    // 付费/大航海/密码房，跳过
```

事件（`EventEmitter`，manager 转发给 `mitt` 总线 → SSE → UI）：

| 事件 | 载荷 | 用途 |
| --- | --- | --- |
| `RecordStart` | — | 开录 |
| `RecordSegment` | — | 分段切分 |
| `videoFileCreated` | `{ filename, cover?, rawFilename? }` | 文件创建（先落临时名，完成后改名） |
| `videoFileCompleted` | `{ filename, stats? }` | 文件完成，写入历史/触发后处理 |
| `progress` | `{ time }` | 从 ffmpeg stderr 解析的 `time=` |
| `RecordStop` | — | 停止 |
| `Updated` | `[prop]` | **任意字段变更**——靠 `new Proxy(recorder, { set })` 自动触发 |
| `Message` | 弹幕消息 | 实时弹幕推送（SSE `/sse/recorder/danma`） |
| `DebugLog` | 文本 | 调试日志 |
| `ChargeLive` | liveTypeDesc | 付费直播提示 |

`Proxy + set trap` 这个技巧很实用：几十个字段的 `recorder` 不需要手写 setter，任何赋值都会 emit `Updated`，前端就能统一做增量刷新。缺点是**性能与调试体验**（每次赋值一次事件），HIVECAST 若字段不多，用显式 setter 或 `update(patch)` 更稳。

### 6.3 单次检查流程（以 BilibiliRecorder 为例）

```text
checkLiveStatusAndRecord({ getSavePath, banLiveId, isManualStart, streamRetryHint })
  ├─ getLiveStatus() → { living, liveId, liveStartTime, owner, title }
  ├─ liveId === banLiveId ?           → tempStopIntervalCheck = true; return null   # 本场已手动停止
  ├─ !living ?                        → return null
  ├─ checkTitleKeywordsBeforeRecord() → 命中则 stateChange('title-blocked'); return null
  ├─ getInfo() → canRecord?
  │     └─ !canRecord → emit DebugLog + emit ChargeLive + stateChange('charge-skipped'); return null
  ├─ getStream({ strictQuality })     → 清晰度不满足且 qualityRetry > 0 时递减重试
  ├─ createDownloader(recorderType, { url, inputOptions, outputOptions, segment,
  │                                   getSavePath, formatName, videoFormat, headers, proxy })
  ├─ 可选：m3u8Proxy（每 50 分钟刷新一次真实 url）
  ├─ 灌弹幕：new DanmaClient(roomId, { auth, uid, useServerTimestamp })
  └─ 返回 RecordHandle { id, stream, source, recorderType, url, downloaderArgs, progress,
                         savePath, stop(reason?), cut() }
```

几个值得抄的细节：

- **`banLiveId` 而非布尔值**：手动停止只禁"这一场"，下次开播（`liveId` 变化）自动恢复。比"停止后关掉自动录制"体验好得多。
- **探测失败时按 normal 放行**：`getInfo()` 抛错时宁可尝试录制，也不误判成付费房漏录（"漏录比多录一次便宜"）。
- **`qualityRetry` 存 cache**：`cache.set('qualityRetryLeft', qualityRetry)` 在 stop 时重置，避免一次失败永久降级。
- **`segmentOnTitleChange`**：标题变化时 `downloader.type !== 'bililive' ? stop('标题变更分段') : downloader.cut()`——能切就切，不能切就重开。

### 6.4 管理器（`packages/liveManager/src/manager.ts`，820 行）

`createRecorderManager`（`manager.ts:263`）返回一个 `mitt` 事件管理器，核心是**每个 provider 一个独立 `setTimeout` 检查循环**（不是 setInterval，避免慢请求堆积）：

```text
startCheckLoop(providerId)
  ├─ 取 providerCheckConfig[providerId] 覆盖默认值
  │    autoCheckInterval 默认 60000ms
  │    maxThreadCount    默认 3
  │    waitTime          默认 0
  ├─ B 站开启 biliBatchQuery 时：先 getBiliStatusInfoByRoomIds 批量查
  │    失败则退回逐个查（批量接口省 N 次请求，但不可靠）
  ├─ multiThreadCheck(): range(0, maxThreadCount) 起线程池消费 needCheckRecorders
  └─ 本轮结束 setTimeout(下一轮, interval)
```

去重与保护机制（**这些是长期跑在生产里的经验，逐条都有 bug 故事**）：

| 机制 | 实现 | 解决什么 |
| --- | --- | --- |
| `tempBanObj[channelId] = liveId` | 手动停止后记录本场 liveId | 防止"手动停了又被自动重录" |
| `liveStartObj` | 按 channelId 去重 `RecorderLiveStart` | 轮询重复触发开播事件 |
| `chargeLiveObj` | 按场次去重 `ChargeLive` | 付费房每轮都提示一次，UI 刷屏 |
| 短录制判定 | `SHORT_RECORDING_THRESHOLD = 12 * 1000`，`SHORT_RECORDING_RETRY_COUNT = 2` | 连续秒退（流坏了/风控）后 `preferAlternativeStream` 换备用流（主要给抖音） |
| 黑名单 reason | `{'manual stop','remove recorder','直播间标题包含关键词','标题变更分段'}` | 这些停止原因不算"失败"，不该触发短录制重试 |
| `recordRetryImmediately` | 录制 ≥60s 且非 manual stop → 1s 后立即重查，`maxRetryCount = 10` | 主播短暂断流后快速续录 |
| `recorder.timeline` | 上限 `RECORDER_TIMELINE_LIMIT = 40` 条 | 给 UI 一个"最近发生了什么"的滚动日志，不用翻全局日志 |

### 6.5 保存路径

```ts
默认 savePathRule = '{platform}/{owner}/{year}-{month}-{date} {hour}-{min}-{sec} {title}'
// path.join 拼接 → ejs.render 渲染 → formatTemplate 格式化
// 文件名：removeSystemReservedChars、'%' → '_'
// 重名：extraMs 追加 '_' + ms
```

先 ejs 再 formatTemplate 的顺序是有意的：ejs 处理条件逻辑（`<% if (title) { %>`），formatTemplate 处理 `{year}` 这类占位符。HIVECAST 若只做一套模板，建议直接上 `{platform}/{owner}/{yyyy}-{MM}-{dd} {HH}-{mm}-{ss} {title}` 单层占位符 + 一个可选的 `{if}` 语法，别引两套。

### 6.6 下载器层

```ts
interface IDownloader {
  type: 'ffmpeg' | 'mesio' | 'bililive';
  hasSegment: boolean;
  segment: number | string;
  inputOptions: Record<string, any>;
  disableDanma: boolean;
  url: string;
  headers: Record<string, any>;
  proxy?: string;
  getSavePath: (opt?) => string;
  run(): void;
  stop(): void;
  cut(): void;
  getArguments(): string[];
  getExtraDataController(): XmlStreamController | null;
  videoFilePath: string;
  // 事件：videoFileCreated / videoFileCompleted / DebugLog / progress
}
```

`createDownloader(type, opts, onEnd, onUpdateLiveInfo)`：

- `selectRecorder` 默认 `'auto' → ffmpeg`；
- `getSourceFormatName(url)`：含 `.m3u8` → `ts`、含 `.m4s` → `fmp4`、否则 `flv`；
- `DEFAULT_USER_AGENT` 是 **Chrome/73**（不是最新的 120）——部分老平台对高版本 UA 更敏感；
- `bililive` 引擎不支持 `onlyAudio` 时回退 ffmpeg。

**FFmpegDownloader（`FFmpegDownloader.ts`，244 行）关键参数**：

```text
输入：
  -user_agent <UA>
  HLS:      -reconnect 1 -reconnect_streamed 1 -reconnect_delay_max 3
  代理:     -http_proxy <proxy>
  其他头:   -headers "K: V\r\nK2: V2\r\n"
输出：
  -c copy -movflags +frag_keyframe+empty_moov+separate_moof
  -fflags +genpts+igndts -min_frag_duration 10000000
分段：
  number  → -f segment -segment_time <segment*60> -reset_timestamps 1 -segment_format mp4
  string  → -fs <size>（按体积切，-fs 后 ffmpeg 自行收尾）
```

**fmp4 输出参数的理由**（注释里写了）：`empty_moov + frag_keyframe` 让文件从头就可播；进程被 KILL 时最多丢最后一个 fragment，而不是整个文件损坏。对"录制器随时可能被强杀"的场景，这是正确默认值。

**失败判定**：`createFFmpegInvalidStreamChecker(invalidCount)` —— HLS 连续 35 行、FLV 连续 18 行无效输出即判定流坏。**超时**：`createTimeoutChecker(30s)`（3×10s）。**停止语义**：`.ts` 发 `SIGINT`，其他格式往 ffmpeg stdin 写 `q`（`q` 是 ffmpeg 的优雅退出，能写完 moov）。

**进度**：正则 `/time=([0-9:.]+)/` 从 stderr 抓。

### 6.7 分段管理（`streamManager.ts`，308 行）

```text
Segment（单个分段）: getSavePath / videoExt / extraDataController(同名 .xml)
                   / rawRecordingVideoPath / outputVideoFilePath
  onSegmentStart(line)  ← 从 stderr 解析 ffmpeg 的 "Opening '...' for writing"
                          或 mesio 的 "segment path=..."
  文件已存在 → 用 extraMs 重新取路径
handleSegmentEnd: rename → flush xml → emit videoFileCompleted
命名: ffmpeg  → {recordSavePath}-PART%03d.ext
      mesio   → {recordSavePath}-PART%i.ext
      bililive→ 无 PART
```

**"先写临时名，段结束再 rename"** 是必须的：否则文件监听/后处理/云同步会读到写了一半的文件。HIVECAST 的 `videoFileCompleted` 语义应当严格定义为"rename 之后"。

---

## 7. 弹幕链路（B 站）

```text
DanmaClient (BilibiliRecorder/src/danma.ts, 290 行)
  ├─ buvid3: /x/frontend/finger/spi，缓存 24h，失败重试 3 次（1s/2s/4s 指数退避）
  ├─ startListen(roomId, handler, { ws: { headers: { Cookie }, uid, keepalive: false } })
  │     来自 blive-message-listener
  ├─ 自管重连: maxRetryCount = 10, 间隔 2000ms, connectionGeneration 防旧连接回写
  └─ 消息映射: comment{timestamp,text,color,mode,sender{uid,name,avatar}}
               super_chat{price,text}
               guard{name,price: price/1000, level}
               give_gift（SEND_GIFT_V2；coin_type==='silver' 时 price=0，否则 price/1000）
```

```text
xml_stream_controller.ts（335 行）createRecordExtraDataController(savePath)
  ├─ XML 头部 = <?xml?> + <?xml-stylesheet href="#s"?> + <i> + 内联 XSL（浏览器直接打开可看表格）
  ├─ metadata 占位符 <!--METADATA_PLACEHOLDER-->（首次写入时替换）
  ├─ 每 10s（DANMA_STATS_INTERVAL）flush 一次 pendingMessages；首次 writeFile(HEADER+content)，之后 append
  ├─ comment 的 @p = progress,mode,fontsize=25,color(hex→int),ctime,pool=0,midHash=uid,uid,weight=0
  │          另带 @user/@uid/@timestamp
  ├─ gift/sc/guard: @ts=进度秒, @price*1000
  ├─ flush(): 追加 </i>，清空内存
  └─ getStats(): danmaNum / uniqMember / scNum / guardNum / 10s 分桶 timeline
```

**为什么是"每 10 秒追加"而不是"录完一次性写"**：注释写得很直白——降低内存占用、降低数据丢失风险。对动辄 4~8 小时的直播，一次性在内存里攒几万条消息再序列化，OOM 风险实在太高。HIVECAST 的弹幕落盘应当照做（10s 或 5s 一批，强制 flush on stop）。

**转 ASS**：`DanmakuFactory(execPath).convertXml2Ass(input, output, config)`（`shared/src/danmu/danmakuFactory.ts`，138 行）拼命令行：

```text
"<exe>" -i "<in>" -o "<out>" --ignore-warnings --force --<key> <val> ...
```

- `resolution`/`msgboxsize`/`msgboxpos` 用 `x` 连接（`1920x1080`）；
- `blockmode` 用 `-` 连接；
- `opacity100` 映射成 `--opacity 0-255`；
- `fontname` 要加引号；
- `fontSizeResponsive` 用 `getFontSize(heightMap, height)` 线性插值/外推（斜率 0.03）。

**外部二进制 + 命令行参数拼接**是这套方案最脆的地方（路径含空格/中文、参数转义）。HIVECAST 若自己写 ASS 渲染器（而不是调 DanmakuFactory），能省掉一个 GPL 依赖和一类现场问题；若确实要调，用 `spawn(exe, argsArray)` 而不是拼字符串。

**解析**（`shared/src/danmu/index.ts`，296 行）：`>400MB` 的 XML 用 `readline` 逐行删 `raw="..."` 流式处理，小文件读全文 replace；`fast-xml-parser` 配 `isArray: ['d','gift','guard','sc']`。

---

## 8. 任务队列（`shared/src/task/core`）

```ts
abstract class AbstractTask {          // AbstractTask.ts:1
  taskId; pid; status; name; relTaskId; output; progress;
  custsomProgressMsg;                  // 拼写错误，原样保留
  action: ('pause'|'kill'|'interrupt'|'restart')[];
  startTime; endTime; error; limitTime; extra;
  abstract exec(): void; abstract kill(): void;
  abstract pause(): void; abstract resume(): void;
  getDuration(): number;
}
```

`TaskQueue`（289 行）：

- `runTask` 按 `TaskType` 查配置键限流：`ffmpegMaxNum`、`douyuDownloadMaxNum`、`biliUploadMaxNum`、`biliDownloadMaxNum`、`syncMaxNum`，**默认 3，-1 表示不限**；
- 同时校验 `isBetweenTimeRange(task.limitTime)`（允许"只在夜间跑上传"）；
- `addTask(task, autoRun = true)` 转发 `task-start/update/end/error/pause/resume/cancel` 事件（SSE `/sse/task/runningNum` 就靠这些）；
- 队列长度上限 `maxNum` 默认 300，超出时**移除最老的 completed**（不是拒绝新任务）；
- `on('task-end'|'task-error'|'task-pause'|'task-cancel')` 触发补位。

**"按类型分别限流 + 全局队列上限 + 结束事件补位"** 是后处理流水线的标准骨架，HIVECAST 的 `10-后处理流水线` 可以直接按这个形状实现。

---

## 9. 配置与存储

### 9.1 Config（`shared/src/config.ts`）

```ts
class Config {                       // config.ts:1
  constructor(filePath)
  get(key, withCache?)               // 默认每次读文件（withCache 才走内存）
  set(key, value) / has / delete
  save()                             // writeFileSync(JSON.stringify(data, null, 2))
  // emit('update', newData, oldData, key)
}
class AppConfig extends Config {     // config.ts:79
  // init 时设默认 savePath = ~/Downloads（docker 下 /app/video）
  // 首次生成 16 位随机 passKey
}
export const appConfig = new AppConfig();   // config.ts:137 单例
```

用 `lodash.get/set/defaultsDeep` 做嵌套默认值。**默认每次读文件**这点对桌面端是对的（用户可能手改配置文件），但高频轮询路径要传 `withCache`。

### 9.2 SQLite（`shared/src/db`）

- `better-sqlite3` + **awilix** 依赖容器；`initDB(dbRootPath)` / `closeDB@@ / `backupDB(filename)` / `reconnectDB`。
- **主库与弹幕库分开**（`getMainDb` / `getDanmuDb`）——弹幕量大，分库避免拖慢主库。
- 服务：`statisticsService`、`streamerService`、`recordHistoryService`、`videoSubService`、`videoSubDataService`、`uploadPartService`、`danmuService`、`subtitleStyleService`、`virtualRecordService`。
- `BaseModel<T>(db, tableName)`（`baseModel.ts`，480 行）提供 `createTable/insert/upsert/insertMany(显式 BEGIN/COMMIT/ROLLBACK)/query/findOne/findMany/complexQuery(where/orWhere/groupBy/having/orderBy/limit/offset)/paginate/findBy/first/last/exists/count/update/updateMany/list/delete/deleteBy/softDelete/transaction`。
- `record_history` 表（`model/recordHistory.ts`，372 行）：`STRICT` 表，字段 `id, created_at, streamer_id, live_id, live_start_time, record_start_time, record_end_time, title, video_file, video_filename, video_duration, danma_num, interact_num, danma_stats_json, quick_hash`；zod schema 校验；`migrate()` 用 `PRAGMA table_info` 判断列存在后 `ALTER TABLE ADD COLUMN`。

**`STRICT` 表 + zod 双保险**、**`PRAGMA table_info` 手工迁移**（不引 ORM 迁移框架）这两点，对"单机应用、表不多、要能降级回滚"的 HIVECAST 是合理取舍。

---

## 10. HTTP 服务（`packages/http`）

`src/index.ts`（175 行）：

```text
Koa + @koa/router + cors + @koa/bodyparser
  authMiddleware: Authorization 头 或 ?auth= 等于 passKey
                  （env BILILIVE_TOOLS_PASSKEY 优先；跳过路径含 /common/video/）
  路由: webhook config llm user common preset recorder bili douyu
        task assets video recordHistory files danma sync ai sse
  http.createServer().on('upgrade', handleRecorderUpgrade)   # 录像 WebSocket
  isHttps = false（硬编码）
```

SSE 端点（`routes/sse.ts`，197 行，基于 `koa-sse-stream`）：

| 端点 | 数据源 |
| --- | --- |
| `/sse/streamLogs` | chokidar 监听日志文件，从上次字节偏移 `createReadStream` 增量推送 |
| `/sse/recorder/danma?id=` | 订阅 `recorderManager.manager` 的 `Message` 事件 |
| `/sse/task/runningNum` | taskQueue 的 `task-start/update/end/error/pause/resume/cancel` |
| `/sse/analyzerWaveform` | 音乐/切片波形分析进度 |

客户端断开时 `off` 监听（否则泄漏）。

**SSE 而不是 WebSocket** 做单向推送：实现简单、自动重连、能过大多数代理；只有"录像流"这种双向/二进制场景才用 WS。HIVECAST 的 IPC 事件若用 `webContents.send`，本质也是同构的单向推送，不需要 EventSource。

Webhook（`routes/webhook.ts`，316 行）：`POST /webhook/bililiverecorder`、`/webhook/blrec`（订阅 `VideoFileCreatedEvent`/`VideoFileCompletedEvent`）、`/webhook/oneliverec`（`video_transmux_finish`）、`/webhook/ddtv`、`/webhook/custom`；`GET /bili/stream` 返回 B 站拉流地址（给外部播放器/联调）。

**被动接收其他录制器的 webhook** 是个很聪明的功能：用户不必二选一，bililive-go/blrec/DDTV 录完的文件可以送进 biliLive-tools 做后处理与上传。HIVECAST 若要抢存量用户，这条"互操作入口"成本低、收益高。

---

## 11. Electron 外壳与 IPC

IPC handler 分三个文件（`handlers.ts` / `common.ts` / `cookie.ts`）：

| 通道 | 作用 |
| --- | --- |
| `config:set` / `config:get` / `config:getAll` / `config:save` | 配置读写（渲染进程也能改） |
| `ffmpeg:mergeAssMp4` | 弹幕压制，**返回 taskId**（进任务队列，不阻塞 IPC） |
| `dialog:openDirectory` / `openFile` / `save` | 系统对话框 |
| `common:relaunch` / `setOpenAtLogin` / `setTheme` / `setMenuBarVisible` / `createSubWindow` / `checkUpdate` | 应用级操作 |
| `cookie:*` | 平台 Cookie 读写 |

子窗口用 **Hash 路由**（`#/route`）—— 打包后 `file://` 协议下 BrowserRouter 会 404，Hash 最省事。HIVECAST 若用自定义协议（`hivecast://`）则可以避免这个限制。

**长任务一律走队列 + 返回 taskId**，IPC 只做"提交 + 订阅进度"。这是桌面端不卡 UI 的关键。

---

## 12. 映射到 HIVECAST

| biliLive-tools | HIVECAST 对应 | 是否直接沿用 |
| --- | --- | --- |
| `StreamGet.PlatformParser` | `PlatformAdapter`（`04-平台适配层设计`） | ✅ 契约形状几乎可照搬（含错误码） |
| `RecorderProvider` | 与 Adapter 合并，或拆成 `RecordingProfile` | ⚠️ 建议合并——我们没有"外部注入 provider"的需求 |
| `Recorder` 状态机 | `RecordingSession` 状态机 | ✅ 5 个状态 + `banLiveId` 语义 |
| `createRecorderManager` 轮询 | `监听调度器` | ✅ 但用单一定时器 + 优先级队列，而非每 provider 一个 timer |
| `IDownloader` | `RecorderEngine` 后端接口 | ✅ 保留 `ffmpeg` + 自研 HTTP 下载器两条 |
| `Segment/StreamManager` | 分段与文件生命周期 | ✅ 先写临时名再 rename |
| `xml_stream_controller` | 弹幕落盘（`09-弹幕子系统`） | ✅ 10s 批量追加 |
| `DanmakuFactory` 外部进程 | 自研 ASS 渲染 或 内置 | ⚠️ 外部 GPL 二进制是负担 |
| `TaskQueue` | 后处理流水线（`10`） | ✅ 按类型限流 + 完成补位 |
| `BaseModel` + STRICT 表 | 数据层（`12`） | ✅ 但要加 WAL 与迁移版本号 |
| Koa + passKey + SSE | Electron IPC | ❌ 不需要本地 HTTP 服务（除非要 CLI/远程访问） |
| awilix DI 容器 | 手写工厂/模块单例 | ❌ 15 个包才需要 DI，我们不需要 |
| 每平台一个 npm 包 | `src/platforms/<name>.ts` 单文件 | ❌ 避免 monorepo 复杂度 |

### 12.1 值得抄的 7 个点

1. **取流与录制两层分离**，取流层无状态可单测。
2. **错误码枚举**（`UNSUPPORTED_PLATFORM/ROOM_NOT_FOUND/NOT_LIVING/NETWORK_ERROR/PARSE_ERROR`）。
3. **`banLiveId` 按场次禁录**，而不是"关掉自动录制"。
4. **付费/加密房探测失败时放行**（宁可多录，不可漏录），并用独立状态 `charge-skipped` 让 UI 说明原因。
5. **ffmpeg fmp4 输出参数**（`+frag_keyframe+empty_moov+separate_moof`、`-min_frag_duration 10000000`），抗强杀。
6. **弹幕 10s 批量追加 + 停止时强制 flush**。
7. **短录制重试 + 备用流切换 + 停止原因黑名单**，这三件套解决"录了 3 秒就断"的现场问题。

### 12.2 必须避开的 6 个点

1. **每 provider 一个 `setTimeout` 循环**：5 个平台 = 5 条互不知情的定时器，暂停/恢复/改配置都容易漏。用**单调度器 + 每频道下次检查时间**。
2. **`Proxy` 拦截所有赋值发事件**：调试困难、易触发额外渲染。用显式 `update(patch)`。
3. **`isHttps = false` 硬编码 + 明文 passKey 走 query string**：本地也不该这么干（同机其他进程可读）。用 IPC 或 `127.0.0.1` + 随机 token header。
4. **`better-sqlite3` 原生模块**：Electron 需要 `electron-rebuild`，跨平台打包容易翻车。若能用 `node:sqlite`（Node 22+）或 `sql.js`/WASM 可省一大块构建麻烦（代价是性能）。
5. **外部二进制路径拼接命令行**：路径含空格/中文/引号时必炸。用 `spawn(exe, args)`。
6. **15 个包的 monorepo**：对单人/小团队是纯负担。单包 + 目录分层足够。

---

## 13. 关键常量速查

| 常量 | 值 | 位置 |
| --- | --- | --- |
| 自动检查间隔 | 60000 ms | `manager.ts` |
| 检查线程数 | 3 | `manager.ts` |
| 短录制阈值 | 12 s | `manager.ts:80` |
| 短录制重试次数 | 2 | `manager.ts` |
| timeline 上限 | 40 | `manager.ts:79` |
| 立即重查最小录制时长 | 60 s | `manager.ts` |
| 最大立即重试 | 10 | `manager.ts` |
| 弹幕落盘间隔 | 10 s | `xml_stream_controller.ts` |
| FFmpeg HLS 无效行阈值 | 35 | `FFmpegDownloader.ts` |
| FFmpeg FLV 无效行阈值 | 18 | `FFmpegDownloader.ts` |
| FFmpeg 超时检查 | 30 s | `FFmpegDownloader.ts` |
| 默认 ffmpeg 输出 | `-c copy -movflags +frag_keyframe+empty_moov+separate_moof -min_frag_duration 10000000` | `liveManager` |
| m3u8 代理刷新 | 50 min | `BilibiliRecorder/index.ts` |
| B 站弹幕重连上限 | 10 次 / 2000 ms | `BilibiliRecorder/danma.ts` |
| 任务队列上限 | 300 | `TaskQueue` |
| 各类型并发默认 | 3（-1 不限） | `TaskQueue` |
| buvid3 缓存 | 24 h | `BilibiliRecorder/danma.ts` |
| 弹幕 XML 流式阈值 | 400 MB | `shared/src/danmu/index.ts` |

---

## 14. 读完应该记住的三句话

1. **biliLive-tools 的产品完成度最高，但工程结构是按"多平台 + 多下载器 + 插件化"演化的结果，不是从零设计的最优解**——抄它的分层与现场经验，别抄它的包数量和 DI。
2. **"取流（无状态） / 录制（有状态） / 下载（可插拔）" 三段切分是这套代码里最值钱的东西**，它让新增平台只需要写一个 200~500 行的 provider。
3. **弹幕与文件生命周期（临时名 → rename → completed 事件）是长期运行录制器最容易出 bug 的两处**，biliLive-tools 在这两处的实现（10s 批量落盘、fmp4 抗损坏参数）可以直接照搬。
