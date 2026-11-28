//! 后端文案本地化：错误信息与原生系统通知跟随界面语言。
//!
//! 前端负责应用内文案，但有两类文字必须由后端生成：
//! 1. IPC 返回的错误 message（会写进行内错误、观看状态、通知详情）；
//! 2. 启动/目录回落等发生在窗口就绪之前的系统通知。
//!
//! 语言来源是设置项 ui.language。这里只保存当前语言，翻译表覆盖
//! AppError::User 与 PlatformError 的固定文案和动态模板；未命中的
//! 文本原样返回，避免因为漏翻而吞掉信息。

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

const SYSTEM: u8 = 0;
const ZH_CN: u8 = 1;
const EN_US: u8 = 2;

static PREFERENCE: AtomicU8 = AtomicU8::new(SYSTEM);
static ENGLISH: AtomicBool = AtomicBool::new(false);

/// 按设置值切换语言；system 按环境变量判断。
pub fn set_preference(value: &str) {
    let preference = match value.trim() {
        "zh-CN" => ZH_CN,
        "en-US" => EN_US,
        _ => SYSTEM,
    };
    PREFERENCE.store(preference, Ordering::Relaxed);
    match preference {
        ZH_CN => ENGLISH.store(false, Ordering::Relaxed),
        EN_US => ENGLISH.store(true, Ordering::Relaxed),
        _ => ENGLISH.store(system_is_english(), Ordering::Relaxed),
    }
}

/// 前端已经解析好「跟随系统」时，直接使用解析后的语言。
pub fn set_effective(value: &str) {
    match value.trim() {
        "en-US" => {
            PREFERENCE.store(EN_US, Ordering::Relaxed);
            ENGLISH.store(true, Ordering::Relaxed);
        }
        "zh-CN" => {
            PREFERENCE.store(ZH_CN, Ordering::Relaxed);
            ENGLISH.store(false, Ordering::Relaxed);
        }
        _ => set_preference("system"),
    }
}

pub fn is_english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

/// 二选一文案，保持调用点的中英顺序一致。
pub fn text(zh: &'static str, en: &'static str) -> &'static str {
    if is_english() {
        en
    } else {
        zh
    }
}

/// 系统语言：LC_ALL > LC_MESSAGES > LANG，空值继续往下找。
fn system_is_english() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| !value.trim().is_empty())
        .map(|value| !value.trim().to_lowercase().starts_with("zh"))
        .unwrap_or(false)
}

/// 翻译一条后端文案。中文环境或未命中时原样返回。
pub fn translate(message: &str) -> String {
    if !is_english() {
        return message.to_string();
    }
    let msg = message.trim();
    if let Some(exact) = exact(msg) {
        return exact.to_string();
    }
    if let Some(dynamic) = dynamic(msg) {
        return dynamic;
    }
    if let Some(embedded) = translate_embedded(msg) {
        return embedded;
    }
    message.to_string()
}

