//! 时间统一入口：只依赖 jiff，避免再引入一个无人维护的时间库。
//!
//! 业务只关心三件事：Unix 秒/毫秒、系统本地时区、以及 B 站返回的
//! “北京时间、不带时区”的字符串。把这三件事收在这里，页面和平台代码
//! 不需要各自处理时区。

use jiff::{Timestamp, ZonedDateTime};

/// 当前 Unix 秒。
pub fn now_secs() -> i64 {
    Timestamp::now().as_second()
}

/// 当前 Unix 毫秒。
pub fn now_ms() -> i64 {
    Timestamp::now().as_millisecond()
}

/// 系统本地时区的当前时刻。
pub fn local_now() -> ZonedDateTime {
    ZonedDateTime::now()
}

/// 把 Unix 秒换成系统本地时区；越界时间回退到当前时刻。
pub fn local_at(secs: i64) -> ZonedDateTime {
    Timestamp::from_second(secs)
        .unwrap_or_else(|_| Timestamp::now())
        .to_zoned_datetime(jiff::tz::TimeZone::system())
}

/// 按系统本地时区格式化 Unix 秒；时间越界时返回 None。
pub fn format_local_opt(secs: i64, fmt: &str) -> Option<String> {
    Timestamp::from_second(secs).ok().map(|stamp| {
        stamp
            .to_zoned_datetime(jiff::tz::TimeZone::system())
            .strftime(fmt)
            .to_string()
    })
}

/// 按系统本地时区格式化当前时刻。
pub fn format_now_local(fmt: &str) -> String {
    local_now().strftime(fmt).to_string()
}

/// B 站的开播时间是不带时区的北京时间；按 UTC+8 解析后转 Unix 秒。
pub fn parse_cn_naive(text: &str) -> Option<i64> {
    let datetime = jiff::civil::DateTime::strptime("%Y-%m-%d %H:%M:%S", text).ok()?;
    let zoned = datetime.to_zoned_datetime(jiff::tz::TimeZone::UTC).ok()?;
    Some(zoned.timestamp().as_second() - 8 * 3600)
}
