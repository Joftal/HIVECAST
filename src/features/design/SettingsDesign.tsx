import { Fragment, useState, type ReactNode } from "react";
import { Button, Segmented, Switch, cx } from "../../components/ui/Controls";

/** 设计稿专用：三个方向的设置页 mock。不接任何 IPC，数据是常量。 */

type Variant = "rail" | "tabs" | "icons";
type PageKey = "account" | "record" | "storage" | "notify" | "appearance" | "about";

const PAGES: Array<{ key: PageKey; label: string; hint: string }> = [
  { key: "account", label: "账号", hint: "登录各直播平台，录制与观看都用这里的凭据" },
  { key: "record", label: "录制与观看", hint: "默认画质与同时录制的路数" },
  { key: "storage", label: "存储", hint: "录像保存在哪、保留多久" },
  { key: "notify", label: "通知", hint: "录制与播放异常时是否提醒" },
  { key: "appearance", label: "外观与语言", hint: "主题、语言与托盘" },
  { key: "about", label: "关于", hint: "版本与构建信息" },
];

const PLATFORMS = [
  { key: "bilibili", name: "哔哩哔哩", color: "#22a7f0", state: "已登录 · 老实憨厚的笑笑", cookies: 7 },
  { key: "douyu", name: "斗鱼", color: "#ff7a1a", state: "未登录", cookies: 0 },
  { key: "huya", name: "虎牙", color: "#f5a623", state: "已登录 · yyfyyf", cookies: 5 },
  { key: "douyin", name: "抖音", color: "#ff2d6f", state: "未登录", cookies: 0 },
];

function Icon({ name, size = 15 }: { name: string; size?: number }) {
  const paths: Record<string, ReactNode> = {
    account: <><circle cx="8" cy="5.5" r="2.6" /><path d="M3 13.4c.6-2.3 2.5-3.6 5-3.6s4.4 1.3 5 3.6" /></>,
    record: <><rect x="2.4" y="3.4" width="11.2" height="9.2" rx="1.4" /><path d="M6.6 6.4v3.2l2.8-1.6z" /></>,
    storage: <><path d="M2.6 5.2h10.8M2.6 8h10.8M2.6 10.8h10.8" /><rect x="2.4" y="3" width="11.2" height="10" rx="1.6" /></>,
    notify: <><path d="M8 2.6a3.6 3.6 0 0 1 3.6 3.6v2.3l1 1.7H3.4l1-1.7V6.2A3.6 3.6 0 0 1 8 2.6Z" /><path d="M6.6 12.2a1.5 1.5 0 0 0 2.8 0" /></>,
    appearance: <><circle cx="8" cy="8" r="5.4" /><path d="M8 2.6a5.4 5.4 0 0 0 0 10.8z" fill="currentColor" stroke="none" /></>,
    about: <><circle cx="8" cy="8" r="5.4" /><path d="M8 7.2v4M8 5.2v.1" /></>,
    search: <><circle cx="7.2" cy="7.2" r="4.2" /><path d="m10.4 10.4 2.4 2.4" /></>,
    chevron: <path d="m4.5 6.5 3.5 3.5 3.5-3.5" />,
  };
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      {paths[name]}
    </svg>
  );
}

function Row({ title, desc, children }: { title: string; desc?: string; children?: ReactNode }) {
  return (
    <div className="flex items-center gap-6 px-4 py-3">
      <div className="min-w-0 flex-1">
        <div className="text-[12.5px] text-ink">{title}</div>
        {desc ? <div className="mt-0.5 text-[11px] leading-4 text-faint">{desc}</div> : null}
      </div>
      {children ? <div className="flex shrink-0 items-center gap-2">{children}</div> : null}
    </div>
  );
}

function Section({ title, children, note }: { title: string; children: ReactNode; note?: string }) {
  return (
    <section className="space-y-2">
      <h3 className="text-[10.5px] font-semibold uppercase tracking-[0.1em] text-faint">{title}</h3>
      <div className="divide-y divide-line overflow-hidden rounded-[8px] border border-line bg-surface">{children}</div>
      {note ? <p className="text-[10.5px] leading-4 text-faint">{note}</p> : null}
    </section>
  );
}

