import { Button } from "../../components/ui/Controls";
import appMark from "../../assets/app-mark.png";

type Item = {
  id: string;
  poster: string;
  platform: keyof typeof PLAT;
  title: string;
  host: string;
  duration: string;
  size: string;
  when: string;
  danmaku: number;
  missing?: boolean;
};

const PLAT = {
  bilibili: { label: "哔哩哔哩", dot: "#22a7f0" },
  douyu: { label: "斗鱼", dot: "#ff7a1a" },
  huya: { label: "虎牙", dot: "#f5a623" },
  douyin: { label: "抖音", dot: "#ff2d6f" },
};

const ITEMS: Item[] = [
  { id: "s1", poster: "/__design/p2.jpg", platform: "bilibili", title: "德云色 S16 六点 BLG DK！", host: "老实憨厚的笑笑", duration: "3:12:44", size: "4.8 GB", when: "2026-11-06 19:02:18", danmaku: 12840 },
  { id: "s2", poster: "/__design/p1.jpg", platform: "bilibili", title: "【预告】7日6点 T1 vs KC", host: "哔哩哔哩英雄联盟赛事", duration: "1:48:12", size: "2.6 GB", when: "2026-11-06 17:30:05", danmaku: 6432 },
  { id: "s3", poster: "/__design/p3.jpg", platform: "douyu", title: "陪伴每一天！", host: "yyfyyf", duration: "2:20:08", size: "3.1 GB", when: "2026-11-06 15:10:41", danmaku: 9210 },
  { id: "s4", poster: "/__design/p4.jpg", platform: "huya", title: "【预告】29日4点 S16瑞士轮第五日", host: "虎牙英雄联盟赛事", duration: "0:52:30", size: "1.4 GB", when: "2026-11-05 22:05:12", danmaku: 2087 },
  { id: "s5", poster: "/__design/p5.jpg", platform: "bilibili", title: "先剪视频，晚上帮帮", host: "某主播", duration: "1:05:20", size: "1.9 GB", when: "2026-11-05 20:41:09", danmaku: 3310 },
  { id: "s6", poster: "/__design/p6.jpg", platform: "bilibili", title: "没出道成功 来直播!", host: "小柔", duration: "0:41:18", size: "980 MB", when: "2026-11-05 18:12:33", danmaku: 1204 },
  { id: "s7", poster: "/__design/p7.jpg", platform: "bilibili", title: "哎哟我", host: "哔哩哔哩直播", duration: "0:28:06", size: "620 MB", when: "2026-11-04 21:30:02", danmaku: 0, missing: true },
];

const COLS = "20px 120px 74px 82px minmax(0,1fr) 88px 82px 152px 104px";

function Dot({ color }: { color: string }) {
  return <span className="inline-block h-[6px] w-[6px] shrink-0 rounded-full" style={{ background: color }} />;
}

function Header() {
  const tabs = [
    { label: "监控", active: false },
    { label: "观看", active: false },
    { label: "媒体库", active: true },
  ];
  return (
    <header className="flex h-12 shrink-0 items-stretch border-b border-line pl-4 pr-3">
      <div className="flex items-center gap-2 pr-1">
        <img src={appMark} alt="" width={24} height={24} className="h-6 w-6 shrink-0 rounded-[6px]" />
        <span className="text-[11.5px] font-semibold tracking-[0.16em] text-ink">HIVECAST</span>
      </div>
      <nav className="ml-3 flex items-stretch">
        {tabs.map((tab) => (
          <span key={tab.label} className={"relative flex items-center px-3 text-[12px] font-medium " + (tab.active ? "text-ink" : "text-muted")}>
            {tab.label}
            {tab.active ? <span className="absolute inset-x-2 bottom-0 h-[2px] rounded-t-[2px] bg-accent" /> : null}
          </span>
        ))}
      </nav>
      <div className="ml-auto flex items-center gap-1 text-[11px] text-muted">
        <span className="px-2">中</span>
        <span className="px-2">☾</span>
        <span className="px-2">⚙</span>
      </div>
    </header>
  );
}