/// 固定短句。
fn exact(msg: &str) -> Option<&'static str> {
    Some(match msg {
        "B 站登录已失效，请重新扫码登录" => {
            "Bilibili login has expired. Scan the QR code again."
        }
        "Cookie 已失效或未登录" => "The cookie has expired or you are not signed in.",
        "qrcode_key 不合法" => "Invalid qrcode_key.",
        "上一场录制尚未确认退出，请恢复录像目录后重新启动" => {
            "The previous recording has not exited cleanly. Restore the recording folder and restart."
        }
        "上一场正在收尾" => "The previous session is still being finalized.",
        "不能把日志写进 .app 包内，请选择包外的目录" => {
            "Logs cannot be written inside the .app bundle. Choose a folder outside it."
        }
        "会话已取消" => "The session was cancelled.",
        "凭据加密失败" => "Failed to encrypt credentials.",
        "凭据密钥初始化失败" => "Failed to initialize the credential key.",
        "只支持 http/https 链接" => "Only http/https links are supported.",
        "只支持有效的 http/https 链接" => "Only valid http/https links are supported.",
        "只有失败或已取消的后处理才能重试" => {
            "Only failed or cancelled post-processing can be retried."
        }
        "合并产物不能再参与合并，请选择原始录像" => {
            "Merged output cannot be merged again. Choose the original recordings."
        }
        "合并任务正忙，请稍后重试" => "A merge job is already running. Try again later.",
        "合并登记单与录像记录不匹配" => {
            "The merge manifest does not match the recording record."
        }
        "同一秒合并次数过多，请稍后重试" => {
            "Too many merges in the same second. Try again later."
        }
        "同名清单属于其他场次" => "A manifest with this name belongs to another session.",
        "媒体目录不能为空" => "The media folder cannot be empty.",
        "导出路径为空" => "The export path is empty.",
        "导出路径是一个目录，请选择 .csv 文件" => {
            "The export path is a folder. Choose a .csv file."
        }
        "尚未登录 B 站" => "Not signed in to Bilibili.",
        "尚未登录斗鱼" => "Not signed in to Douyu.",
        "布局数据过大" => "Layout data is too large.",
        "应用正在退出" => "The app is quitting.",
        "当前直播只提供 AV1 编码，录制链路暂不支持 AV1" => {
            "This stream is AV1-only, which the recording pipeline does not support yet."
        }
        "当前系统没有可打开的回收站目录" => "No trash folder is available on this system.",
        "录像不存在" => "Recording not found.",
        "录像文件已不存在" => "The recording file no longer exists.",
        "房间不存在" => "Room not found.",
        "房间已移除或监控已暂停" => "The room was removed or monitoring is paused.",
        "房间未开播" => "The room is not live.",
        "抖音 Cookie 已失效或未登录" => "The Douyin cookie has expired or you are not signed in.",
        "抖音 Cookie 缺少 sessionid，请在抖音网页登录后重新复制完整 Cookie" => {
            "The Douyin cookie is missing sessionid. Sign in on the Douyin website and copy the full cookie again."
        }
        "抖音账号信息缺少 uid" => "Douyin account info is missing uid.",
        "收尾仍未完成，缺失的分片或源文件可能已被清理" => {
            "Finalizing is still incomplete; missing segments or source files may have been cleaned up."
        }
        "收尾清单损坏，无法继续重试" => {
            "The finalize manifest is corrupted and cannot be retried."
        }
        "收尾重试锁被污染" => "The finalize retry lock is poisoned.",
        "房间写入后读取失败" => "Failed to read the room after writing it.",
        "文件已不存在" => "The file no longer exists.",
        "文件未登记在这条录像下" => "The file is not registered under this recording.",
        "斗鱼 Cookie 已失效或未登录" => "The Douyu cookie has expired or you are not signed in.",
        "斗鱼二维码 code 无效" => "Invalid Douyu QR code.",
        "斗鱼二维码内容为空" => "The Douyu QR code is empty.",
        "斗鱼二维码接口缺少 error 字段，可能触发了风控" => {
            "The Douyu QR code API response is missing the error field; risk control may have been triggered."
        }
        "斗鱼二维码接口返回格式异常" => {
            "The Douyu QR code API returned an unexpected format."
        }
        "斗鱼二维码返回缺少 data" => "The Douyu QR code response is missing data.",
        "斗鱼响应过大" => "The Douyu response is too large.",
        "斗鱼已确认登录，但没有返回回调地址" => {
            "Douyu confirmed sign-in but returned no callback URL."
        }
        "斗鱼接口返回的不是 JSON，可能触发了风控" => {
            "The Douyu API did not return JSON; risk control may have been triggered."
        }
        "斗鱼登录回调跳转到了非斗鱼域名" => {
            "The Douyu sign-in callback redirected to a non-Douyu domain."
        }
        "斗鱼登录已失效，请重新扫码登录" => {
            "Douyu login has expired. Scan the QR code again."
        }
        "斗鱼登录跳转到了非斗鱼域名" => {
            "The Douyu sign-in redirect went to a non-Douyu domain."
        }
        "斗鱼登录跳转地址无效" => "Invalid Douyu sign-in redirect URL.",
        "斗鱼登录跳转次数过多" => "Too many Douyu sign-in redirects.",
        "斗鱼登录跳转缺少 Location" => "The Douyu sign-in redirect is missing Location.",
        "日志目录不能是磁盘根目录" => "The log folder cannot be a disk root.",
        "日志目录必须是绝对路径" => "The log folder must be an absolute path.",
        "暂时无法读取收尾清单，请检查存储目录后重试" => {
            "The finalize manifest cannot be read right now. Check the storage folder and try again."
        }
        "有录像还在录制，先停下再合并" => {
            "A recording is still in progress. Stop it before merging."
        }
        "未登录 B 站，请先在设置中登录" => {
            "Not signed in to Bilibili. Sign in from Settings first."
        }
        "本地回放启动超时" => "Local replay took too long to start.",
        "没有可用的线路" => "No line is available.",
        "源封装就是当前主文件，不能只删源" => {
            "The source container is the main file; it cannot be deleted separately."
        }
        "源封装文件已不存在" => "The source container no longer exists.",
        "源封装文件已不存在，无法重试" => {
            "The source container no longer exists and cannot be retried."
        }
        "登录响应缺少 data" => "The login response is missing data.",
        "监控状态锁被污染" => "The monitoring state lock is poisoned.",
        "监控已暂停，恢复监控后才能开启自动录制" => {
            "Monitoring is paused. Resume monitoring before enabling auto-record."
        }
        "缺少 qrcode_key" => "Missing qrcode_key.",
        "自动录制已关闭" => "Auto-record is off.",
        "至少选两段录像才能合并" | "至少需要两段录像才能合并" => {
            "Select at least two recordings to merge."
        }
        "观看槽位已满（最多同时看 4 路）" => {
            "Viewing slots are full (up to 4 at once)."
        }
        "该房间连续录制失败，已暂停自动重试" => {
            "This room failed repeatedly; automatic retry is paused."
        }
        "这一场正在别的窗口重试收尾，请稍候" => {
            "This session is being finalized in another window. Please wait."
        }
        "这一场正在重试收尾，请稍候" => {
            "This session is being finalized. Please wait."
        }
        "这场没有弹幕" => "This session has no danmaku.",
        "这场直播还在录制，先停下再删除" => {
            "This stream is still being recorded. Stop it before deleting."
        }
        "这段录像不存在，可能已被清理" => {
            "This recording no longer exists and may have been cleaned up."
        }
        "这段录像包含多个片段，请删除整场" => {
            "This recording contains multiple segments. Delete the whole session."
        }
        "这段录像没有主文件" => "This recording has no main file.",
        "这段录像没有文件" => "This recording has no file.",
        "这段录像没有可处理的源文件" => "This recording has no processable source file.",
        "这段录像没有弹幕文件" => "This recording has no danmaku file.",
        "这段录像没有待恢复的收尾清单，可能已经收尾完成" => {
            "This recording has no pending finalize manifest; it may already be finalized."
        }
        "这段录像没有独立的源封装" => "This recording has no separate source container.",
        "这段还在录制，先停下再删源" => {
            "This session is still recording. Stop it before deleting the source."
        }
        "这段还在录制，先停下再删除" => {
            "This session is still recording. Stop it before deleting."
        }
        "这段还在录制，先停下再回放" => {
            "This session is still recording. Stop it before replaying."
        }
        "这段还在录制，先停下再处理" => {
            "This session is still recording. Stop it before processing."
        }
        "这段还在录制，先停下再重试收尾" => {
            "This session is still recording. Stop it before retrying finalize."
        }
        "连续开录失败，已暂停自动重试 5 分钟" => {
            "Recording failed repeatedly; automatic retry is paused for 5 minutes."
        }
        "链接包含非法控制字符" => "The link contains invalid control characters.",
        _ => return None,
    })
}

