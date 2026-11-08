import { useState } from "react";
import { ThemeSwitch, type Theme } from "@/components/arc/theme-switch/theme-switch";
import { Switch } from "@/components/arc/switch/switch";
import { Checkbox } from "@/components/arc/checkbox/checkbox";
import { FloatingButtonGroup } from "@/components/arc/floating-button-group/floating-button-group";
import { ToastStack, useToastStack } from "@/components/arc/toast-stack/toast-stack";
import { Button } from "@/components/arc/button/button";
import { SelectMenu } from "@/components/ui/SelectMenu";
import { applyTheme } from "@/lib/theme";

/** 开发期控件预览台：一次只放当前在迭代的 Arc 组件，随用随删。
    打开方式：pnpm dev 后访问 http://localhost:1420/?arc-lab=1 */
export default function ArcLab() {
  const [theme, setTheme] = useState<Theme>(() =>
    document.documentElement.dataset.theme === "dark" ? "dark" : "light",
  );
  const [quality, setQuality] = useState("10000");
  const [platform, setPlatform] = useState("all");
  const [switchOn, setSwitchOn] = useState(true);
  const [switchOff, setSwitchOff] = useState(false);
  const [checkA, setCheckA] = useState(true);
  const [checkB, setCheckB] = useState(false);
  const [demoRecording, setDemoRecording] = useState(false);
  const { toast } = useToastStack();

  const changeTheme = (next: Theme, _variant: unknown, trigger: HTMLElement) => {
    const apply = () => {
      applyTheme(next);
      setTheme(next);
    };
    if (!document.startViewTransition || window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      apply();
      return;
    }
    const { left, top, width, height } = trigger.getBoundingClientRect();
    document.documentElement.style.setProperty("--x", `${left + width / 2}px`);
    document.documentElement.style.setProperty("--y", `${top + height / 2}px`);
    document.startViewTransition(apply);
  };

  return (
    <div className="min-h-full bg-canvas p-10 text-ink">
      <div className="mx-auto flex max-w-[820px] flex-col gap-8">
        <header className="flex flex-col gap-1">
          <h1 className="text-[18px] font-semibold">Arc UI Lab</h1>
          <p className="text-[12px] text-muted">当前主题：{theme} · 仅开发环境，?arc-lab=1 打开</p>
        </header>

        <section className="card flex flex-col gap-4 p-6">
          <h2 className="text-[13px] font-semibold">Theme switch</h2>
          <div className="flex flex-wrap items-center gap-4">
            <ThemeSwitch theme={theme} onThemeChange={changeTheme} />
            <ThemeSwitch theme={theme} onThemeChange={changeTheme} iconOnly />
            <ThemeSwitch theme={theme} onThemeChange={changeTheme} iconOnly bare />
            <ThemeSwitch theme={theme} onThemeChange={changeTheme} variant="eclipse" iconOnly />
          </div>
        </section>

        <section className="card flex flex-col gap-4 p-6">
          <h2 className="text-[13px] font-semibold">Button</h2>
          <div className="flex flex-wrap items-center gap-3">
            <Button variant="accent">开始录制</Button>
            <Button variant="primary">Arc primary</Button>
            <Button variant="secondary">次要操作</Button>
            <Button variant="ghost">Ghost</Button>
            <Button variant="danger">删除</Button>
            <Button variant="recording">录制中</Button>
            <Button variant="accent" loading>
              处理中
            </Button>
          </div>
          <div className="flex flex-wrap items-end gap-3">
            <Button variant="accent" size="sm">
              Small
            </Button>
            <Button variant="accent" size="md">
              Medium
            </Button>
            <Button variant="accent" size="lg">
              Large
            </Button>
          </div>
        </section>

        <section className="card flex flex-col gap-4 p-6">
          <h2 className="text-[13px] font-semibold">Toast stack</h2>
          <div className="flex flex-wrap items-center gap-2">
            <Button variant="secondary" onClick={() => toast({ type: "info", title: "直播间设置即将上线" })}>
              Info
            </Button>
            <Button variant="secondary" onClick={() => toast({ type: "success", title: "已保存", description: "设置已写入本地数据库。" })}>
              Success
            </Button>
            <Button variant="secondary" onClick={() => toast({ type: "warning", title: "多路声音同时播放", description: "需要安静时点「全部静音」。" })}>
              Warning
            </Button>
            <Button variant="secondary" onClick={() => toast({ type: "error", title: "操作失败", description: "无法连接到直播间，请稍后重试。" })}>
              Error
            </Button>
            <Button
              variant="secondary"
              onClick={() => {
                const id = toast({ type: "loading", title: "正在处理", description: "转封装进行中…" });
                window.setTimeout(() => toast({ type: "success", id, title: "处理完成", description: "已生成 MP4。" }), 1400);
              }}
            >
              Loading → success
            </Button>
          </div>
          <ToastStack position="top-center" label="通知" />
        </section>

        <section className="card flex flex-col gap-4 p-6">
          <h2 className="text-[13px] font-semibold">Floating button group</h2>
          <div className="flex items-center justify-between gap-4 rounded-[10px] border border-line bg-surface px-4 py-3">
            <span className="min-w-0 text-[12.5px] text-ink">示例直播间</span>
            <FloatingButtonGroup
              label="行内操作预览"
              variant="muted"
              size="sm"
              items={[
                { id: "watch", label: "观看", tone: "accent", onSelect: () => undefined },
                {
                  id: "record",
                  label: demoRecording ? "停止" : "录制",
                  reserveLabels: ["录制", "停止"],
                  tone: demoRecording ? "danger" : "default",
                  onSelect: () => setDemoRecording((value) => !value),
                },
              ]}
            />
          </div>
          <div className="flex flex-wrap items-center gap-4">
            <FloatingButtonGroup
              label="浮动操作预览"
              variant="floating"
              size="md"
              items={[
                { id: "undo", label: "撤销", onSelect: () => undefined },
                { id: "redo", label: "重做", onSelect: () => undefined },
                { type: "separator" },
                { id: "share", label: "分享", onSelect: () => undefined },
              ]}
            />
          </div>
        </section>

        <section className="card flex flex-col gap-4 p-6">
          <h2 className="text-[13px] font-semibold">Checkbox</h2>
          <div className="flex flex-wrap items-center gap-6">
            <Checkbox checked={checkA} onCheckedChange={(next) => setCheckA(next === true)} aria-label="已勾选" />
            <Checkbox checked={checkB} onCheckedChange={(next) => setCheckB(next === true)} aria-label="未勾选" />
            <Checkbox checked="indeterminate" onCheckedChange={() => undefined} aria-label="部分选中" />
            <Checkbox checked={false} onCheckedChange={() => undefined} disabled aria-label="禁用" />
            <Checkbox
              label="带标签"
              description="说明文字与复选框第一行对齐"
              checked={checkA}
              onCheckedChange={(next) => setCheckA(next === true)}
            />
          </div>
          <div className="flex flex-wrap items-center gap-4 rounded-[8px] bg-black/60 p-4">
            <Checkbox tone="overlay" checked onCheckedChange={() => undefined} aria-label="封面已选中" />
            <Checkbox tone="overlay" checked={false} onCheckedChange={() => undefined} aria-label="封面未选中" />
          </div>
        </section>

        <section className="card flex flex-col gap-4 p-6">
          <h2 className="text-[13px] font-semibold">Switch</h2>
          <div className="flex flex-wrap items-center gap-6">
            <Switch checked={switchOn} onCheckedChange={setSwitchOn} aria-label="已开启" />
            <Switch checked={switchOff} onCheckedChange={setSwitchOff} aria-label="已关闭" />
            <Switch checked onCheckedChange={() => undefined} aria-label="只读开启" />
            <Switch checked={false} onCheckedChange={() => undefined} disabled aria-label="禁用" />
            <Switch label="带可见文字" checked={switchOn} onCheckedChange={setSwitchOn} />
          </div>
        </section>

        <section className="card flex flex-col gap-3 p-6">
          <h2 className="text-[13px] font-semibold">Select menu</h2>
          <div className="flex flex-wrap items-center gap-3">
            <SelectMenu
              size="md"
              value={quality}
              onChange={setQuality}
              options={[
                { value: "0", label: "自动" },
                { value: "40000", label: "最高" },
                { value: "10000", label: "原画" },
                { value: "400", label: "蓝光 1080P 60帧 高码率（大会员）" },
              ]}
              className="w-[208px] justify-between"
            />
            <SelectMenu
              variant="bare"
              value={platform}
              onChange={setPlatform}
              placeholder="平台"
              active={platform !== "all"}
              options={[
                { value: "all", label: "全部", hint: "12" },
                { value: "bilibili", label: "哔哩哔哩", hint: "7" },
                { value: "douyu", label: "斗鱼", hint: "5" },
              ]}
            />
            <span className="rounded-ctl bg-black px-3 py-2">
              <SelectMenu
                variant="dark"
                value="1080"
                onChange={() => undefined}
                options={[
                  { value: "1080", label: "原画" },
                  { value: "720", label: "高清" },
                ]}
              />
            </span>
          </div>
          <div className="flex w-[360px] items-center gap-2">
            <span className="w-[62px] shrink-0 text-[12.5px] text-muted">线路</span>
            <SelectMenu
              size="md"
              value="line"
              onChange={() => undefined}
              options={[
                {
                  value: "line",
                  label: "d1--cn-gotcha209.bilivideo.com · 超清 · HLS/fMP4 · 60帧",
                },
              ]}
              className="min-w-0 flex-1 justify-between"
            />
          </div>
        </section>

        <section className="card flex flex-col gap-3 p-6">
          <h2 className="text-[13px] font-semibold">Token bridge</h2>
          <div className="flex flex-wrap gap-2 text-[11px]">
            {[
              ["bg-surface", "bg-surface"],
              ["bg-raised", "bg-raised"],
              ["bg-accent", "bg-accent"],
              ["text-ink", "text-ink"],
              ["text-muted", "text-muted"],
              ["border-line", "border-line"],
            ].map(([label, cls]) => (
              <span key={label} className={`rounded-ctl border border-line px-2 py-1 ${cls}`}>
                {label}
              </span>
            ))}
          </div>
        </section>
      </div>
    </div>
  );
}
