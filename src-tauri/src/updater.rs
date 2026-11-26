//! 版本更新检测与多源更新日志拉取（方案 B：jsDelivr CDN -> GitHub Pages -> GitHub API）。
//!
//! 走后端 `reqwest` 发起请求，自动继承系统代理、避开 WebView CORS 限制并统一控制超时。

use std::cmp::Ordering;
use std::time::Duration;

use hivecast_ipc::{ChangelogNote, ReleaseEntry, UpdateCheckResult};
use serde::Deserialize;

pub const RELEASES_URL: &str = "https://github.com/Joftal/HIVECAST/releases";
pub const CHANGELOG_SITE_URL: &str = "https://joftal.github.io/HIVECAST/";

const JSDELIVR_CHANGELOG_URL: &str =
    "https://cdn.jsdelivr.net/gh/Joftal/HIVECAST@gh-pages/changelog.json";
const GHPAGES_CHANGELOG_URL: &str = "https://joftal.github.io/HIVECAST/changelog.json";
const GITHUB_API_RELEASES_URL: &str =
    "https://api.github.com/repos/Joftal/HIVECAST/releases?per_page=8";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemVer {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Option<String>,
}

impl SemVer {
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim().strip_prefix('v').unwrap_or(raw.trim());
        let (core, pre) = match trimmed.split_once('-') {
            Some((c, p)) if !p.is_empty() => (c, Some(p.to_string())),
            _ => (trimmed, None),
        };
        let mut parts = core.split('.');
        let major = parts.next()?.parse::<u64>().ok()?;
        let minor = parts.next()?.parse::<u64>().ok()?;
        let patch = parts.next()?.parse::<u64>().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
            pre,
        })
    }

    pub fn is_prerelease(&self) -> bool {
        self.pre.is_some()
    }
}

impl Ord for SemVer {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => compare_pre(a, b),
            })
    }
}

impl PartialOrd for SemVer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn compare_pre(a: &str, b: &str) -> Ordering {
    let mut ai = a.split('.');
    let mut bi = b.split('.');
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(nx), Ok(ny)) => nx.cmp(&ny),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StaticChangelogFeed {
    #[serde(default)]
    releases: Vec<ReleaseEntry>,
}

#[derive(Debug, Deserialize)]
struct GitHubApiRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    body: Option<String>,
}

/// 按多源回退策略检查最新版本并拉取结构化更新日志。
pub async fn check_for_updates(current_version: &str) -> UpdateCheckResult {
    let client = match reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(format!("HIVECAST/{current_version}"))
        .build()
    {
        Ok(c) => c,
        Err(err) => {
            return offline_result(current_version, format!("初始化网络客户端失败：{err}"));
        }
    };

    let mut errors: Vec<String> = Vec::new();

    for (source, url) in [
        ("jsdelivr", JSDELIVR_CHANGELOG_URL),
        ("gh-pages", GHPAGES_CHANGELOG_URL),
    ] {
        match fetch_static_feed(&client, url).await {
            Ok(releases) if !releases.is_empty() => {
                return build_result(current_version, source, releases);
            }
            Ok(_) => errors.push(format!("{source}: empty feed")),
            Err(err) => errors.push(format!("{source}: {err}")),
        }
    }

    match fetch_github_api_releases(&client, GITHUB_API_RELEASES_URL).await {
        Ok(releases) if !releases.is_empty() => {
            return build_result(current_version, "github-api", releases);
        }
        Ok(_) => errors.push("github-api: no published releases".to_string()),
        Err(err) => errors.push(format!("github-api: {err}")),
    }

    tracing::warn!(
        target: "hivecast_lib::updater",
        errors = %errors.join(" | "),
        "所有更新源均不可达，返回离线快照"
    );
    offline_result(
        current_version,
        "无法连接更新源，请检查网络或前往 GitHub Releases 查看".to_string(),
    )
}

async fn fetch_static_feed(
    client: &reqwest::Client,
    url: &str,
) -> Result<Vec<ReleaseEntry>, String> {
    let resp = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let text = resp.text().await.map_err(|e| e.to_string())?;
    parse_static_feed(&text)
}

async fn fetch_github_api_releases(
    client: &reqwest::Client,
    url: &str,
) -> Result<Vec<ReleaseEntry>, String> {
    let resp = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let text = resp.text().await.map_err(|e| e.to_string())?;
    parse_github_api_releases(&text)
}

pub fn parse_static_feed(raw: &str) -> Result<Vec<ReleaseEntry>, String> {
    let feed: StaticChangelogFeed =
        serde_json::from_str(raw).map_err(|e| format!("JSON 解析失败：{e}"))?;
    Ok(feed.releases.into_iter().take(8).collect())
}