/// 动态模板：值已经格式化进字符串，只能按前后缀拆回来。
fn dynamic(msg: &str) -> Option<String> {
    if let Some(value) = between(msg, "槽位 ", " 没有在观看") {
        return Some(format!("Slot {value} is not being watched."));
    }
    if let Some(value) = after(msg, "无效的设置值：") {
        return Some(format!("Invalid setting value: {value}"));
    }
    if let Some(value) = after(msg, "自定义后处理命令不可用：") {
        return Some(format!(
            "Custom post-processing command unavailable: {value}"
        ));
    }
    if let Some(value) = after(msg, "文件名模板不可用：") {
        return Some(format!("Filename template unavailable: {value}"));
    }
    if let Some(value) = after(msg, "ffmpeg 路径不可用：") {
        return Some(format!("ffmpeg path unavailable: {value}"));
    }
    if let Some(value) = msg.strip_suffix(" 不是目录") {
        return Some(format!("{value} is not a folder."));
    }
    if let Some(value) = between(
        msg,
        "媒体目录与程序运行目录 ",
        " 重叠，请选择独立的录像目录",
    ) {
        return Some(format!(
            "The media folder overlaps the app folder {value}. Choose a separate recording folder."
        ));
    }
    if let Some(value) = after(msg, "不支持的平台：") {
        return Some(format!("Unsupported platform: {value}"));
    }
    if let Some(value) = after(msg, "斗鱼请求失败：") {
        return Some(format!("Douyu request failed: {value}"));
    }
    if let Some(value) = after(msg, "读取斗鱼响应失败：") {
        return Some(format!("Failed to read the Douyu response: {value}"));
    }
    if let Some(value) = after(msg, "读取斗鱼二维码失败：") {
        return Some(format!("Failed to read the Douyu QR code: {value}"));
    }
    if let Some(value) = after(msg, "斗鱼二维码生成失败：") {
        return Some(format!("Failed to generate the Douyu QR code: {value}"));
    }
    if let Some(value) = after(msg, "斗鱼登录回调地址无效：") {
        return Some(format!("Invalid Douyu sign-in callback URL: {value}"));
    }
    if let Some(rest) = msg.strip_prefix("生成二维码失败（code ") {
        if let Some((code, detail)) = rest.split_once("）") {
            return Some(format!(
                "Failed to generate the QR code (code {code}): {detail}"
            ));
        }
    }
    if let Some(value) = between(msg, "斗鱼二维码接口异常（HTTP ", "）") {
        return Some(format!("Douyu QR code API error (HTTP {value})"));
    }
    if let Some(value) = between(msg, "最多同时回放 ", " 条，先停掉一条再开") {
        return Some(format!(
            "At most {value} recordings can be replayed at once. Stop one first."
        ));
    }
    if let Some(value) = after(msg, "主文件校验未通过，不能删除源封装：") {
        return Some(format!(
            "Main file verification failed; the source container cannot be deleted: {value}"
        ));
    }
    if let Some(value) = after(msg, "录像仍待恢复，请先完成收尾再合并：") {
        return Some(format!(
            "The recording is still pending recovery. Finish finalizing before merging: {value}"
        ));
    }
    if let Some(value) = after(msg, "待登记成品已改变，请核对文件：") {
        return Some(format!(
            "The pending output has changed. Check the file: {value}"
        ));
    }
    if let Some(rest) = msg.strip_prefix("成品已保存在 ") {
        if let Some((path, detail)) = rest.split_once("，登记失败：") {
            let detail = detail
                .strip_suffix("。重新合并同一组片段可重试登记。")
                .unwrap_or(detail);
            return Some(format!(
                "The output was saved to {path}, but registration failed: {detail}. Merge the same segments again to retry registration."
            ));
        }
        if let Some((path, detail)) = rest.split_once("，保存登记单失败：") {
            return Some(format!(
                "The output was saved to {path}, but saving the registration manifest failed: {detail}"
            ));
        }
    }
    if let Some(value) = after(msg, "导出目录不存在：") {
        return Some(format!("Export folder not found: {value}"));
    }
    if let Some(value) = after(msg, "无法创建导出文件：") {
        return Some(format!("Failed to create the export file: {value}"));
    }
    if let Some(value) = after(msg, "写入导出文件失败：") {
        return Some(format!("Failed to write the export file: {value}"));
    }
    if let Some(rest) = msg.strip_prefix("有 ") {
        if let Some((count, paths)) =
            rest.split_once(" 段录像文件不存在、为空或无法读取，已停止合并：")
        {
            let paths = paths.trim_end_matches(['。', '、']);
            return Some(format!(
                "{count} recording files are missing, empty, or unreadable; merge stopped: {paths}"
            ));
        }
    }
    if let Some(rest) = msg.strip_prefix("录像无法读取，已停止合并：") {
        if let Some((path, detail)) = rest.rsplit_once("（") {
            if let Some(detail) = detail.strip_suffix("）") {
                return Some(format!(
                    "The recording cannot be read; merge stopped: {path} ({detail})"
                ));
            }
        }
    }
    // 平台层错误。
    if let Some(value) = after(msg, "不支持的平台或输入：") {
        let value = translate_embedded(value).unwrap_or_else(|| value.to_string());
        return Some(format!("Unsupported platform or input: {value}"));
    }
    if let Some(value) = after(msg, "房间不存在：") {
        return Some(format!("Room not found: {value}"));
    }
    if let Some(rest) = msg.strip_prefix("平台返回错误 ") {
        if let Some((code, detail)) = rest.split_once("：") {
            return Some(format!("Platform returned error {code}: {detail}"));
        }
    }
    if let Some(value) = after(msg, "网络请求失败：") {
        return Some(format!("Network request failed: {value}"));
    }
    if let Some(value) = after(msg, "响应解析失败：") {
        return Some(format!("Failed to parse the response: {value}"));
    }
    if let Some(rest) = msg.strip_prefix("平台请求过快，请 ") {
        if let Some(seconds) = rest.strip_suffix(" 秒后重试") {
            return Some(format!(
                "Too many platform requests. Try again in {seconds} seconds."
            ));
        }
    }
    None
}