function Field({ value, width = 208 }: { value: string; width?: number }) {
  return (
    <span
      className="flex h-8 items-center justify-between gap-2 rounded-[6px] border border-line bg-surface px-2.5 text-[12px] text-ink"
      style={{ width }}
    >
      <span className="truncate">{value}</span>
      <span className="text-faint"><Icon name="chevron" size={12} /></span>
    </span>
  );
}

function AccountPage({ qr }: { qr?: boolean }) {
  return (
    <div className="space-y-5">
      <Section title="直播平台" note="Cookie 只保存在本机，用于取流与发送弹幕；不会上传。">
        {PLATFORMS.map((one) => (
          <Fragment key={one.key}>
            <div className="flex items-center gap-3 px-4 py-3">
              <span className="h-2 w-2 shrink-0 rounded-full" style={{ background: one.color }} />
              <span className="w-[64px] shrink-0 text-[12.5px] text-ink">{one.name}</span>
              <span className={cx("min-w-0 flex-1 truncate text-[11.5px]", one.cookies ? "text-muted" : "text-faint")}>{one.state}</span>
              {one.cookies ? <span className="font-mono text-[10px] tabular-nums text-faint">{one.cookies} 项</span> : null}
              <div className="flex items-center gap-1">
                {one.key === "bilibili" ? <Button size="sm" variant="ghost">扫码登录</Button> : null}
                <Button size="sm" variant="ghost">{one.cookies ? "更新 Cookie" : "粘贴 Cookie"}</Button>
                {one.cookies ? <Button size="sm" variant="ghost">退出</Button> : null}
              </div>
            </div>
            {qr && one.key === "bilibili" ? (
              <div className="flex gap-4 bg-raised/60 px-4 py-3.5">
                <div className="flex h-[132px] w-[132px] shrink-0 items-center justify-center rounded-[6px] border border-line bg-white">
                  <span className="text-[10px] text-black/40">二维码</span>
                </div>
                <div className="min-w-0 flex-1 space-y-1.5">
                  <div className="text-[12px] text-ink">扫码登录哔哩哔哩</div>
                  <p className="text-[10.5px] leading-4 text-faint">用哔哩哔哩手机客户端扫码，确认后自动写入 Cookie。</p>
                  <p className="font-mono text-[10px] tabular-nums text-warn">二维码 01:42 后过期</p>
                  <div className="flex items-center gap-2 pt-1">
                    <Button size="sm" variant="secondary">刷新二维码</Button>
                    <span className="text-[10px] text-faint">扫码后请在手机上点「确认登录」</span>
                  </div>
                </div>
              </div>
            ) : null}
          </Fragment>
        ))}
      </Section>
      <Section title="录制与弹幕权限">
        <Row title="使用账号 Cookie 发送弹幕" desc="关闭后仍会录制，但不会发送任何消息"><Switch checked={false} onChange={() => undefined} label="发送弹幕" /></Row>
      </Section>
    </div>
  );
}

function RecordPage() {
  return (
    <div className="space-y-5">
      <Section title="画质">
        <Row title="默认录制画质" desc="房间没有单独设置时使用"><Field value="原画（10000）" /></Row>
        <Row title="默认观看画质" desc="分屏观看时的起播档位"><Field value="蓝光 4M（400）" /></Row>
        <Row title="自动降级" desc="平台不提供该档位时自动选最接近的一档"><Switch checked onChange={() => undefined} label="自动降级" /></Row>
      </Section>
      <Section title="并发">
        <Row title="同时录制路数" desc="超出的房间会排队，等待前面的录制结束"><Field value="3 路" width={120} /></Row>
      </Section>
      <Section title="文件后处理">
        <Row title="录制结束自动转 MP4" desc="把分段 FLV 合并为一个 MP4，失败时保留原始分段"><Switch checked onChange={() => undefined} label="自动转 MP4" /></Row>
      </Section>
    </div>
  );
}

