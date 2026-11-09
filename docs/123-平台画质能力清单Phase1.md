# 123-平台画质能力清单（Phase 1）

> 状态：v1 已实施（2026-11-09）；观看画质菜单改为读取平台真实能力清单，线路仍按 qn 获取。
> 范围：平台适配器、StreamBroker、`room_stream_options` 命令、观看页画质/线路菜单。

## 0. 结论速览

- 新增 `QualityInfo` / `StreamOptions` 能力模型：后端返回平台真实画质名称、补充信息、是否可用与编码。
- `PlatformAdapter` 新增 `stream_plan()`，一次取流同时返回线路组与画质清单；`stream_groups()` 保留为便捷方法。
- `StreamBroker` 缓存从“线路组”升级为“线路组 + 画质清单”的 `StreamPlan`，观看起播后菜单打开直接命中缓存，不额外触网。
- 新命令 `room_stream_options(id, qn)` 返回画质与线路；观看页用它替换原先的 `room_lines`。
- 画质菜单不再使用前端 `QN_OPTIONS`，直接渲染平台档位：B 站「杜比/4K/2K/原画/蓝光…」、斗鱼「原画1080P60/蓝光4M/超清/高清」、虎牙「蓝光10M/蓝光4M/超清/流畅」、抖音「原画/蓝光/超清/高清」。
- 不可用档位显示为禁用并提示「当前不可用」；平台缺失的档位不再伪造。
- 线路仍是二级视图；换线路保留当前 qn，缺档由后端回退。
- 统一 qn 继续作为持久化 ID（设置、房间、观看请求），平台名称只用于展示。

## 1. 数据模型

`src-tauri/ipc-types/src/lib.rs`：

```rust
pub struct QualityInfo {
    pub qn: u32,              // 应用内请求值，view_set_quality 使用
    pub label: String,        // 平台真实名称
    pub hint: Option<String>, // 码率/编码/不可用原因
    pub available: bool,
    pub codec: Option<String>,
}

pub struct StreamOptions {
    pub qualities: Vec<QualityInfo>,
    pub lines: Vec<LineInfo>,
}
```

平台内部的 `StreamPlan` 额外带 `Vec<StreamGroup>`，供录制与观看管线使用；IPC 只暴露画质与线路，不暴露候选地址。

## 2. 各平台能力来源

| 平台 | 画质 | 线路 |
| --- | --- | --- |
| B 站 | `g_qn_desc` 名称 + 全部 codec 的 `accept_qn` 并集；不在并集的档位标记不可用 | `url_info` 的 CDN host |
| 斗鱼 | `multirates` 的 name/rate/bit；同一 qn 保留码率更高的项 | `cdnsWithName` |
| 虎牙 | `vMultiStreamInfo` 中 `iCodecType=0` 的 H.264 档位，name 原样返回 | `gameStreamInfoList` 的 sCdnType |
| 抖音 | `collect_qualities()` 的 key/name/qn/bitrate/codec；同一 qn 保留码率更高的项 | 现有 CDN 解析 |

- B 站 qn 40000 仍在请求前归一化为 10000；能力清单里不会出现 40000。
- 虎牙 HDR/4K（codec 1）Phase 1 不列入可选档位；Phase 3 已列入清单并标记为暂不可用。
- 画质按 qn 降序排列；斗鱼/虎牙/抖音的标签直接使用平台名称。

## 3. 缓存与请求链路

```
观看起播 / 菜单打开
  → broker.plan(room, qn)
      ├─ 命中 StreamPlan 缓存：直接返回线路 + 画质
      └─ 未命中：adapter.stream_plan() 一次解析
  → room_stream_options() 映射为 { qualities, lines }
  → 前端 optionsCache 按 qn 保存
```

- 观看管线与画质菜单共用同一个 `StreamPlan` 缓存：起播后打开菜单通常零网络请求。
- 切画质后 `requestedQn` 变化，前端缓存失效并重新请求；broker 的并发去重与限速继续生效。
- 录制、续录、观看管线仍从 `StreamPlan.groups` 取候选，行为不变。

## 4. 前端菜单

- `quality.options` 由 `StreamOptions.qualities` 生成，附带 `hint` 与 `disabled`。
- 不可用项：`disabled` + 提示「当前不可用」，不能选择。
- 自动项仍由前端追加：`自动（实际画质）`；自动时 `requestedQn=0`。
- 能力清单加载前只补一个当前 qn 的占位项，避免入口空标签；加载完成后替换为平台完整列表。
- 线路视图逻辑不变：默认画质、线路二级、当前线路勾选、失效时显示实际线路。

## 5. 兼容与限制

- 旧的 qn 40000 / 20000 / 15000 配置仍可发给后端；B 站会归一化或按平台返回实际档位。
- 设置页（全局录制/观看默认画质）仍是通用 qn 名称，属于 Phase 2。
- 虎牙 4K/HDR、B 站杜比/4K 的编码与浏览器兼容性属于 Phase 3；当前只展示可用 H.264 档位，B 站不可用档位显示禁用。
- `LineInfo` 的 label 仍是通用的「线路 N + 平台名 hint」，线路展示的进一步平台化属于后续阶段。

## 6. 验证

- `cargo fmt --check`
- `cargo check`
- `cargo test --manifest-path src-tauri/ipc-types/Cargo.toml`（37 passed，已重新生成 TS 绑定）
- `pnpm exec tsc -b`
- `pnpm build`
- `pnpm check:i18n`：736 keys
- `pnpm check:docs`：124 files
