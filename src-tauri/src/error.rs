use std::path::PathBuf;

/// 统一错误类型：IPC 返回给前端时转成 `{ code, message }`，
/// 前端只按 code 分支，message 仅用于展示与日志。
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("ffmpeg failed: {0}")]
    Ffmpeg(String),

    #[error("ffmpeg not found in PATH")]
    FfmpegMissing,

    #[error("path is not accessible: {0}")]
    Path(String),

    #[error("internal error: {0}")]
    Internal(String),

    /// 面向用户的拒绝原因（磁盘不足、冷却中等），message 直接展示，不加前缀。
    #[error("{0}")]
    User(String),

    #[error("database error: {0}")]
    Db(String),

    /// 平台侧错误：code 直接沿用 platform 层的错误码，前端按 code 分支。
    #[error("{message}")]
    Platform { code: &'static str, message: String },
}

impl AppError {
    pub fn code(&self) -> &'static str {
        match self {
            AppError::Io(_) => "io",
            AppError::Ffmpeg(_) => "ffmpeg_failed",
            AppError::FfmpegMissing => "ffmpeg_missing",
            AppError::Path(_) => "path_invalid",
            AppError::Internal(_) => "internal",
            AppError::User(_) => "user",
            AppError::Db(_) => "db",
            AppError::Platform { code, .. } => code,
        }
    }

    pub fn from_io(err: std::io::Error, path: impl Into<PathBuf>) -> Self {
        let path: PathBuf = path.into();
        AppError::Path(format!("{} ({})", path.display(), err))
    }

    /// 展示给用户/写进通知的文案。日志仍用 Display 的中文原文。
    pub fn localized_message(&self) -> String {
        match self {
            AppError::User(message) => crate::i18n::translate(message),
            AppError::Platform { message, .. } => crate::i18n::translate(message),
            AppError::Internal(message) => crate::i18n::translate(message),
            other => other.to_string(),
        }
    }
}

impl From<crate::platform::PlatformError> for AppError {
    fn from(err: crate::platform::PlatformError) -> Self {
        AppError::Platform {
            code: err.code(),
            message: err.to_string(),
        }
    }
}

impl serde::Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = serializer.serialize_struct("AppError", 2)?;
        st.serialize_field("code", self.code())?;
        st.serialize_field("message", &self.localized_message())?;
        st.end()
    }
}