function StoragePage() {
  return (
    <div className="space-y-5">
      <Section title="位置">
        <Row title="媒体目录" desc="/Users/joftal/Movies/HIVECAST"><Button size="sm" variant="secondary">选择文件夹</Button></Row>
        <Row title="按平台与主播分目录" desc="哔哩哔哩/主播名/[时间][标题].mp4"><Switch checked onChange={() => undefined} label="分目录" /></Row>
      </Section>
      <Section title="清理">
        <Row title="保留天数" desc="超过天数的录像会被清理，0 表示不限制"><Field value="30 天" width={120} /></Row>
        <Row title="总量上限" desc="超过上限时从最旧的开始删"><Field value="200 GB" width={120} /></Row>
      </Section>
    </div>
  );
}

function NotifyPage() {
  return (
    <div className="space-y-5">
      <Section title="系统通知">
        <Row title="录制开始与结束" desc="在通知中心显示主播与文件名"><Switch checked onChange={() => undefined} label="录制通知" /></Row>
        <Row title="播放异常" desc="取流失败、连续重连失败时提醒"><Switch checked onChange={() => undefined} label="异常通知" /></Row>
        <Row title="登录失效" desc="Cookie 过期时提醒重新登录"><Switch checked onChange={() => undefined} label="登录失效通知" /></Row>
      </Section>
    </div>
  );
}

function AppearancePage() {
  return (
    <div className="space-y-5">
      <Section title="主题">
        <Row title="外观" desc="跟随系统时随 macOS 深浅色切换">
          <Segmented items={[{ key: "light", label: "浅色" }, { key: "dark", label: "深色" }, { key: "system", label: "跟随系统" }]} value="light" onChange={() => undefined} />
        </Row>
      </Section>
      <Section title="语言">
        <Row title="界面语言" desc="平台名不翻译">
          <Segmented items={[{ key: "system", label: "跟随系统" }, { key: "zh", label: "中文" }, { key: "en", label: "English" }]} value="system" onChange={() => undefined} />
        </Row>
      </Section>
      <Section title="托盘">
        <Row title="关闭窗口时留在托盘" desc="点菜单栏图标可重新打开"><Switch checked onChange={() => undefined} label="留在托盘" /></Row>
      </Section>
    </div>
  );
}

function AboutPage() {
  return (
    <div className="space-y-5">
      <Section title="HIVECAST">
        <Row title="版本" desc="构建于 2026-11-06 · Tauri 2 + React 19"><span className="font-mono text-[12px] tabular-nums text-muted">v0.1.0</span></Row>
        <Row title="检查更新" desc="当前已是最新版本"><Button size="sm" variant="secondary">检查</Button></Row>
      </Section>
      <Section title="诊断">
        <Row title="打开日志目录" desc="反馈问题时附上最近的日志"><Button size="sm" variant="secondary">打开</Button></Row>
      </Section>
    </div>
  );
}

function Body({ page, qr }: { page: PageKey; qr?: boolean }) {
  if (page === "account") return <AccountPage qr={qr} />;
  if (page === "record") return <RecordPage />;
  if (page === "storage") return <StoragePage />;
  if (page === "notify") return <NotifyPage />;
  if (page === "appearance") return <AppearancePage />;
  return <AboutPage />;
}

function SaveBar({ dirty }: { dirty: boolean }) {
  return (
    <div className="flex h-14 shrink-0 items-center justify-end gap-2 border-t border-line bg-surface px-6">
      <span className={cx("mr-auto text-[11px]", dirty ? "text-warn" : "text-faint")}>{dirty ? "有未保存的更改" : "所有更改已保存"}</span>
      <Button variant="secondary">关闭</Button>
      <Button variant="primary" disabled={!dirty}>保存</Button>
    </div>
  );
}

function PageHead({ page }: { page: PageKey }) {
  const meta = PAGES.find((one) => one.key === page)!;
  return (
    <div className="flex items-end justify-between gap-6">
      <div>
        <h1 className="text-[18px] font-semibold tracking-[-0.01em] text-ink">{meta.label}</h1>
        <p className="mt-1 text-[11.5px] text-muted">{meta.hint}</p>
      </div>
      <label className="flex h-8 w-[196px] items-center gap-2 rounded-[6px] border border-line bg-surface px-2.5 text-[12px] text-faint">
        <Icon name="search" size={13} />
        <span>搜索设置</span>
      </label>
    </div>
  );
}