pub fn parse_github_api_releases(raw: &str) -> Result<Vec<ReleaseEntry>, String> {
    let items: Vec<GitHubApiRelease> =
        serde_json::from_str(raw).map_err(|e| format!("GitHub API 解析失败：{e}"))?;
    let mut out = Vec::new();
    for item in items.into_iter().filter(|r| !r.draft).take(8) {
        let version = item
            .tag_name
            .trim()
            .strip_prefix('v')
            .unwrap_or(item.tag_name.trim())
            .to_string();
        let published_at = item
            .published_at
            .as_deref()
            .and_then(|s| s.get(0..10))
            .unwrap_or("")
            .to_string();
        let notes = parse_release_notes(item.body.as_deref().unwrap_or(""));
        out.push(ReleaseEntry {
            version,
            published_at,
            url: item.html_url,
            prerelease: item.prerelease,
            notes,
        });
    }
    Ok(out)
}

/// 从 GitHub Release Markdown 正文提取简明变更条目，跳过纯安装说明段落。
pub fn parse_release_notes(body: &str) -> Vec<ChangelogNote> {
    let mut notes = Vec::new();
    let mut in_install_guide = false;
    for raw_line in body.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            let lower = line.to_ascii_lowercase();
            in_install_guide = lower.contains("安装说明")
                || lower.contains("macos")
                || lower.contains("windows")
                || lower.contains("录制与更新");
            continue;
        }
        if in_install_guide {
            continue;
        }
        let bullet = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .map(str::trim);
        let Some(text) = bullet else {
            continue;
        };
        if text.is_empty() || text.starts_with("Full Changelog:") {
            continue;
        }
        let cleaned = strip_markdown_links(text);
        let (kind, body_text) = classify_note(&cleaned);
        if !body_text.is_empty() {
            notes.push(ChangelogNote {
                kind: kind.to_string(),
                text: body_text,
            });
        }
        if notes.len() >= 8 {
            break;
        }
    }
    notes
}

fn strip_markdown_links(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some(mid) = input[i + 1..].find("](") {
                let label_end = i + 1 + mid;
                if let Some(end_rel) = input[label_end + 2..].find(')') {
                    out.push_str(&input[i + 1..label_end]);
                    i = label_end + 2 + end_rel + 1;
                    continue;
                }
            }
        }
        let ch = input[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out.replace("**", "").trim().to_string()
}

fn classify_note(text: &str) -> (&'static str, String) {
    let lower = text.to_ascii_lowercase();
    for (prefix, kind) in [
        ("feat:", "feat"),
        ("feat(", "feat"),
        ("新增：", "feat"),
        ("新增:", "feat"),
        ("新增 ", "feat"),
        ("fix:", "fix"),
        ("fix(", "fix"),
        ("修复：", "fix"),
        ("修复:", "fix"),
        ("修复 ", "fix"),
        ("perf:", "perf"),
        ("优化：", "perf"),
        ("优化:", "perf"),
        ("优化 ", "perf"),
    ] {
        if lower.starts_with(prefix) {
            let stripped = if prefix.ends_with('(') {
                text.split_once("):")
                    .map(|(_, rest)| rest.trim())
                    .unwrap_or(text)
            } else {
                text[prefix.len()..].trim()
            };
            return (kind, stripped.to_string());
        }
    }
    ("other", text.to_string())
}

pub fn build_result(
    current_version: &str,
    source: &str,
    releases: Vec<ReleaseEntry>,
) -> UpdateCheckResult {
    let current = SemVer::parse(current_version);
    let allow_prerelease = current.as_ref().is_some_and(SemVer::is_prerelease);

    let best = releases
        .iter()
        .filter(|r| allow_prerelease || !r.prerelease)
        .filter_map(|r| SemVer::parse(&r.version).map(|v| (v, r)))
        .max_by(|(va, _), (vb, _)| va.cmp(vb));

    let (latest_version, has_update, download_url) = match (current, best) {
        (Some(cur), Some((latest_v, entry))) => {
            let newer = latest_v > cur;
            (
                Some(entry.version.clone()),
                newer,
                if entry.url.trim().is_empty() {
                    RELEASES_URL.to_string()
                } else {
                    entry.url.clone()
                },
            )
        }
        (_, Some((_, entry))) => (
            Some(entry.version.clone()),
            false,
            if entry.url.trim().is_empty() {
                RELEASES_URL.to_string()
            } else {
                entry.url.clone()
            },
        ),
        _ => (None, false, RELEASES_URL.to_string()),
    };

    UpdateCheckResult {
        current_version: current_version.to_string(),
        latest_version,
        has_update,
        download_url,
        changelog_url: CHANGELOG_SITE_URL.to_string(),
        source: source.to_string(),
        error: None,
        releases,
    }
}

