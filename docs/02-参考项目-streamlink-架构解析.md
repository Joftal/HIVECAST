# 02 · 参考项目 streamlink 架构解析

> 源码位置：`streamlink/`，包根 `streamlink/src/streamlink/`，CLI 在 `streamlink/src/streamlink_cli/`。
> 与 bililive-go 相反，streamlink 是"库 + CLI"：它不关心持续监听，只负责"给一个 URL，吐出可读的字节流"。这恰好是 HIVECAST 录制引擎应当学习的那一半。

## 1. 分层

```text
streamlink_cli/           命令行：参数解析、流选择、播放器/文件输出、进度条
      ↓
streamlink/api.py         Streamlink 门面（= session.Streamlink）
      ↓
session/                  Session（http、options、plugins、resolve_url、streams）
plugin/                   Plugin 基类、matchers、arguments、cache
plugins/                  约 150 个站点插件（每个 ~100-1000 行）
stream/                   流实现：hls / dash / http / ffmpegmux / segmented / file / filtered
buffers.py                RingBuffer（生产者-消费者缓冲）
webbrowser/               内建 Chromium（CDP）用于反爬站点取 token
```

## 2. Session（`session/session.py`，204 行）

```python
class Streamlink:
    def __init__(self, options=None, *, plugins_builtin=True, plugins_lazy=True):
        self.http = HTTPSession()
        self.options = StreamlinkOptions(self)
        self.plugins = StreamlinkPlugins(builtin=plugins_builtin, lazy=plugins_lazy)
```

关键方法：

| 方法 | 说明 |
| --- | --- |
| `resolve_url(url, follow_redirect=True)` | 先匹配插件；不匹配则 HEAD 跟随重定向（501 退回 GET）后递归。结果 `lru_cache(128)` |
| `streams(url, options=None, **params)` | 解析 URL → 实例化插件 → `plugin.streams()` |
| `set_option/get_option` | 全量选项集中管理（见 §7） |

> `resolve_url` 的"短链/跳转链"处理对国内平台很重要：抖音 `v.douyin.com` 短链、B 站 `b23.tv`、微博 `t.cn` 都需要先跳到落点再匹配平台。

## 3. Plugin（`plugin/plugin.py`，816 行）

- `@pluginmatcher(re.compile(...))` 注册 URL 规则（可多个），`Matcher(pattern, priority, name)`。
- `@pluginargument` 注册插件级 CLI 参数（自动加入 `--help`）。
- `Plugin.__init__(session, url, options)`：`self.options = Options(options)`、`self.cache = Cache("plugin-cache.json", key_prefix=module, disabled=no-plugin-cache)`。
- `_get_streams()`：**子类唯一必须实现的方法**，返回 `dict[name, Stream]` 或 `(name, stream)` 序列。
- `streams(stream_types=None, sorting_excludes=None)`：基类负责"名字归一化、类型优先级、去重、排序、best/worst 别名"。这是整个取流层最值钱的一段逻辑。

### 3.1 streams() 的规范化算法（可直接照搬）

1. 收集 `_get_streams()` 的结果（list 或 mapping）。
2. `stream_types` 默认 `["hls","http"]`，再追加实际出现过的类型；按 `stream_type_priority` 排序，不在列表里且没有 `*` 通配则丢弃。
3. 同名但类型不同 → 改名 `<name>_<type>`；仍冲突 → `name_alt`、`name_alt2`（最多两级）。
4. 名字必须匹配 `^([A-z0-9_+]+)`，否则丢弃；统一 `lower()`。
5. 按 `stream_weight` 排序，生成 `best`/`worst` 别名（全被过滤时给 `best-unfiltered`/`worst-unfiltered`）。
6. `stream_weight` 解析 `1080p60+` 这类名字：以分辨率像素数为主权重，fps 加成，`+` 与 bitrate 做微调。

> HIVECAST 的"清晰度选择器"应直接复用这套权重模型：平台给的 `Quality` 字符串五花八门（"原画"/"蓝光4M"/"1080p60"），统一折算成分数才能跨平台比较。

### 3.2 插件元数据的懒加载（`session/plugins.py`，366 行）

`StreamlinkPlugins(builtin, lazy)`：`lazy=True` 时读预构建的 plugin JSON 元数据（matchers/arguments），不 import 全部插件模块；元数据不可用则回退全量导入。由 `_build_matchers`/`_build_arguments` 构建索引。

> 150 个插件全量 import 会让启动多花上百毫秒。桌面端冷启动敏感，这个"元数据索引 + 按需加载"的做法值得照抄。