export default function SettingsDesign({
  variant = "rail",
  page = "account",
  qr = false,
}: {
  variant?: Variant;
  page?: PageKey;
  qr?: boolean;
}) {
  const [active, setActive] = useState<PageKey>(page);

  if (variant === "tabs") {
    return (
      <div className="flex h-full flex-col bg-canvas text-ink">
        <div className="shrink-0 border-b border-line bg-surface px-8 pt-6">
          <h1 className="text-[20px] font-semibold tracking-[-0.01em]">设置</h1>
          <div className="mt-4 flex items-center gap-6">
            {PAGES.map((one) => (
              <button key={one.key} onClick={() => setActive(one.key)} className={cx("relative pb-2.5 text-[12.5px] transition-colors", active === one.key ? "text-ink" : "text-muted hover:text-ink")}>
                {one.label}
                {active === one.key ? <span className="absolute inset-x-0 -bottom-px h-[2px] rounded-full bg-accent" /> : null}
              </button>
            ))}
          </div>
        </div>
        <div className="min-h-0 flex-1 overflow-auto">
          <div className="mx-auto w-[720px] pb-10 pt-6">
            <p className="mb-5 text-[11.5px] text-muted">{PAGES.find((one) => one.key === active)!.hint}</p>
            <Body page={active} qr={qr} />
          </div>
        </div>
        <SaveBar dirty />
      </div>
    );
  }

  if (variant === "icons") {
    return (
      <div className="flex h-full bg-canvas text-ink">
        <div className="flex w-[64px] shrink-0 flex-col items-center gap-1 border-r border-line bg-surface py-4">
          <div className="mb-2 flex h-8 w-8 items-center justify-center rounded-[7px] bg-accent text-[13px] font-semibold text-accent-ink">H</div>
          {PAGES.map((one) => (
            <button key={one.key} onClick={() => setActive(one.key)} title={one.label} className={cx("flex h-9 w-9 items-center justify-center rounded-[6px] transition-colors", active === one.key ? "bg-accent-soft text-accent-text" : "text-faint hover:bg-raised hover:text-ink")}>
              <Icon name={one.key} size={16} />
            </button>
          ))}
        </div>
        <div className="flex min-w-0 flex-1 flex-col">
          <div className="min-h-0 flex-1 overflow-auto px-8 py-6">
            <div className="mx-auto w-[720px]">
              <PageHead page={active} />
              <div className="mt-5"><Body page={active} qr={qr} /></div>
            </div>
          </div>
          <SaveBar dirty={false} />
        </div>
      </div>
    );
  }

  return (
    <div className="flex h-full bg-canvas text-ink">
      <aside className="flex w-[208px] shrink-0 flex-col border-r border-line bg-surface">
        <div className="flex items-center justify-between px-4 pb-3 pt-5">
          <h1 className="text-[15px] font-semibold tracking-[-0.01em]">设置</h1>
          <button className="flex h-6 w-6 items-center justify-center rounded-[5px] text-faint hover:bg-raised hover:text-ink" aria-label="关闭">
            <svg width="11" height="11" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden><path d="M2.5 2.5 9.5 9.5M9.5 2.5 2.5 9.5" strokeLinecap="round" /></svg>
          </button>
        </div>
        <nav className="flex-1 space-y-0.5 px-2">
          {PAGES.map((one) => (
            <button key={one.key} onClick={() => setActive(one.key)} className={cx("relative flex h-8 w-full items-center gap-2.5 rounded-[6px] px-2.5 text-left text-[12.5px] transition-colors", active === one.key ? "bg-accent-soft text-accent-text" : "text-muted hover:bg-raised hover:text-ink")}>
              {active === one.key ? <span className="absolute left-0 top-1.5 h-5 w-[2px] rounded-full bg-accent" /> : null}
              <Icon name={one.key} size={14} />
              {one.label}
            </button>
          ))}
        </nav>
        <div className="border-t border-line px-4 py-3 text-[10.5px] text-faint">HIVECAST v0.1.0</div>
      </aside>
      <div className="flex min-w-0 flex-1 flex-col">
        <div className="min-h-0 flex-1 overflow-auto px-8 py-6">
          <div className="mx-auto w-[720px]">
            <PageHead page={active} />
            <div className="mt-5"><Body page={active} qr={qr} /></div>
          </div>
        </div>
        <SaveBar dirty={false} />
      </div>
    </div>
  );
}