pub fn embedded_fallback_releases(current_version: &str) -> Vec<ReleaseEntry> {
    vec![ReleaseEntry {
        version: current_version.to_string(),
        published_at: "2026-11-26".to_string(),
        url: RELEASES_URL.to_string(),
        prerelease: current_version.contains('-'),
        notes: vec![
            ChangelogNote {
                kind: "feat".to_string(),
                text: "支持 Bilibili、斗鱼、虎牙、抖音四平台自动监控、无损分段录制、弹幕同步与本地画廊回放。".to_string(),
            },
            ChangelogNote {
                kind: "feat".to_string(),
                text: "双端液态玻璃极简安装器（macOS Retina DMG 与 Windows 独立现代安装/卸载器）。".to_string(),
            },
            ChangelogNote {
                kind: "fix".to_string(),
                text: "多段 FLV 合并增加包数完整性核验与实测时长校准，差值超过 3 秒时提示核对原片。".to_string(),
            },
        ],
    }]
}

fn offline_result(current_version: &str, error: String) -> UpdateCheckResult {
    UpdateCheckResult {
        current_version: current_version.to_string(),
        latest_version: None,
        has_update: false,
        download_url: RELEASES_URL.to_string(),
        changelog_url: CHANGELOG_SITE_URL.to_string(),
        source: "offline".to_string(),
        error: Some(error),
        releases: embedded_fallback_releases(current_version),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_ordering_handles_stable_and_prerelease() {
        let v010 = SemVer::parse("0.1.0").unwrap();
        let v020_rc1 = SemVer::parse("v0.2.0-rc.1").unwrap();
        let v020_rc2 = SemVer::parse("0.2.0-rc.2").unwrap();
        let v020 = SemVer::parse("0.2.0").unwrap();

        assert!(v010 < v020_rc1);
        assert!(v020_rc1 < v020_rc2);
        assert!(v020_rc2 < v020);
        assert!(SemVer::parse("invalid").is_none());
    }

    #[test]
    fn stable_client_ignores_prerelease_updates() {
        let releases = vec![
            ReleaseEntry {
                version: "0.2.0-rc.1".into(),
                published_at: "2026-11-26".into(),
                url: "https://github.com/Joftal/HIVECAST/releases/tag/v0.2.0-rc.1".into(),
                prerelease: true,
                notes: vec![],
            },
            ReleaseEntry {
                version: "0.1.0".into(),
                published_at: "2026-11-24".into(),
                url: "https://github.com/Joftal/HIVECAST/releases/tag/v0.1.0".into(),
                prerelease: false,
                notes: vec![],
            },
        ];

        let stable_res = build_result("0.1.0", "jsdelivr", releases.clone());
        assert!(!stable_res.has_update);
        assert_eq!(stable_res.latest_version.as_deref(), Some("0.1.0"));

        let pre_res = build_result("0.1.0-rc.1", "jsdelivr", releases);
        assert!(pre_res.has_update);
        assert_eq!(pre_res.latest_version.as_deref(), Some("0.2.0-rc.1"));
    }

    #[test]
    fn parses_static_changelog_feed_and_github_release_markdown() {
        let feed_json = r#"{
          "releases": [
            {
              "version": "0.2.0",
              "publishedAt": "2026-11-26",
              "url": "https://github.com/Joftal/HIVECAST/releases/tag/v0.2.0",
              "prerelease": false,
              "notes": [
                { "kind": "feat", "text": "液态玻璃安装器" }
              ]
            }
          ]
        }"#;
        let parsed = parse_static_feed(feed_json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].version, "0.2.0");

        let res = build_result("0.1.0", "jsdelivr", parsed);
        assert!(res.has_update);
        assert_eq!(res.latest_version.as_deref(), Some("0.2.0"));

        let md = r#"
## 安装说明
- macOS 请拖拽安装
## What's Changed
* feat: 液态玻璃安装器 in https://github.com/Joftal/HIVECAST/pull/1
* fix(merge): 修复 FLV 合并时长核验
* 优化：减少空闲轮询开销
"#;
        let notes = parse_release_notes(md);
        assert_eq!(notes.len(), 3);
        assert_eq!(notes[0].kind, "feat");
        assert_eq!(notes[1].kind, "fix");
        assert_eq!(notes[2].kind, "perf");
    }
}