## 4. 流抽象（`stream/`）

```python
class Stream:
    __shortname__: str
    def to_url(self): ...
    def to_manifest_url(self): ...
    def open(self) -> StreamIO: ...

class StreamIO(io.IOBase): ...
```

| 实现 | 文件 | 用途 |
| --- | --- | --- |
| `HTTPStream` | `stream/http.py` | 直接透传 HTTP 响应体（FLV/TS 直链） |
| `HLSStream` | `stream/hls/hls.py`（954 行） | m3u8 直播/点播 |
| `DASHStream` | `stream/dash/dash.py`（453 行）+ `manifest.py`（1103 行） | mpd |
| `MuxedStream`/`FFMPEGMuxer` | `stream/ffmpegmux.py`（330 行） | 音视频分离时用 ffmpeg 合成 |
| `FilteredStream` | `stream/filtered.py` | 按 `hls-segment-ignore-names` 丢弃广告分片 |
| `StreamWrapper` | `stream/wrappers.py` | 只读包装（如限制时长/前缀） |
| `FileStream` | `stream/file.py` | 本地文件（测试用） |

统一契约是 `open() → StreamIO`，上层（CLI 的 `streamrunner`）只做 `read()` 写文件/管道。**这就是 HIVECAST 录制核心应有的形状**：所有下载器最终都返回一个 `AsyncRead`。

## 5. 分段流框架（`stream/segmented/segmented.py`，361 行）

三角色模型：

```text
SegmentedStreamWorker   生成器：iter_segments() 产出 (sequence, segment)
        │  (yield 后接收 queued 布尔：下游队列是否已满 → 决定是否继续拉)
        ↓
SegmentedStreamWriter   ThreadPoolExecutor 并发 fetch，queue.Queue(20) 按序写
        ↓
SegmentedStreamReader   StreamIO，读写之间是 RingBuffer(默认 16MiB)
```

细节：

- Writer 并发数 = `stream-segment-threads`（默认 1），单段重试 `stream-segment-attempts`（默认 3），超时 `stream-segment-timeout`（默认 10s）。
- Reader 的 `read(size)` 阻塞条件是 `writer.is_alive()`，超时 `stream-timeout`（默认 60s）。
- `check_queue_deadline`：超过 `max(5, targetduration * stream-segmented-queue-deadline)` 秒没有新分片即停止（默认 deadline=3），日志 `No new segments for more than X s. Stopping...`。
- Worker 负责 `check_sequence_gap`（序列号断档检测）与 `duration_limit` 提前停止。

> 这三个角色的拆分比"一个循环下载 + 一个管道"要健壮得多：**拉取（决定要什么）、下载（并发/重试）、输出（顺序/背压）各自独立**。HIVECAST 应保留这个分层，即使换成 async Rust。

## 6. HLS 实现要点（`stream/hls/`）

- `HLSStream(session, url, multivariant=None, name=None, force_restart=False, start_offset=0.0, duration=None)`。
- `parse_variant_playlist(..., check_streams=False|True|"playlists"|"segments")`：遍历 `multivariant.playlists`，跳过 iframe，用 `playlist.get_name(key="name")` 生成清晰度名，重名加 `_alt`。
- **音视频分离**：`external_audio` 存在且 FFmpeg 可用 → `MuxedHLSStream(video=..., audio=[...])`。
- **直播重载节奏** `hls-playlist-reload-time`：
  - `"default"` → `targetduration`；
  - `"segment"` → 最后一个分段时长；
  - `"live-edge"` → 最后 `live_edge-1` 段之和；
  - 数字（<2 视为 0 即用 targetduration）。
  - 播放列表未变化时 `_reload_time = max(_reload_time / 2, 1)`（退避）。
- **起播位置**：非 VOD 且未开 `hls-live-restart` 时，从倒数 `min(len(segments), hls-live-edge=3)` 段开始；开了则从第一段（看重播）。
- **AES-128**：`num_to_iv`/`create_decryptor`，`hls-segment-key-uri` 可改写 key 地址（绕过地域校验）。
- **EXT-X-BYTERANGE**：`ByteRangeOffset` 分别缓存"带/不带 byterange"两种偏移。
- **打包音频时间戳**：解析 ID3 PRIV `com.apple.streaming.transportStreamTimestamp`，给 MuxedHLSStream 设置 ffmpeg `itsoffset`/`copyts` 对齐音画。

## 7. RingBuffer（`buffers.py`，163 行）

