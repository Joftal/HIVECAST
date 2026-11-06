//! 录制文件名：模板渲染、跨平台清洗、同名去重。
//!
//! 目录结构固定为「媒体根/平台/主播」（可用设置关掉），模板只决定文件名。
//! 弹幕旁挂文件由视频路径换后缀得到（.jsonl / .ass），所以只要模板不改扩展名，
//! 弹幕永远和视频同名——这条契约由 docs/52 与单测一起锁住。

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};

/// 默认模板：平台-主播-标题-日期_时间。
pub const DEFAULT_TEMPLATE: &str = "{platform}-{host}-{title}-{date}_{time}";
/// 模板长度上限：再长就只是在浪费路径预算。
pub const MAX_TEMPLATE_CHARS: usize = 180;
/// Windows 默认路径上限 260，这里留出扩展名与同名后缀的余量。
const PATH_BUDGET: usize = 240;
const MAX_STEM_CHARS: usize = 120;
const MIN_STEM_CHARS: usize = 24;

/// 模板里允许出现的变量。前端提示文案也用这份清单，避免两处不一致。
pub const VARIABLES: [&str; 16] = [
    "platform",
    "platform_id",
    "host",
    "title",
    "area",
    "room_key",
    "qn",
    "date",
    "time",
    "datetime",
    "year",
    "month",
    "day",
    "hour",
    "min",
    "sec",
];

/// 渲染模板需要的房间信息。
pub struct NameParts<'a> {
    pub platform_id: &'a str,
    pub platform: &'a str,
    pub host: &'a str,
    pub title: &'a str,
    pub area: &'a str,
    pub room_key: &'a str,
    pub qn: u32,
}

/// 渲染模板并清洗；返回不带扩展名的文件名（stem）。
pub fn render(
    template: &str,
    parts: &NameParts,
    started_at: i64,
    strip_emoji: bool,
    max_chars: usize,
) -> Result<String, String> {
    let time = DateTime::from_timestamp(started_at, 0)
        .unwrap_or_else(chrono::Utc::now)
        .with_timezone(&Local);
    let mut out = String::with_capacity(template.len() + 32);
    let mut chars = template.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '{' {
            out.push(ch);
            continue;
        }
        let mut name = String::new();
        let mut closed = false;
        while let Some(next) = chars.next() {
            if next == '}' {
                closed = true;
                break;
            }
            name.push(next);
        }
        if !closed {
            return Err("模板里的 { 没有闭合".into());
        }
        let value = value_of(&name, parts, &time)?;
        out.push_str(&value);
    }

    let stem = sanitize(&out, "未命名直播", max_chars, strip_emoji);
    Ok(stem)
}

/// 只做校验，不落盘；设置保存与前端预览共用。
pub fn validate(template: &str) -> Result<(), String> {
    let trimmed = template.trim();
    if trimmed.is_empty() {
        return Err("模板不能为空".into());
    }
    if trimmed.chars().count() > MAX_TEMPLATE_CHARS {
        return Err(format!("模板最长 {MAX_TEMPLATE_CHARS} 个字符"));
    }
    let mut chars = trimmed.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '{' {
            continue;
        }
        let mut name = String::new();
        let mut closed = false;
        while let Some(next) = chars.next() {
            if next == '}' {
                closed = true;
                break;
            }
            name.push(next);
        }
        if !closed {
            return Err("模板里的 { 没有闭合".into());
        }
        if !VARIABLES.contains(&name.as_str()) {
            return Err(format!("未知变量 {{{name}}}"));
        }
    }
    Ok(())
}

fn value_of(name: &str, parts: &NameParts, time: &DateTime<Local>) -> Result<String, String> {
    let value = match name {
        "platform" => parts.platform.to_string(),
        "platform_id" => parts.platform_id.to_string(),
        "host" => parts.host.to_string(),
        "title" => parts.title.to_string(),
        "area" => parts.area.to_string(),
        "room_key" => parts.room_key.to_string(),
        "qn" => parts.qn.to_string(),
        "date" => time.format("%Y-%m-%d").to_string(),
        "time" => time.format("%H-%M-%S").to_string(),
        "datetime" => time.format("%Y-%m-%d %H-%M-%S").to_string(),
        "year" => time.format("%Y").to_string(),
        "month" => time.format("%m").to_string(),
        "day" => time.format("%d").to_string(),
        "hour" => time.format("%H").to_string(),
        "min" => time.format("%M").to_string(),
        "sec" => time.format("%S").to_string(),
        other => return Err(format!("未知变量 {{{other}}}")),
    };
    Ok(value)
}

/// 跨平台文件名清洗：非法字符、emoji、Windows 保留名、首尾点与空格、长度。
pub fn sanitize(input: &str, fallback: &str, max_chars: usize, strip_emoji: bool) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_was_space = false;
    for ch in input.chars() {
        if strip_emoji && is_emoji(ch) {
            continue;
        }
        let mapped = match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => '_',
            c => c,
        };
        if mapped.is_whitespace() {
            if last_was_space {
                continue;
            }
            last_was_space = true;
            out.push(' ');
        } else {
            last_was_space = false;
            out.push(mapped);
        }
    }
    let trimmed = out.trim().trim_matches('.').trim();
    let mut result: String = trimmed.chars().take(max_chars).collect();
    // 截断后可能又留下尾随点或空格，再清一次。
    result = result.trim_end_matches(['.', ' ']).to_string();
    if result.is_empty() {
        result = fallback.to_string();
    }
    // Windows 保留名：CON、PRN、AUX、NUL、COM1-9、LPT1-9（不区分大小写，带扩展名也算）。
    let base = result.split('.').next().unwrap_or("").trim().to_ascii_uppercase();
    let reserved = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4
            && (base.starts_with("COM") || base.starts_with("LPT"))
            && base.as_bytes()[3].is_ascii_digit()
            && base.as_bytes()[3] != b'0');
    if reserved {
        result.insert(0, '_');
    }
    result
}

