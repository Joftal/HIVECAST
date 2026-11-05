import { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import Player from "./Player";
import { describeError, isTauri, openDemoStream, type StreamDescriptor } from "../../lib/ipc";

const SLOT_COUNT = 4;

// 预设布局：M2 会在这里接二叉分割树；当前只做视觉占位，避免假装已实现。
const PRESETS = ["single", "twoH", "twoV", "quad", "main"] as const;
const PRESET_LABELS: Record<(typeof PRESETS)[number], string> = {
  single: "1",
  twoH: "2 ⬍",
  twoV: "2 ⬌",
  quad: "4 ⬚",
  main: "4 主次",
};

function SlotCard({ index }: { index: number }) {
  const { t } = useTranslation();
  const [stream, setStream] = useState<StreamDescriptor | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadDemo = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setStream(await openDemoStream());
    } catch (err) {
      setError(describeError(err));
    } finally {
      setLoading(false);
    }
  }, []);

  return (
    <section className="panel flex min-h-0 flex-col overflow-hidden p-2">
      <div className="flex items-center gap-2 px-1.5 pb-2 pt-1 text-[11px] text-zinc-400">
        <span className="font-medium text-zinc-300">{t("watch.slot", { index: index + 1 })}</span>
        {stream && <span className="rounded-full bg-hive-500/15 px-1.5 py-0.5 text-[10px] text-hive-400">LIVE</span>}
        {stream && <span className="truncate text-zinc-500">{stream.quality}</span>}
      </div>

      <div className="relative min-h-0 flex-1">
        {stream ? (
          <Player url={stream.url} onFatal={(detail) => setError(detail)} />
        ) : (
          <div className="flex h-full min-h-[180px] flex-col items-center justify-center gap-2 rounded-xl border border-dashed border-white/10 bg-black/25 px-4 text-center">
            <span className="font-mono text-2xl text-zinc-700">0{index + 1}</span>
            <span className="text-[12px] text-zinc-500">{t("watch.empty")}</span>
            {isTauri() ? (
              <button
                type="button"
                onClick={() => void loadDemo()}
                disabled={loading}
                className="focus-ring mt-1 rounded-full border hairline bg-ink-700/70 px-3 py-1 text-[11px] text-zinc-200 transition-colors hover:bg-ink-600 disabled:opacity-50"
              >
                {loading ? t("common.loading") : t("watch.loadDemo")}
              </button>
            ) : (
              <span className="mt-1 text-[11px] text-zinc-600">{t("watch.pickRoom")} · Tauri only</span>
            )}
          </div>
        )}

        {error && (
          <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 rounded-xl bg-black/75 px-4 text-center">
            <span className="text-[12px] text-red-300">{t("watch.error")}</span>
            <span className="max-w-[90%] break-all font-mono text-[10px] text-zinc-500">{error}</span>
            <button
              type="button"
              onClick={() => {
                setError(null);
                setStream(null);
                void loadDemo();
              }}
              className="focus-ring rounded-full border hairline bg-ink-700 px-3 py-1 text-[11px] text-zinc-200 hover:bg-ink-600"
            >
              {t("watch.retry")}
            </button>
          </div>
        )}
      </div>
    </section>
  );
}

export default function WatchPage() {
  const { t } = useTranslation();

  return (
    <div className="flex h-full min-h-0 flex-col gap-4">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="text-[15px] font-semibold text-zinc-100">{t("watch.title")}</h1>
          <p className="mt-0.5 text-[12px] text-zinc-500">{t("watch.subtitle")}</p>
        </div>
        <div className="flex items-center gap-1.5" title={t("watch.qualityLocked")}>
          {PRESETS.map((preset) => (
            <span
              key={preset}
              className="cursor-not-allowed rounded-lg border hairline px-2 py-1 text-[11px] text-zinc-500"
            >
              {PRESET_LABELS[preset]}
            </span>
          ))}
        </div>
      </div>

      <div className="grid min-h-0 flex-1 grid-cols-1 gap-3 md:grid-cols-2 md:grid-rows-2">
        {Array.from({ length: SLOT_COUNT }, (_, index) => (
          <SlotCard key={index} index={index} />
        ))}
      </div>

      <p className="shrink-0 text-[11px] leading-5 text-zinc-600">{t("watch.demoTip")}</p>
    </div>
  );
}