function ToolRow() {
  return (
    <div className="flex items-center gap-3 px-5 pb-3 pt-4">
      <h1 className="text-[19px] font-semibold tracking-tight text-ink">媒体库</h1>
      <span className="text-[11.5px] text-muted">7 条录像 · 共 14.8 GB</span>
      <div className="ml-auto flex items-center gap-2">
        <div className="flex h-8 w-[196px] items-center gap-1.5 rounded-[6px] border border-line bg-surface px-2.5 text-[11.5px] text-faint">
          <span className="text-[11px]">⌕</span>搜索标题或主播
        </div>
        <div className="flex h-8 items-center gap-1.5 rounded-[6px] border border-line bg-surface px-2.5 text-[11.5px] text-ink-soft">
          最近录制 <span className="text-[9px] text-faint">▾</span>
        </div>
        <Button size="sm" variant="primary">打开媒体目录</Button>
      </div>
    </div>
  );
}

function FilterRow({ mode }: { mode: "list" | "cover" }) {
  return (
    <div className="flex items-center gap-2 px-5 pb-2.5">
      <span className="flex h-7 items-center gap-1.5 rounded-[6px] bg-raised px-2.5 text-[11.5px] font-medium text-ink">
        全部 <span className="font-mono text-[10.5px] text-faint">7</span>
      </span>
      <span className="flex h-7 items-center gap-1.5 rounded-[6px] px-2 text-[11.5px] text-muted">
        平台 <span className="text-[9px] text-faint">▾</span>
      </span>
      <span className="mx-1 h-4 w-px bg-line" />
      {["今天", "近 7 天", "近 30 天"].map((label, i) => (
        <span key={label} className={"flex h-7 items-center rounded-[6px] px-2.5 text-[11.5px] " + (i === 1 ? "bg-raised font-medium text-ink" : "text-muted")}>
          {label}
        </span>
      ))}
      <span className="mx-1 h-4 w-px bg-line" />
      <span className="flex items-center gap-1.5 text-[11.5px] text-muted">
        <span className="h-[13px] w-[13px] rounded-[3px] border border-line-strong bg-surface" />
        仅看有弹幕
      </span>
      <div className="ml-auto flex items-center gap-0.5 rounded-[6px] border border-line bg-surface p-[3px]">
        {[
          { key: "list", label: "列表" },
          { key: "cover", label: "封面" },
        ].map((one) => (
          <span
            key={one.key}
            className={
              "flex h-6 items-center rounded-[4px] px-2 text-[11px] " +
              (mode === one.key ? "bg-raised text-ink shadow-[0_1px_2px_rgba(16,24,40,0.08)]" : "text-muted")
            }
          >
            {one.label}
          </span>
        ))}
      </div>
    </div>
  );
}

function Thumb({ item, w, h }: { item: Item; w: number; h: number }) {
  return (
    <div className="relative shrink-0 overflow-hidden rounded-[5px] bg-black" style={{ width: w, height: h }}>
      <img src={item.poster} alt="" className="h-full w-full object-cover" />
      <span className="absolute bottom-1 right-1 rounded-[3px] bg-black/70 px-1 py-[1px] font-mono text-[9.5px] tabular-nums text-white/90">
        {item.duration}
      </span>
      {item.missing ? (
        <span className="absolute inset-0 flex items-center justify-center bg-black/55 text-[10px] font-medium text-white">文件缺失</span>
      ) : null}
    </div>
  );
}

