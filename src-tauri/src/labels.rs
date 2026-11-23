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

pub fn stage_label(stage: &str) -> &str {
    STAGE_LABELS
        .iter()
        .find_map(|(raw, label)| (*raw == stage).then_some(*label))
        .unwrap_or(stage)
}

pub fn state_label(state: &str) -> &str {
    STATE_LABELS
        .iter()
        .find_map(|(raw, label)| (*raw == state).then_some(*label))
        .unwrap_or(state)
}

/// 生成 SQLite CASE 表达式，让跨 Run 检索也能按界面中文匹配内部枚举。
/// 列名只由调用方传入固定标识符，标签本身是常量，不含用户输入。
pub fn stage_case_sql(column: &str) -> String {
    label_case_sql(column, STAGE_LABELS)
}

pub fn state_case_sql(column: &str) -> String {
    label_case_sql(column, STATE_LABELS)
}

fn label_case_sql(column: &str, labels: &[(&str, &str)]) -> String {
    let mut sql = format!("CASE {column}");
    for (raw, label) in labels {
        sql.push_str(&format!(" WHEN '{raw}' THEN '{label}'"));
    }
    sql.push_str(&format!(" ELSE {column} END"));
    sql
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_sql_cases_stay_in_sync() {
        assert_eq!(stage_label("record"), "录制");
        assert_eq!(stage_label("thumbnail"), "封面");
        assert_eq!(stage_label("preparing"), "准备中");
        assert_eq!(stage_label("converting"), "转换中");
        assert_eq!(stage_label("custom"), "custom");
        assert_eq!(state_label("running"), "运行中");
        assert_eq!(state_label("partial"), "部分完成");
        assert_eq!(state_label("custom"), "custom");

        let stage = stage_case_sql("e.stage");
        assert!(stage.starts_with("CASE e.stage"));
        assert!(stage.contains("WHEN 'record' THEN '录制'"));
        assert!(stage.ends_with("ELSE e.stage END"));
        let state = state_case_sql("e.state");
        assert!(state.contains("WHEN 'running' THEN '运行中'"));
    }
}
