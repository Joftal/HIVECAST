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
}

impl AppError {
    pub fn code(&self) -> &'static str {
        match self {
            AppError::Io(_) => "io",
            AppError::Ffmpeg(_) => "ffmpeg_failed",
            AppError::FfmpegMissing => "ffmpeg_missing",
            AppError::Path(_) => "path_invalid",
            AppError::Internal(_) => "internal",
        }
    }

    pub fn from_io(err: std::io::Error, path: impl Into<PathBuf>) -> Self {
        let path: PathBuf = path.into();
        AppError::Path(format!("{} ({})", path.display(), err))
    }
}

impl serde::Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = serializer.serialize_struct("AppError", 2)?;
        st.serialize_field("code", self.code())?;
        st.serialize_field("message", &self.to_string())?;
        st.end()
    }
}
