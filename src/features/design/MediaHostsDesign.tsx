import { useState } from "react";

type Variant = "grid" | "detail" | "drawer" | "split";

const PLAT: Record<string, string> = {
  bilibili: "哔哩哔哩",
  douyu: "斗鱼",
  huya: "虎牙",
  douyin: "抖音",
};

type Host = {
  id: string;
  platform: string;
  name: string;
  room: string;
  avatar: string;
  cover: string;
  count: number;
  size: string;
  latest: string;
};

const HOSTS: Host[] = [
  { id: "h1", platform: "bilibili", name: "老实憨厚的笑笑", room: "德云色直播间", avatar: "/__design/p1.jpg", cover: "/__design/p1.jpg", count: 128, size: "86.4 GB", latest: "11-06 14:12" },
  { id: "h2", platform: "douyu", name: "yyfyyf", room: "陪伴每一天！", avatar: "/__design/p2.jpg", cover: "/__design/p2.jpg", count: 64, size: "41.2 GB", latest: "11-06 13:02" },
  { id: "h3", platform: "huya", name: "虎牙英雄联盟赛事", room: "S16 瑞士轮第五日", avatar: "/__design/p3.jpg", cover: "/__design/p3.jpg", count: 39, size: "22.8 GB", latest: "11-05 23:41" },
  { id: "h4", platform: "bilibili", name: "哔哩哔哩英雄联盟赛事", room: "2026 全球总决赛", avatar: "/__design/p4.jpg", cover: "/__design/p4.jpg", count: 52, size: "31.7 GB", latest: "11-05 21:08" },
  { id: "h5", platform: "douyin", name: "哎哟喂", room: "哎哟喂正在直播", avatar: "/__design/p5.jpg", cover: "/__design/p5.jpg", count: 17, size: "9.3 GB", latest: "11-05 18:20" },
  { id: "h6", platform: "douyu", name: "呆妹儿小霸王", room: "呆妹儿直播间", avatar: "/__design/p6.jpg", cover: "/__design/p6.jpg", count: 24, size: "14.6 GB", latest: "11-04 22:55" },
  { id: "h7", platform: "douyu", name: "旭旭宝宝", room: "旭旭宝宝直播间", avatar: "/__design/p7.jpg", cover: "/__design/p7.jpg", count: 8, size: "5.1 GB", latest: "11-04 20:30" },
  { id: "h8", platform: "bilibili", name: "影视飓风", room: "飓风小课堂", avatar: "/__design/p3.jpg", cover: "/__design/p3.jpg", count: 6, size: "3.4 GB", latest: "11-03 19:12" },
];

const FILES = [
  { id: "f1", title: "【预告】7日6点 T1 vs KC", time: "11-06 11:55", dur: "2:30", size: "11.3 MB", dm: 120, cover: "/__design/p1.jpg" },
  { id: "f2", title: "小组赛 Day3 上半场", time: "11-05 20:10", dur: "1:54:22", size: "1.9 GB", dm: 8421, cover: "/__design/p2.jpg" },
  { id: "f3", title: "赛后复盘与采访", time: "11-05 18:02", dur: "24:07", size: "412 MB", dm: 0, cover: "/__design/p3.jpg" },
  { id: "f4", title: "瑞士轮第五日 完整场", time: "11-04 23:31", dur: "3:12:40", size: "2.8 GB", dm: 12043, cover: "/__design/p4.jpg" },
  { id: "f5", title: "开幕式与选手介绍", time: "11-04 19:00", dur: "18:55", size: "301 MB", dm: 2103, cover: "/__design/p5.jpg" },
  { id: "f6", title: "加赛 BO1", time: "11-03 22:14", dur: "48:12", size: "806 MB", dm: 532, cover: "/__design/p6.jpg" },
];

function cx(...parts: Array<string | false | null | undefined>) {
  return parts.filter(Boolean).join(" ");
}

function Tab({ label, active }: { label: string; active?: boolean }) {
  return (
    <span
      className={cx(
        "relative flex h-12 items-center px-3 text-[12.5px] transition-colors",
        active ? "text-ink" : "text-muted hover:text-ink",
      )}
    >
      {label}
      {active && <span className="absolute inset-x-2 bottom-0 h-[2px] rounded-full bg-accent" />}
    </span>
  );
}