/// 嵌在平台错误 detail 里的短句。
fn translate_embedded(value: &str) -> Option<String> {
    if value == "输入为空" {
        return Some("the input is empty".to_string());
    }
    if value == "房间未开播" {
        return Some("the room is not live".to_string());
    }
    if let Some(rest) = value.strip_prefix("暂不支持的直播地址：") {
        return Some(format!("unsupported live URL: {rest}"));
    }
    if let Some(rest) = value.strip_prefix("无法识别的输入：") {
        return Some(format!("unrecognized input: {rest}"));
    }
    None
}

fn after<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.strip_prefix(prefix)
}

fn between<'a>(text: &'a str, prefix: &str, suffix: &str) -> Option<&'a str> {
    text.strip_prefix(prefix)?.strip_suffix(suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_static_and_dynamic_messages() {
        let previous = is_english();
        set_effective("en-US");
        assert_eq!(translate("房间未开播"), "The room is not live.");
        assert_eq!(
            translate("槽位 2 没有在观看"),
            "Slot 2 is not being watched."
        );
        assert_eq!(
            translate("平台返回错误 -1：请求错误"),
            "Platform returned error -1: 请求错误"
        );
        assert_eq!(
            translate("不支持的平台或输入：暂不支持的直播地址：https://example.com"),
            "Unsupported platform or input: unsupported live URL: https://example.com"
        );
        assert_eq!(
            translate("有 2 段录像文件不存在、为空或无法读取，已停止合并：a、b。"),
            "2 recording files are missing, empty, or unreadable; merge stopped: a、b"
        );
        assert_eq!(
            translate("成品已保存在 /tmp/a，登记失败：db。重新合并同一组片段可重试登记。"),
            "The output was saved to /tmp/a, but registration failed: db. Merge the same segments again to retry registration."
        );
        set_effective(if previous { "en-US" } else { "zh-CN" });
    }
}
