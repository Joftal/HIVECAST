export type PlayerStats = {
  width: number;
  height: number;
  kbps: number;
  /** 浏览器自本次播放以来的累计丢帧数，仅用于判断是否出现过丢帧。 */
  dropped: number;
  /** 最近一次采样窗口内每秒新增的丢帧数。 */
  dropRate: number;
  /** 最近一次采样窗口内的丢帧占比（0–100）。 */
  dropRatio: number;
};

/** 丢帧展示为「新增帧率 + 窗口占比」；从未出现过丢帧时不显示。 */
export function formatDroppedFrames(stats: PlayerStats): string | null {
  if (stats.dropped <= 0) return null;
  const rate =
    stats.dropRate >= 10
      ? String(Math.round(stats.dropRate))
      : String(Math.round(stats.dropRate * 10) / 10);
  const ratio =
    stats.dropRatio >= 10
      ? String(Math.round(stats.dropRatio))
      : String(Math.round(stats.dropRatio * 10) / 10);
  return rate + "/s · " + ratio + "%";
}