function AppBar() {
  return (
    <header className="flex h-12 shrink-0 items-center gap-2 border-b border-line bg-surface pr-3" style={{ paddingLeft: 78 }}>
      <div className="mr-4 flex items-center gap-2">
        <div className="grid h-6 w-6 place-items-center rounded-[6px] bg-accent text-[11px] font-bold text-accent-ink">H</div>
        <span className="text-[12.5px] font-semibold tracking-wide">HIVECAST</span>
      </div>
      <Tab label="监控" />
      <Tab label="观看" />
      <Tab label="媒体库" active />
      <div className="ml-auto flex items-center gap-3 text-[11.5px] text-muted">
        <span>中</span>
        <span>☾</span>
        <span>⚙</span>
      </div>
    </header>
  );
}

function PlatformChip({ platform, dark }: { platform: string; dark?: boolean }) {
  return (
    <span
      className={cx(
        "rounded-[4px] px-1.5 py-0.5 text-[10px] leading-none",
        dark ? "bg-black/50 text-white/85 backdrop-blur-sm" : "bg-raised text-muted",
      )}
    >
      {PLAT[platform]}
    </span>
  );
}

function HostCard({ host, selected }: { host: Host; selected?: boolean }) {
  return (
    <div
      className={cx(
        "group cursor-pointer overflow-hidden rounded-[8px] border bg-surface transition",
        selected ? "border-accent/60 shadow-card" : "border-line hover:border-line hover:shadow-card",
      )}
    >
      <div className="relative overflow-hidden bg-raised" style={{ aspectRatio: "16 / 10" }}>
        <img src={host.cover} alt="" className="h-full w-full object-cover transition duration-500 group-hover:scale-[1.04]" />
        <div className="absolute inset-0 bg-gradient-to-t from-black/75 via-black/10 to-transparent" />
        <div className="absolute left-2 top-2">
          <PlatformChip platform={host.platform} dark />
        </div>
        <div className="absolute inset-x-2.5 bottom-2 flex items-center gap-2">
          <img src={host.avatar} alt="" className="h-9 w-9 rounded-full border-2 border-white/80 object-cover" />
          <div className="min-w-0">
            <div className="truncate text-[13px] font-medium text-white drop-shadow">{host.name}</div>
            <div className="truncate text-[10px] text-white/60">{host.room}</div>
          </div>
        </div>
      </div>
      <div className="flex items-center gap-2 px-2.5 py-2 text-[10.5px] text-muted">
        <span className="font-mono tabular-nums">{host.count} 个录像</span>
        <span className="text-line">·</span>
        <span className="font-mono tabular-nums">{host.size}</span>
        <span className="ml-auto shrink-0 text-faint">最新 {host.latest}</span>
      </div>
    </div>
  );
}

function Toolbar({ compact, placeholder }: { compact?: boolean; placeholder?: string }) {
  return (
    <div className="flex items-center gap-2">
      <div className={cx("flex items-center gap-1.5 rounded-[6px] border border-line bg-surface px-2", compact ? "h-8 w-48" : "h-8 w-56")}>
        <span className="text-[11px] text-faint">⌕</span>
        <span className="text-[11.5px] text-faint">{placeholder ?? "搜索主播或房间"}</span>
      </div>
      <div className="flex h-8 items-center gap-1 rounded-[6px] border border-line bg-surface px-2 text-[11.5px] text-muted">
        平台 <span className="text-faint">▾</span>
      </div>
      <div className="flex h-8 items-center gap-1 rounded-[6px] border border-line bg-surface px-2 text-[11.5px] text-muted">
        按最新录制 <span className="text-faint">▾</span>
      </div>
      <div className="ml-auto flex items-center gap-1 rounded-[6px] border border-line bg-surface p-[3px]">
        <span className="grid h-6 w-6 place-items-center rounded-[4px] bg-raised text-[11px] text-ink">▦</span>
        <span className="grid h-6 w-6 place-items-center rounded-[4px] text-[11px] text-faint">☰</span>
      </div>
    </div>
  );
}

function HostWall({ selectedId }: { selectedId?: string }) {
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="px-5 pb-3 pt-4">
        <Toolbar />
      </div>
      <div className="min-h-0 flex-1 overflow-auto px-5 pb-6">
        <div className="grid grid-cols-2 items-start gap-3 xl:grid-cols-3 2xl:grid-cols-4">
          {HOSTS.map((host) => (
            <HostCard key={host.id} host={host} selected={host.id === selectedId} />
          ))}
        </div>
      </div>
    </div>
  );
}

