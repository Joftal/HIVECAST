//! 任务阶段/状态的中文展示名。
//!
//! 同一套映射同时用于 CSV 导出与跨 Run 检索：数据库里存内部枚举，
//! 用户在界面上看到的是中文，检索时输入中文必须能命中对应事件。

pub const STAGE_LABELS: &[(&str, &str)] = &[
    ("record", "录制"),
    ("finalize", "收尾"),
    ("post", "后处理"),
    ("commit", "入库"),
    ("merging", "合并"),
    ("splitting", "切片"),
    ("danmaku", "弹幕"),
    ("thumbnail", "封面"),
    ("exporting", "弹幕导出"),
    ("postprocessing", "后处理"),
    ("starting", "开录中"),
    ("recording", "录制中"),
    ("finalizing", "收尾中"),
    ("preparing", "准备中"),
    ("converting", "转换中"),
    ("validating", "校验中"),
    ("committing", "入库中"),
    ("queued", "等槽位"),
    ("done", "完成"),
    ("failed", "失败"),
    ("cancelled", "已取消"),
    ("interrupted", "已中断"),
    ("pending", "等待中"),
    ("skipped", "已跳过"),
    ("partial", "部分完成"),
];

/// 英文界面与英文 CSV 的同一套展示名，必须与 en-US.json 的 tasks.stage* 对齐。
pub const STAGE_LABELS_EN: &[(&str, &str)] = &[
    ("record", "Record"),
    ("finalize", "Finalize"),
    ("post", "Post"),
    ("commit", "Commit"),
    ("merging", "Merging"),
    ("splitting", "Splitting"),
    ("danmaku", "Danmaku"),
    ("thumbnail", "Cover"),
    ("exporting", "Danmaku export"),
    ("postprocessing", "Post-processing"),
    ("starting", "Starting"),
    ("recording", "Recording"),
    ("finalizing", "Finalizing"),
    ("preparing", "Preparing"),
    ("converting", "Converting"),
    ("validating", "Validating"),
    ("committing", "Committing"),
    ("queued", "Queued"),
    ("done", "Done"),
    ("failed", "Failed"),
    ("cancelled", "Cancelled"),
    ("interrupted", "Interrupted"),
    ("pending", "Pending"),
    ("skipped", "Skipped"),
    ("partial", "Partially completed"),
];

pub const STATE_LABELS: &[(&str, &str)] = &[
    ("running", "运行中"),
    ("active", "运行中"),
    ("queued", "排队中"),
    ("pending", "等待中"),
    ("skipped", "已跳过"),
    ("done", "完成"),
    ("failed", "失败"),
    ("cancelled", "已取消"),
    ("interrupted", "已中断"),
    ("recording", "录制中"),
    ("finalizing", "收尾中"),
    ("partial", "部分完成"),
];

pub const STATE_LABELS_EN: &[(&str, &str)] = &[
    ("running", "Running"),
    ("active", "Running"),
    ("queued", "Queued"),
    ("pending", "Pending"),
    ("skipped", "Skipped"),
    ("done", "Done"),
    ("failed", "Failed"),
    ("cancelled", "Cancelled"),
    ("interrupted", "Interrupted"),
    ("recording", "Recording"),
    ("finalizing", "Finalizing"),
    ("partial", "Partially completed"),
];

/// 语言标签只分中文/英文两套；`en-US`、`en` 都按英文处理，其余回退中文，
/// 与 i18n 只有 zh-CN / en-US 两个资源保持一致。
pub fn is_english(lang: Option<&str>) -> bool {
    lang.is_some_and(|value| value.trim().to_ascii_lowercase().starts_with("en"))
}

/// 按界面语言取展示名；未知内部值原样返回，避免把调试信息吞掉。
pub fn stage_label_for(lang: Option<&str>, stage: &str) -> String {
    let labels = if is_english(lang) {
        STAGE_LABELS_EN
    } else {
        STAGE_LABELS
    };
    labels
        .iter()
        .find_map(|(raw, label)| (*raw == stage).then_some((*label).to_string()))
        .unwrap_or_else(|| stage.to_string())
}

pub fn state_label_for(lang: Option<&str>, state: &str) -> String {
    let labels = if is_english(lang) {
        STATE_LABELS_EN
    } else {
        STATE_LABELS
    };
    labels
        .iter()
        .find_map(|(raw, label)| (*raw == state).then_some((*label).to_string()))
        .unwrap_or_else(|| state.to_string())
}

/// 检索命中的内部阶段值：同时匹配内部枚举、中文标签、英文标签。
///
/// 返回常量里的原始枚举，调用方可用 `IN (...)` 走索引；这样 Cover、
/// Post-processing 这类英文标签不依赖 FTS 分词也能命中。
pub fn matching_stages(query: &str) -> Vec<&'static str> {
    matching_enum_values(query, STAGE_LABELS, STAGE_LABELS_EN)
}

pub fn matching_states(query: &str) -> Vec<&'static str> {
    matching_enum_values(query, STATE_LABELS, STATE_LABELS_EN)
}

fn matching_enum_values(
    query: &str,
    zh: &'static [(&'static str, &'static str)],
    en: &'static [(&'static str, &'static str)],
) -> Vec<&'static str> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut values = Vec::new();
    for (raw, zh_label) in zh {
        let en_label = en
            .iter()
            .find_map(|(en_raw, label)| (*en_raw == *raw).then_some(*label))
            .unwrap_or(raw);
        if (raw.to_lowercase().contains(&needle)
            || zh_label.contains(&needle)
            || en_label.to_lowercase().contains(&needle))
            && !values.contains(raw)
        {
            values.push(*raw);
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_cover_both_languages() {
        assert_eq!(stage_label_for(None, "record"), "录制");
        assert_eq!(stage_label_for(Some("zh-CN"), "thumbnail"), "封面");
        assert_eq!(
            stage_label_for(Some("en-US"), "postprocessing"),
            "Post-processing"
        );
        assert_eq!(stage_label_for(Some("en"), "thumbnail"), "Cover");
        assert_eq!(stage_label_for(Some("en-US"), "custom"), "custom");
        assert_eq!(state_label_for(Some("en-US"), "running"), "Running");
        assert_eq!(
            state_label_for(Some("en-US"), "partial"),
            "Partially completed"
        );
        assert_eq!(state_label_for(None, "custom"), "custom");
    }

    #[test]
    fn matching_maps_chinese_english_and_raw_values() {
        assert_eq!(matching_stages("封面"), vec!["thumbnail"]);
        assert_eq!(matching_stages("Cover"), vec!["thumbnail"]);
        assert_eq!(matching_stages("cover"), vec!["thumbnail"]);
        assert_eq!(matching_stages("后处理"), vec!["post", "postprocessing"]);
        assert_eq!(matching_stages("post-processing"), vec!["postprocessing"]);
        assert_eq!(matching_stages("record"), vec!["record", "recording"]);
        assert_eq!(matching_states("Running"), vec!["running", "active"]);
        assert_eq!(matching_states("部分完成"), vec!["partial"]);
        assert!(matching_stages("zzz").is_empty());
        assert!(matching_states("").is_empty());
    }
}