- `Chunk(BytesIO)` + `deque`：读走整块就丢弃，避免大 buffer 反复扩容/搬移。
- `RingBuffer(size)`：`Lock` + `event_free`/`event_used` 两个 Event；写满等待、读空等待，`read(size, block=True, timeout)` 超时抛 `OSError("Read timeout")`。
- `close()` 必须同时置位两个 Event，否则等待方永久挂死。

> 这是"生产者（网络）与消费者（磁盘/播放器）速率不匹配"的标准解法。Rust 里等价物是 `tokio::sync::broadcast`/`async-channel` + `BytesMut` 环形缓冲，但**"写满阻塞 / 读空阻塞 / 关闭唤醒"三个语义必须齐备**。

## 8. FFMPEGMuxer（`stream/ffmpegmux.py`）

- `MuxedStream.open()` → 逐个打开子流 → 交给 `FFMPEGMuxer` 进程。
- `is_usable(session)` 先校验可执行文件存在。
- `copy_to_pipe`：用 NamedPipe 把每路流的字节喂给 ffmpeg，ffmpeg 输出 remux 结果到 stdout（再被上层读走）。
- 参数映射：`maps`（`["0:v?","0:a?"]` + 额外音频索引）、`format`（默认 mpegts）、`itsoffset`、`copyts`、`ffmpeg-fout`、`ffmpeg-video-transcode`、`ffmpeg-audio-transcode`、`ffmpeg-loglevel`。

## 9. 选项体系（`session/options.py`）

三类：

1. **传输/缓冲**：`ringbuffer-size`(16MiB)、`stream-timeout`(60s)、`stream-segment-attempts`(3)、`stream-segment-threads`(1)、`stream-segment-timeout`(10s)、`stream-segmented-duration`(0)、`stream-segmented-queue-deadline`(3)、`stream-passthrough-encrypted`(False)。
2. **HLS/DASH**：`hls-live-edge`(3)、`hls-live-restart`(False)、`hls-start-offset`(0)、`hls-playlist-reload-attempts`(3)、`hls-playlist-reload-time`("default")、`hls-segment-stream-data`(False)、`hls-segment-ignore-names`([])、`hls-segment-key-uri`(None)、`hls-audio-select`([])、`dash-manifest-reload-attempts`(3)。
3. **HTTP/外部**：`http-proxy`、`http-cookies`、`http-headers`、`http-query-params`、`http-ssl-verify`、`http-timeout`(20s)、`ffmpeg-*`、`webbrowser-*`。

> HIVECAST 配置应把这些"引擎级参数"与"业务配置"分开：引擎参数（缓冲大小、并发数、超时）通常按设备性能调，业务配置（房间、清晰度、命名）按用户习惯调。

## 10. CLI 层值得抄的交互细节（`streamlink_cli/`）

- `-o/--output` 支持元数据模板：`~/recordings/{author}/{category}/{id}-{time:%Y%m%d%H%M%S}.ts`，非法字符替换为下划线（`--fs-safe-rules {POSIX,Windows}`）。
- `-r/--record` 边播边录（不启动播放器）；`-O/--stdout` 输出到管道。
- `--default-stream "720p,480p,best"`：**逗号分隔的候选列表**，逐个尝试——比"单一清晰度"实用得多。
- `--stream-url`：只打印直链（不下载）。
- `--retry-streams`/`--retry-max`/`--retry-open`：重试策略三层。
- `--stream-types "hls,http,*"`、`--stream-sorting-excludes ">480p"`：类型与过滤。
- `--progress {yes,force,no}`、`--url`（打印最终解析 URL）、`--player-args`。

## 11. 内建浏览器（`webbrowser/`）

`Webbrowser` 基类：`names()`、`fallback_paths()`、`launch_args()`、`launch(headless, timeout)`；Chromium 实现用 trio 启动进程并通过 **CDP** 交互（`cdp/`）。用途：需要真实浏览器执行 JS 才能拿到 token/cookie 的站点（Twitch 的部分校验、强反爬站点）。

> 桌面端做这件事成本更低：可以直接内嵌 WebView 让用户登录，把 cookie 落到自己的 jar 里，不必驱动外部 Chromium。

## 12. 可复用清单

1. `Stream → StreamIO` 统一 I/O 契约。
2. `Worker / Writer / Reader` 三分段模型 + RingBuffer 背压。
3. `stream_weight` 清晰度评分 + `best/worst` 别名。
4. `--default-stream` 式"候选列表回退"。
5. 分片重试/超时/断档检测/无新分片超时停机。
6. 插件元数据懒加载（启动性能）。
7. HLS 直播重载节奏与起播位置（`live-edge`）。