function HostHeader({ host, back }: { host: Host; back?: boolean }) {
  return (
    <div className="px-5 pb-3.5 pt-4">
      {back && (
        <button className="mb-2.5 inline-flex items-center gap-1 text-[11.5px] text-muted hover:text-ink">
          <span className="text-[13px]">←</span> 全部主播
        </button>
      )}
      <div className="flex items-center gap-3">
        <img src={host.avatar} alt="" className="h-16 w-16 rounded-[10px] border border-line object-cover" />
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span className="truncate text-[18px] font-semibold text-ink">{host.name}</span>
            <PlatformChip platform={host.platform} />
          </div>
          <div className="mt-1 flex items-center gap-2 text-[11px] text-muted">
            <span className="font-mono tabular-nums">{host.count} 个录像</span>
            <span className="text-line">·</span>
            <span className="font-mono tabular-nums">{host.size}</span>
            <span className="text-line">·</span>
            <span>最新 {host.latest}</span>
            <span className="text-line">·</span>
            <span className="truncate">{host.room}</span>
          </div>
        </div>
        <div className="ml-auto flex shrink-0 items-center gap-2">
          <button className="h-7 rounded-[5px] border border-line px-2.5 text-[11.5px] text-muted hover:text-ink">打开文件夹</button>
          <button className="h-7 rounded-[5px] border border-line px-2.5 text-[11.5px] text-muted hover:text-ink">全选</button>
          <button className="h-7 rounded-[5px] border border-line px-2.5 text-[11.5px] text-rec">删除</button>
        </div>
      </div>
    </div>
  );
}

const POOL = ["/__design/p1.jpg", "/__design/p2.jpg", "/__design/p3.jpg", "/__design/p4.jpg", "/__design/p5.jpg", "/__design/p6.jpg", "/__design/p7.jpg"];

function NineGrid({ src }: { src: string }) {
  const start = Math.max(0, POOL.indexOf(src));
  return (
    <div className="flex h-full w-full flex-col gap-[2px] bg-[#101010]">
      {[0, 1, 2].map((row) => (
        <div key={row} className="flex min-h-0 flex-1 gap-[2px]">
          {[0, 1, 2].map((col) => (
            <div key={col} className="relative min-w-0 flex-1 overflow-hidden">
              <img
                src={POOL[(start + row * 3 + col) % POOL.length]}
                alt=""
                className="h-full w-full object-cover"
              />
            </div>
          ))}
        </div>
      ))}
    </div>
  );
}

