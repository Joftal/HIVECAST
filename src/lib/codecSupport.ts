/** 当前 WebView 是否能通过 MSE 播放该视频编码；未知编码按兼容处理。 */
export function videoCodecSupported(codec: string | null | undefined): boolean {
  if (!codec || codec === "h264" || codec === "unknown") return true;
  if (typeof MediaSource === "undefined" || typeof MediaSource.isTypeSupported !== "function") {
    return false;
  }
  const types =
    codec === "h265"
      ? ['video/mp4; codecs="hvc1.1.6.L123.B0"', 'video/mp4; codecs="hev1.1.6.L123.B0"']
      : codec === "av1"
        ? ['video/mp4; codecs="av01.0.05M.08"']
        : [];
  return types.some((type) => MediaSource.isTypeSupported(type));
}

/** 录制链路（ffmpeg 分片 + 后处理）目前只支持 H.264/H.265；AV1 取流会生成空文件。 */
export function recordingCodecSupported(codec: string | null | undefined): boolean {
  return codec !== "av1";
}

export function codecLabel(codec: string | null | undefined): string {
  if (codec === "h265") return "HEVC";
  if (codec === "av1") return "AV1";
  if (codec === "h264") return "H.264";
  return codec ? codec.toUpperCase() : "";
}
