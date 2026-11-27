//! 版本更新检测与多源更新日志拉取（方案 B：jsDelivr CDN -> GitHub Pages -> GitHub API）。
//!
//! 走后端 `reqwest` 发起请求，自动继承系统代理、避开 WebView CORS 限制并统一控制超时。

use semver::Version;
use std::time::Duration;

use hivecast_ipc::{ChangelogNote, ReleaseEntry, UpdateCheckResult};
use serde::Deserialize;

pub const RELEASES_URL: &str = "https://github.com/Joftal/HIVECAST/releases";
pub const CHANGELOG_SITE_URL: &str = "https://joftal.github.io/HIVECAST/";

const JSDELIVR_CHANGELOG_URL: &str =
    "https://cdn.jsdelivr.net/gh/Joftal/HIVECAST@gh-pages/changelog.json";
const GHPAGES_CHANGELOG_URL: &str = "https://joftal.github.io/HIVECAST/changelog.json";
const GITHUB_API_RELEASES_URL: &str =
    "https://api.github.com/repos/Joftal/HIVECAST/releases?per_page=100";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);

fn parse_version(raw: &str) -> Option<Version> {
    let tag = raw.trim();
    Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StaticChangelogFeed {
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

    let mut no_candidate = None;
    for (source, url) in [
        ("jsdelivr", JSDELIVR_CHANGELOG_URL),
        ("gh-pages", GHPAGES_CHANGELOG_URL),
        ("github-api", GITHUB_API_RELEASES_URL),
    ] {
        let fetched = if source == "github-api" {
            fetch_github_api_releases(&client, url).await
        } else {
            fetch_static_feed(&client, url).await
        };
        match fetched {
            Ok(releases) => {
                let result = build_result(current_version, source, releases);
                if result.latest_version.is_some() {
                    return result;
                }
                no_candidate.get_or_insert(result);
            }
            Err(err) => errors.push(format!("{source}: {err}")),
        }
    }
    if let Some(result) = no_candidate {
        return result;
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
    let mut releases = Vec::new();
    for page in 1.. {
        let resp = client
            .get(format!("{url}&page={page}"))
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        let more = resp
            .headers()
            .get(reqwest::header::LINK)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("rel=\"next\""));
        let text = resp.text().await.map_err(|e| e.to_string())?;
        releases.extend(parse_github_api_releases(&text)?);
        if !more {
            break;
        }
    }
    Ok(releases)
}

pub fn parse_static_feed(raw: &str) -> Result<Vec<ReleaseEntry>, String> {
    let feed: StaticChangelogFeed =
        serde_json::from_str(raw).map_err(|e| format!("JSON 解析失败：{e}"))?;
    Ok(feed.releases)
}

pub fn parse_github_api_releases(raw: &str) -> Result<Vec<ReleaseEntry>, String> {
    let items: Vec<GitHubApiRelease> =
        serde_json::from_str(raw).map_err(|e| format!("GitHub API 解析失败：{e}"))?;
    let mut out = Vec::new();
    for item in items.into_iter().filter(|r| !r.draft) {
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
    let mut guide_depth = None;
    let mut section_kind = "other";
    for raw_line in body.lines() {
        let line = raw_line.trim();
        if line.starts_with('#') {
            let title = line.trim_start_matches('#');
            let depth = line.len() - title.len();
            if guide_depth.is_none_or(|d| depth <= d) {
                let normalized = title.trim().to_ascii_lowercase();
                guide_depth = matches!(
                    normalized.as_str(),
                    "安装说明" | "installation" | "installation instructions" | "录制与更新"
                )
                .then_some(depth);
                if guide_depth.is_none() {
                    section_kind = classify_heading_kind(&normalized);
                }
            }
            continue;
        }
        if guide_depth.is_some() {
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
        let (kind, body_text) = classify_note(&cleaned, section_kind);
        if !body_text.is_empty() {
            notes.push(ChangelogNote {
                kind: kind.to_string(),
                text: body_text,
            });
        }
        if notes.len() >= 80 {
            break;
        }
    }
    notes
}

fn classify_heading_kind(lower_title: &str) -> &'static str {
    if lower_title.contains("新增")
        || lower_title.contains("feat")
        || lower_title.contains("feature")
    {
        "feat"
    } else if lower_title.contains("修复") || lower_title.contains("fix") {
        "fix"
    } else if lower_title.contains("优化")
        || lower_title.contains("perf")
        || lower_title.contains("体验")
        || lower_title.contains("改进")
    {
        "perf"
    } else {
        "other"
    }
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
    let cleaned = out.replace("**", "").replace('`', "");
    let trimmed = cleaned.trim();
    if let Some(open_idx) = trimmed.rfind('(') {
        if trimmed.ends_with(')') {
            let inner = &trimmed[open_idx + 1..trimmed.len() - 1];
            if (7..=40).contains(&inner.len()) && inner.bytes().all(|b| b.is_ascii_hexdigit()) {
                return trimmed[..open_idx].trim_end().to_string();
            }
        }
    }
    trimmed.to_string()
}

fn classify_note(text: &str, fallback_kind: &'static str) -> (&'static str, String) {
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
    (fallback_kind, text.to_string())
}

pub fn build_result(
    current_version: &str,
    source: &str,
    releases: Vec<ReleaseEntry>,
) -> UpdateCheckResult {
    let current = parse_version(current_version);
    let allow_prerelease = current.as_ref().is_some_and(|v| !v.pre.is_empty());

    let best = releases
        .iter()
        .filter(|r| allow_prerelease || !r.prerelease)
        .filter_map(|r| {
            parse_version(&r.version)
                .filter(|v| allow_prerelease || v.pre.is_empty())
                .map(|v| (v, r))
        })
        .max_by(|(va, _), (vb, _)| va.cmp_precedence(vb));

    let (latest_version, has_update, download_url) = match (current, best) {
        (Some(cur), Some((latest_v, entry))) => {
            let newer = latest_v.cmp_precedence(&cur).is_gt();
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
        error: latest_version
            .is_none()
            .then(|| "当前更新通道暂无有效发布".into()),
        latest_version,
        has_update,
        download_url,
        changelog_url: CHANGELOG_SITE_URL.to_string(),
        source: source.to_string(),
        releases: releases.into_iter().take(8).collect(),
    }
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
        releases: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_ordering_handles_stable_and_prerelease() {
        let v010 = parse_version("0.1.0").unwrap();
        let v020_rc1 = parse_version("v0.2.0-rc.1").unwrap();
        let v020_rc2 = parse_version("0.2.0-rc.2").unwrap();
        let v020 = parse_version("0.2.0").unwrap();

        assert!(v010 < v020_rc1);
        assert!(v020_rc1 < v020_rc2);
        assert!(v020_rc2 < v020);
        assert!(parse_version("invalid").is_none());
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
    fn choose_stable_before_limiting_history() {
        let mut raw = vec![
            serde_json::json!({"tag_name":"v2.0.0-rc.1","prerelease":true,"html_url":"rc"});
            8
        ];
        raw.push(serde_json::json!({"tag_name":"v1.1.0","html_url":"stable"}));
        let api = parse_github_api_releases(&serde_json::to_string(&raw).unwrap()).unwrap();
        let feed = serde_json::json!({"releases":api});
        for entries in [api, parse_static_feed(&feed.to_string()).unwrap()] {
            let r = build_result("1.0.0", "fixture", entries);
            assert!(r.has_update);
            assert_eq!(r.latest_version.as_deref(), Some("1.1.0"));
            assert_eq!(r.releases.len(), 8);
        }
        assert!(build_result("1.0.0", "fixture", vec![]).error.is_some());
        let notes = parse_release_notes("## 安装说明\n### Windows（x64）\n- setup\n## Windows fixes\n- fix: capture\n## macOS 修复\n- fix: playback");
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].text, "capture");
    }

    #[tokio::test]
    async fn api_follows_next_page_before_selecting_release() {
        use axum::{http::Uri, response::IntoResponse, routing::get, Router};
        let server = Router::new().route(
            "/releases",
            get(|uri: Uri| async move {
                match uri.query() {
                    Some("per_page=100&page=1") => (
                        [("link", "<next>; rel=\"next\"")],
                        r#"[{"tag_name":"v3.0.0-rc.1","prerelease":true,"html_url":"rc"}]"#,
                    )
                        .into_response(),
                    Some("per_page=100&page=2") => {
                        r#"[{"tag_name":"v2.0.0","html_url":"stable"}]"#.into_response()
                    }
                    _ => axum::http::StatusCode::BAD_REQUEST.into_response(),
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let url = format!(
            "http://{}/releases?per_page=100",
            listener.local_addr().unwrap()
        );
        let task = tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap();
        let entries = fetch_github_api_releases(&client, &url).await.unwrap();
        task.abort();
        assert_eq!(
            build_result("1.0.0", "github-api", entries)
                .latest_version
                .as_deref(),
            Some("2.0.0")
        );
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

        let categorized_md = r#"
## 更新内容

### ✨ 新增功能
- Windows 采用安装目录优先布局 ([0e54571](https://github.com/Joftal/HIVECAST/commit/0e54571))

### 🐛 问题修复
- 安装器浏览目录自动补齐 HIVECAST ([da080ba](https://github.com/Joftal/HIVECAST/commit/da080ba))

### ⚡ 优化与体验
- 对齐关于页图标视觉 ([c0dadcb](https://github.com/Joftal/HIVECAST/commit/c0dadcb))

### 🔧 工程与维护
- 新增自动化分诊工作流 ([da16d54](https://github.com/Joftal/HIVECAST/commit/da16d54))

---

## 安装说明

- **macOS**：拖拽安装
"#;
        let categorized = parse_release_notes(categorized_md);
        assert_eq!(categorized.len(), 4);
        assert_eq!(categorized[0].kind, "feat");
        assert_eq!(categorized[0].text, "Windows 采用安装目录优先布局");
        assert_eq!(categorized[1].kind, "fix");
        assert_eq!(categorized[1].text, "安装器浏览目录自动补齐 HIVECAST");
        assert_eq!(categorized[2].kind, "perf");
        assert_eq!(categorized[2].text, "对齐关于页图标视觉");
        assert_eq!(categorized[3].kind, "other");
        assert_eq!(categorized[3].text, "新增自动化分诊工作流");

        let many_lines = (1..=20)
            .map(|i| {
                format!("- 修复 `item-{i}` 同步问题 ([1234567](https://example.invalid/c/{i}))")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let many_notes =
            parse_release_notes(&format!("## 更新内容\n### 🐛 问题修复\n{many_lines}"));
        assert_eq!(many_notes.len(), 20);
        assert_eq!(many_notes[0].text, "item-1 同步问题");
        assert_eq!(many_notes[19].text, "item-20 同步问题");

        let offline = offline_result("1.0.0", "network error".into());
        assert!(offline.releases.is_empty());
    }
}