function FileGrid() {
  return (
    <div className="min-h-0 flex-1 overflow-auto px-5 pb-6">
      <div className="grid grid-cols-2 items-start gap-3 xl:grid-cols-4 2xl:grid-cols-5">
        {FILES.map((file) => (
          <div key={file.id} className="group overflow-hidden rounded-[8px] border border-line bg-surface transition hover:shadow-card">
            <div className="relative overflow-hidden bg-raised" style={{ aspectRatio: "16 / 9" }}>
              <NineGrid src={file.cover} />
              <span className="absolute bottom-1.5 left-1.5 rounded-[3px] bg-black/70 px-1.5 py-0.5 font-mono text-[10px] leading-none text-white">{file.dur}</span>
              {file.dm > 0 && (
                <span className="absolute bottom-1.5 right-1.5 rounded-[3px] bg-black/70 px-1.5 py-0.5 font-mono text-[10px] leading-none text-white/90">{file.dm} 条弹幕</span>
              )}
              <div className="absolute inset-0 grid place-items-center opacity-0 transition group-hover:opacity-100">
                <span className="grid h-9 w-9 place-items-center rounded-full bg-white/90 text-[13px] text-black">▶</span>
              </div>
            </div>
            <div className="px-2.5 py-2">
              <div className="truncate text-[12px] text-ink">{file.title}</div>
              <div className="mt-1 flex items-center gap-2 text-[10px] text-faint">
                <span className="font-mono tabular-nums">{file.time}</span>
                <span className="ml-auto font-mono tabular-nums">{file.size}</span>
              </div>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

function Drawer({ host }: { host: Host }) {
  return (
    <div className="absolute inset-0 z-40 flex justify-end">
      <div className="absolute inset-0 bg-black/30 backdrop-blur-[1px]" />
      <aside className="relative flex h-full w-[420px] flex-col border-l border-line bg-surface shadow-2xl">
        <div className="flex items-start gap-3 border-b border-line px-4 py-3">
          <img src={host.avatar} alt="" className="h-11 w-11 rounded-[9px] border border-line object-cover" />
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <span className="truncate text-[13.5px] font-semibold text-ink">{host.name}</span>
              <PlatformChip platform={host.platform} />
            </div>
            <div className="mt-1 flex items-center gap-2 text-[10.5px] text-muted">
              <span className="font-mono tabular-nums">{host.count} 个录像</span>
              <span className="text-line">·</span>
              <span className="font-mono tabular-nums">{host.size}</span>
            </div>
          </div>
          <span className="text-[13px] text-faint">✕</span>
        </div>
        <div className="flex items-center gap-2 border-b border-line px-4 py-2 text-[11px] text-muted">
          <span className="rounded-[4px] bg-raised px-1.5 py-0.5">全部</span>
          <span className="px-1.5 py-0.5">近 7 天</span>
          <span className="ml-auto text-faint">按时间 ▾</span>
        </div>
        <div className="min-h-0 flex-1 overflow-auto">
          {FILES.map((file) => (
            <div key={file.id} className="flex gap-2.5 border-b border-line/70 px-4 py-2.5 hover:bg-raised/60">
              <div className="relative h-[46px] w-[82px] shrink-0 overflow-hidden rounded-[4px] bg-raised">
                <img src={file.cover} alt="" className="h-full w-full object-cover" />
                <span className="absolute bottom-0.5 right-0.5 rounded-[2px] bg-black/65 px-1 font-mono text-[9px] text-white/85">{file.dur}</span>
              </div>
              <div className="min-w-0 flex-1">
                <div className="truncate text-[11.5px] text-ink">{file.title}</div>
                <div className="mt-1 flex items-center gap-2 text-[10px] text-faint">
                  <span className="font-mono tabular-nums">{file.time}</span>
                  <span className="font-mono tabular-nums">{file.size}</span>
                  {file.dm > 0 && <span>{file.dm} 弹幕</span>}
                </div>
              </div>
            </div>
          ))}
        </div>
      </aside>
    </div>
  );
}

function SplitView() {
  const [active] = useState("h1");
  const host = HOSTS.find((one) => one.id === active) ?? HOSTS[0];
  return (
    <div className="flex min-h-0 flex-1">
      <aside className="w-[264px] shrink-0 overflow-auto border-r border-line bg-surface py-2">
        {HOSTS.map((one) => (
          <button
            key={one.id}
            className={cx(
              "flex w-full items-center gap-2 px-3 py-2 text-left transition",
              one.id === active ? "bg-accent-soft" : "hover:bg-raised",
            )}
          >
            <img src={one.avatar} alt="" className="h-7 w-7 rounded-full object-cover" />
            <div className="min-w-0 flex-1">
              <div className="truncate text-[11.5px] text-ink">{one.name}</div>
              <div className="truncate text-[10px] text-faint">{PLAT[one.platform]} · {one.count} 个</div>
            </div>
            <span className="font-mono text-[9.5px] text-faint">{one.size}</span>
          </button>
        ))}
      </aside>
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        <HostHeader host={host} />
        <div className="px-5 pb-3">
          <Toolbar compact placeholder="搜索这个主播的录像" />
        </div>
        <FileGrid />
      </div>
    </div>
  );
}

export default function MediaHostsDesign({ variant = "grid" }: { variant?: Variant }) {
  const host = HOSTS[0];
  return (
    <div className="flex h-screen w-screen flex-col overflow-hidden bg-canvas text-ink">
      <AppBar />
      {variant === "grid" && <HostWall />}
      {variant === "detail" && (
        <div className="flex min-h-0 flex-1 flex-col">
          <HostHeader host={host} back />
          <div className="px-5 pb-3">
            <Toolbar placeholder="搜索这个主播的录像" />
          </div>
          <FileGrid />
        </div>
      )}
      {variant === "drawer" && (
        <div className="relative flex min-h-0 flex-1 flex-col">
          <HostWall selectedId="h1" />
          <Drawer host={host} />
        </div>
      )}
      {variant === "split" && <SplitView />}
    </div>
  );
}
