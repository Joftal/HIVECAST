//! 前后端共享的 IPC 数据契约。
//!
//! 这里的类型由 ts-rs 导出成 TypeScript（`pnpm gen:bindings`），
//! 因此任何字段变更都会同步反映到 `src/lib/bindings/`，避免手写两份定义漂移。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// 应用与运行环境信息，用于前端环境自检面板。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub arch: String,
    /// PATH 中探测到的 ffmpeg；未安装时为 None。
    pub ffmpeg_path: Option<String>,
    pub data_dir: String,
    pub cache_dir: String,
    /// 本地回环流服务的端口（每个进程随机）。
    pub loopback_port: u16,
}

/// 一次可播放的流会话描述。
///
/// 注意：这里是「租约」的快照，消费者拿到后不应期望它随别人的操作变化。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct StreamDescriptor {
    pub url: String,
    pub session_id: String,
    pub kind: String,
    pub quality: String,
    pub format: String,
}

/// 结构化 IPC 错误：前端只依赖 code 做分支，message 仅用于展示。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct IpcError {
    pub code: String,
    pub message: String,
}