/// 图形字符与「会让文件名变脏」的修饰符：ZWJ、变体选择符、肤色、旗帜、keycap、标签。
fn is_emoji(ch: char) -> bool {
    let c = ch as u32;
    matches!(c,
        0x200D | 0x20E3 | 0xFE0E | 0xFE0F
        | 0x1F3FB..=0x1F3FF
        | 0x1F1E6..=0x1F1FF
        | 0xE0020..=0xE007F
        | 0x1F000..=0x1FAFF
        | 0x2600..=0x27BF
        | 0x2B00..=0x2BFF
        | 0x2190..=0x21FF
    )
}

/// 同名时追加 (2)、(3)…；同时看旁挂的 .jsonl/.ass，避免视频换了名字、弹幕被覆盖。
pub fn unique_path(path: &Path) -> PathBuf {
    if !taken(path) {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let stem = path
        .file_stem()
        .map(|one| one.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = path.extension().map(|one| one.to_string_lossy().to_string());
    for index in 2..=99 {
        let name = match &ext {
            Some(ext) => format!("{stem} ({index}).{ext}"),
            None => format!("{stem} ({index})"),
        };
        let candidate = parent.join(name);
        if !taken(&candidate) {
            return candidate;
        }
    }
    path.to_path_buf()
}

fn taken(path: &Path) -> bool {
    path.exists()
        || path.with_extension("jsonl").exists()
        || path.with_extension("ass").exists()
}

/// 按目录长度算出文件名可用的字符预算，避免撞上 Windows 260 上限。
pub fn stem_budget(dir: &Path, ext: &str) -> usize {
    let dir_len = dir.to_string_lossy().chars().count();
    PATH_BUDGET
        .saturating_sub(dir_len + ext.chars().count() + 6)
        .clamp(MIN_STEM_CHARS, MAX_STEM_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts<'a>() -> NameParts<'a> {
        NameParts {
            platform_id: "bilibili",
            platform: "哔哩哔哩",
            host: "老实憨厚的笑笑",
            title: "今晚打排位 🎮",
            area: "英雄联盟",
            room_key: "6",
            qn: 10000,
        }
    }

    #[test]
    fn renders_default_template() {
        let ts = 1_791_366_450;
        let stem = render(DEFAULT_TEMPLATE, &parts(), ts, false, 120).expect("render");
        // 日期时间按本地时区渲染，测试里用同一套换算，避免 CI 换时区就挂。
        let local = chrono::DateTime::from_timestamp(ts, 0)
            .expect("时间戳")
            .with_timezone(&chrono::Local);
        let date = local.format("%Y-%m-%d").to_string();
        let time = local.format("%H-%M-%S").to_string();
        assert_eq!(
            stem,
            format!("哔哩哔哩-老实憨厚的笑笑-今晚打排位 🎮-{date}_{time}")
        );
    }

    #[test]
    fn strips_emoji_and_variation_selectors() {
        let stem = sanitize("标题🎮❤️👨‍👩‍👧", "兜底", 80, true);
        assert!(!stem.contains('🎮'));
        assert!(!stem.contains('❤'));
        assert!(!stem.contains('\u{200d}'));
    }

    #[test]
    fn rejects_unknown_variables() {
        assert!(validate("{host}-{nope}").is_err());
        assert!(validate("{host").is_err());
        assert!(validate("   ").is_err());
        assert!(validate(DEFAULT_TEMPLATE).is_ok());
    }

    #[test]
    fn escapes_windows_reserved_names() {
        assert_eq!(sanitize("CON", "x", 40, false), "_CON");
        assert_eq!(sanitize("com1.txt", "x", 40, false), "_com1.txt");
        assert_eq!(sanitize("console", "x", 40, false), "console");
    }

    #[test]
    fn unique_path_avoids_overwriting_sidecars() {
        let dir = std::env::temp_dir().join(format!("hivecast-name-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let base = dir.join("a.mp4");
        assert_eq!(unique_path(&base), base);
        std::fs::write(base.with_extension("jsonl"), "x").expect("write");
        assert_eq!(unique_path(&base), dir.join("a (2).mp4"));
        std::fs::write(dir.join("a (2).mp4"), "x").expect("write");
        assert_eq!(unique_path(&base), dir.join("a (3).mp4"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn budget_shrinks_with_long_dirs() {
        assert_eq!(stem_budget(Path::new("/tmp"), "mp4"), MAX_STEM_CHARS);
        let deep = format!("/tmp/{}", "a".repeat(300));
        assert_eq!(stem_budget(Path::new(&deep), "mp4"), MIN_STEM_CHARS);
    }
}
