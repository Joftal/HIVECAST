import { useState } from "react";
import Player from "../watch/Player";

/** Development-only harness for checking the live player chrome without opening a Tauri window. */
export default function PlayerLab() {
  const [volume, setVolume] = useState(0);
  const [danmaku, setDanmaku] = useState(true);
  const [fit, setFit] = useState<"contain" | "cover">("contain");
  const [quality, setQuality] = useState(0);

  return (
    <div className="flex h-full min-h-0 flex-col bg-canvas p-6">
      <div className="min-h-0 flex-1 overflow-hidden bg-black">
        <Player
          url="https://test-streams.mux.dev/x36xhzz/x36xhzz.m3u8"
          volume={volume}
          onVolumeChange={setVolume}
          danmakuEnabled={danmaku}
          onToggleDanmaku={() => setDanmaku((value) => !value)}
          fit={fit}
          onToggleFit={() => setFit((value) => (value === "contain" ? "cover" : "contain"))}
          quality={{
            value: quality,
            options: [
              { value: 40000, label: "最高可用", hint: "源码率" },
              { value: 10000, label: "原画", hint: "15100 kbps · H264" },
              { value: 400, label: "蓝光", hint: "4000 kbps · H264" },
              { value: 250, label: "超清", hint: "2000 kbps · H264" },
              { value: 150, label: "高清", hint: "900 kbps · H264" },
              { value: 80, label: "流畅", hint: "500 kbps · H264" },
              {
                value: 20000,
                label: "4K HDR",
                hint: "当前播放链路不支持 HEVC",
                disabled: true,
              },
            ],
            actual: "1080P 蓝光",
            onChange: setQuality,
          }}
          lines={{
            value: null,
            options: [
              { value: null, label: "自动" },
              { value: "line-1", label: "线路 1", hint: "d1--cn-gotcha07.bilivideo.com" },
              { value: "line-2", label: "线路 2", hint: "d1--cn-gotcha104b.bilivideo.com" },
              { value: "line-3", label: "线路 3", hint: "cn-hnly-cu-01-01.bilivideo.com" },
            ],
            onChange: () => undefined,
          }}
        />
      </div>
    </div>
  );
}