function Table({ selected }: { selected: string[] }) {
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="grid items-center gap-3 border-b border-line px-5 pb-1.5 text-[10.5px] text-faint" style={{ gridTemplateColumns: COLS }}>
        <span className="flex h-[13px] w-[13px] items-center justify-center rounded-[3px] border border-line-strong bg-surface" />
        <span>画面</span>
        <span>状态</span>
        <span>平台</span>
        <span>标题</span>
        <span>时长</span>
        <span>大小</span>
        <span>录制时间</span>
        <span className="text-right">操作</span>
      </div>
      <div className="min-h-0 flex-1 overflow-hidden">
        {ITEMS.map((item) => {
          const plat = PLAT[item.platform];
          const on = selected.includes(item.id);
          return (
            <div
              key={item.id}
              className={"grid items-center gap-3 border-b border-line/70 px-5 " + (on ? "bg-accent-soft" : "")}
              style={{ gridTemplateColumns: COLS, height: 76 }}
            >
              <span
                className={
                  "flex h-[13px] w-[13px] items-center justify-center rounded-[3px] border text-[8px] " +
                  (on ? "border-accent bg-accent text-white" : "border-line-strong bg-surface text-transparent")
                }
              >
                ✓
              </span>
              <Thumb item={item} w={120} h={68} />
              <span className="flex items-center gap-1.5 text-[11.5px]">
                <span className={"h-[6px] w-[6px] rounded-full " + (item.missing ? "bg-warn" : "bg-live")} />
                <span className={item.missing ? "text-warn" : "text-muted"}>{item.missing ? "缺失" : "完整"}</span>
              </span>
              <span className="flex items-center gap-1.5 text-[11.5px] text-ink-soft">
                <Dot color={plat.dot} />
                {plat.label}
              </span>
              <div className="min-w-0">
                <div className="truncate text-[13px] font-medium text-ink">{item.title}</div>
                <div className="mt-0.5 truncate text-[11px] text-faint">
                  {item.host}
                  {item.danmaku > 0 ? " · " + item.danmaku.toLocaleString("en-US") + " 条弹幕" : " · 无弹幕"}
                </div>
              </div>
              <span className="font-mono text-[11.5px] tabular-nums text-ink-soft">{item.duration}</span>
              <span className="font-mono text-[11.5px] tabular-nums text-ink-soft">{item.size}</span>
              <span className="font-mono text-[11px] tabular-nums text-faint">{item.when}</span>
              <div className="flex items-center justify-end gap-1">
                <Button size="sm" variant="secondary">播放</Button>
                <span className="flex h-6 w-6 items-center justify-center rounded-[5px] text-muted">⋯</span>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function SelectionBar({ count, size }: { count: number; size: string }) {
  return (
    <div className="pointer-events-none absolute inset-x-0 bottom-5 flex justify-center">
      <div className="flex items-center gap-2 rounded-full border border-line bg-surface px-3 py-1.5 shadow-[var(--shadow-pop)]">
        <span className="pl-1 text-[11.5px] text-ink">
          已选 <span className="font-mono tabular-nums">{count}</span> 条 · {size}
        </span>
        <span className="mx-1 h-4 w-px bg-line" />
        <Button size="sm" variant="secondary">合并为一条</Button>
        <Button size="sm" variant="ghost">显示文件</Button>
        <Button size="sm" variant="ghost" className="text-rec">删除</Button>
        <span className="flex h-6 w-6 items-center justify-center rounded-full text-muted">✕</span>
      </div>
    </div>
  );
}

function Drawer() {
  const item = ITEMS[0];
  const rows = [
    ["平台", PLAT[item.platform].label],
    ["主播", item.host],
    ["开播时间", "2026-11-06 15:49:22"],
    ["录制时长", item.duration],
    ["文件大小", item.size],
    ["画质", "原画 · 1080P"],
    ["线路", "线路 1 · 阿里云"],
    ["弹幕", item.danmaku.toLocaleString("en-US") + " 条"],
  ];
  return (
    <div className="absolute inset-0 z-40 flex justify-end">
      <div className="absolute inset-0 bg-black/30 backdrop-blur-[2px]" />
      <aside className="relative flex h-full w-[440px] flex-col border-l border-line bg-surface shadow-2xl">
        <div className="flex items-start justify-between gap-3 border-b border-line px-5 py-4">
          <div className="min-w-0">
            <h2 className="text-[13.5px] font-semibold text-ink">录像详情</h2>
            <p className="mt-0.5 truncate text-[11px] text-muted">{item.title}</p>
          </div>
          <span className="flex h-7 w-7 items-center justify-center rounded-[6px] text-muted">✕</span>
        </div>
        <div className="min-h-0 flex-1 overflow-hidden px-5 py-4">
          <div className="relative aspect-video w-full overflow-hidden rounded-[8px] bg-black">
            <img src={item.poster} alt="" className="h-full w-full object-cover" />
            <span className="absolute inset-0 flex items-center justify-center bg-black/25">
              <span className="flex h-11 w-11 items-center justify-center rounded-full bg-white/90 text-[13px] text-[#14171c]">▶</span>
            </span>
          </div>
          <div className="mt-3 text-[13.5px] font-medium leading-snug text-ink">{item.title}</div>
          <div className="mt-3 divide-y divide-line/70 rounded-[8px] border border-line">
            {rows.map(([label, value]) => (
              <div key={label} className="flex items-center justify-between px-3 py-2 text-[11.5px]">
                <span className="text-faint">{label}</span>
                <span className="text-ink-soft">{value}</span>
              </div>
            ))}
          </div>
          <div className="mt-3 flex items-center gap-1.5">
            <Button size="sm" variant="secondary">转 MP4</Button>
            <Button size="sm" variant="secondary">导出弹幕 ASS</Button>
            <Button size="sm" variant="ghost">打开文件夹</Button>
          </div>
        </div>
        <div className="flex items-center gap-2 border-t border-line px-5 py-3">
          <Button size="sm" variant="primary" className="flex-1">播放</Button>
          <Button size="sm" variant="ghost" className="text-rec">删除</Button>
        </div>
      </aside>
    </div>
  );
}

function Grid() {
  return (
    <div className="min-h-0 flex-1 overflow-hidden px-5 pb-4">
      <div className="grid grid-cols-4 gap-3">
        {ITEMS.slice(0, 8).map((item, index) => {
          const plat = PLAT[item.platform];
          const playing = index === 2;
          const picked = index === 0 || index === 3;
          return (
            <article
              key={item.id}
              className={"overflow-hidden rounded-[8px] border bg-surface " + (picked ? "border-accent-line" : "border-line")}
            >
              <div className="relative aspect-video w-full bg-black">
                <img src={item.poster} alt="" className={"h-full w-full object-cover " + (playing ? "opacity-45" : "")} />
                {playing ? (
                  <span className="absolute inset-0 flex items-center justify-center">
                    <span className="flex h-10 w-10 items-center justify-center rounded-full bg-white/90 text-[12px] text-[#14171c]">▶</span>
                  </span>
                ) : null}
                <span className="absolute left-1.5 top-1.5 flex items-center gap-1 rounded-[4px] bg-black/55 px-1.5 py-[2px] text-[9.5px] text-white/90">
                  <Dot color={plat.dot} />
                  {plat.label}
                </span>
                <span className="absolute bottom-1.5 right-1.5 rounded-[3px] bg-black/70 px-1 py-[1px] font-mono text-[9.5px] tabular-nums text-white/90">
                  {item.duration}
                </span>
                {picked ? (
                  <span className="absolute left-1.5 top-7 flex h-4 w-4 items-center justify-center rounded-[4px] bg-accent text-[9px] text-white">✓</span>
                ) : null}
                {playing ? (
                  <span className="absolute inset-x-0 bottom-0 h-[3px] bg-white/25">
                    <span className="block h-full w-[38%] bg-accent" />
                  </span>
                ) : null}
              </div>
              <div className="px-2.5 pb-2.5 pt-2">
                <div className="line-clamp-2 text-[12.5px] font-medium leading-snug text-ink">{item.title}</div>
                <div className="mt-1 flex items-center gap-1.5 text-[10.5px] text-faint">
                  <span className="truncate">{item.host}</span>
                  <span className="ml-auto shrink-0 font-mono tabular-nums">{item.size}</span>
                </div>
                <div className="mt-1 flex items-center justify-between text-[10.5px] text-faint">
                  <span className="font-mono tabular-nums">{item.when.slice(0, 16)}</span>
                  <span>{item.danmaku > 0 ? item.danmaku.toLocaleString("en-US") + " 弹幕" : "无弹幕"}</span>
                </div>
              </div>
            </article>
          );
        })}
      </div>
    </div>
  );
}

function Split() {
  const current = ITEMS[0];
  return (
    <div className="flex min-h-0 flex-1">
      <aside className="flex w-[336px] shrink-0 flex-col border-r border-line bg-surface">
        <div className="px-3 py-2.5">
          <div className="flex h-8 items-center gap-1.5 rounded-[6px] border border-line bg-canvas px-2.5 text-[11.5px] text-faint">
            <span className="text-[11px]">⌕</span>搜索标题或主播
          </div>
        </div>
        <div className="flex items-center gap-1.5 px-3 pb-2">
          <span className="flex h-6 items-center rounded-[5px] bg-raised px-2 text-[11px] font-medium text-ink">全部 7</span>
          {["哔哩哔哩", "斗鱼", "虎牙"].map((one) => (
            <span key={one} className="flex h-6 items-center rounded-[5px] px-2 text-[11px] text-muted">{one}</span>
          ))}
        </div>
        <div className="min-h-0 flex-1 overflow-hidden border-t border-line">
          {[
            { group: "今天", from: 0, to: 3 },
            { group: "昨天", from: 3, to: 6 },
          ].map((sec) => (
            <div key={sec.group}>
              <div className="px-3 pb-1 pt-2.5 text-[10px] font-medium uppercase tracking-[0.1em] text-faint">{sec.group}</div>
              {ITEMS.slice(sec.from, sec.to).map((item, index) => {
                const active = sec.from === 0 && index === 0;
                return (
                  <div key={item.id} className={"flex items-center gap-2.5 border-l-2 px-3 py-1.5 " + (active ? "border-accent bg-accent-soft" : "border-transparent")}>
                    <div className="relative h-[38px] w-[68px] shrink-0 overflow-hidden rounded-[4px] bg-black">
                      <img src={item.poster} alt="" className="h-full w-full object-cover" />
                    </div>
                    <div className="min-w-0 flex-1">
                      <div className={"truncate text-[12px] " + (active ? "font-medium text-ink" : "text-ink-soft")}>{item.title}</div>
                      <div className="mt-0.5 flex items-center gap-1.5 text-[10px] text-faint">
                        <Dot color={PLAT[item.platform].dot} />
                        <span>{PLAT[item.platform].label}</span>
                        <span className="font-mono tabular-nums">{item.duration}</span>
                      </div>
                    </div>
                  </div>
                );
              })}
            </div>
          ))}
        </div>
      </aside>
      <section className="flex min-w-0 flex-1 flex-col">
        <div className="relative min-h-0 flex-1 bg-black">
          <img src={current.poster} alt="" className="h-full w-full object-contain" />
          <div className="absolute inset-x-0 bottom-0 flex items-center gap-2.5 bg-gradient-to-t from-black/85 to-transparent px-4 pb-2.5 pt-8">
            <span className="flex h-7 w-7 items-center justify-center rounded-full bg-white/90 text-[10px] text-[#14171c]">▶</span>
            <span className="font-mono text-[10.5px] tabular-nums text-white/85">0:12:38</span>
            <span className="relative h-[3px] flex-1 rounded-full bg-white/20">
              <span className="absolute inset-y-0 left-0 w-[9%] rounded-full bg-accent" />
              <span className="absolute -top-[3px] left-[9%] h-[9px] w-[9px] rounded-full bg-white" />
            </span>
            <span className="font-mono text-[10.5px] tabular-nums text-white/60">{current.duration}</span>
            <span className="text-[10.5px] text-white/70">1.0×</span>
            <span className="text-[10.5px] text-white/70">弹幕开</span>
            <span className="text-[10.5px] text-white/70">全屏</span>
          </div>
        </div>
        <div className="border-t border-line bg-surface px-5 py-3">
          <div className="flex items-start gap-3">
            <div className="min-w-0 flex-1">
              <div className="truncate text-[14px] font-medium text-ink">{current.title}</div>
              <div className="mt-1 flex items-center gap-2 text-[11px] text-faint">
                <Dot color={PLAT[current.platform].dot} />
                {PLAT[current.platform].label}
                <span>·</span>
                {current.host}
                <span>·</span>
                <span className="font-mono tabular-nums">{current.when}</span>
                <span>·</span>
                <span className="font-mono tabular-nums">{current.size}</span>
                <span>·</span>
                <span>12,840 条弹幕</span>
              </div>
            </div>
            <div className="flex shrink-0 items-center gap-1.5">
              <Button size="sm" variant="secondary">转 MP4</Button>
              <Button size="sm" variant="secondary">导出 ASS</Button>
              <Button size="sm" variant="ghost">打开文件夹</Button>
              <Button size="sm" variant="ghost" className="text-rec">删除</Button>
            </div>
          </div>
        </div>
      </section>
    </div>
  );
}

export default function MediaDesign({ variant }: { variant: string }) {
  const mode = variant === "grid" ? "cover" : "list";
  return (
    <div className="flex h-screen flex-col bg-canvas text-ink">
      <Header />
      <main className="relative flex min-h-0 flex-1 flex-col">
        <ToolRow />
        <FilterRow mode={mode} />
        {variant === "grid" ? <Grid /> : null}
        {variant === "split" ? <Split /> : null}
        {variant === "table" || variant === "detail" ? <Table selected={variant === "detail" ? ["s1"] : ["s1", "s3", "s5"]} /> : null}
        {variant === "table" ? <SelectionBar count={3} size="9.8 GB" /> : null}
        {variant === "grid" ? <SelectionBar count={2} size="6.7 GB" /> : null}
        {variant === "detail" ? <Drawer /> : null}
      </main>
    </div>
  );
}