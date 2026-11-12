import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Dialog, DialogContent } from "../../components/arc/dialog/dialog";
import { Checkbox } from "../../components/arc/checkbox/checkbox";
import { Button, Chip, Switch, cx } from "../../components/ui/Controls";
import { PageChip } from "../../components/ui/PageShell";
import { useNotify } from "../../lib/notify";
import {
  bilibiliFollowings,
  bilibiliFollowStatuses,
  roomsImport,
  type FollowingUser,
  type ImportRoomInput,
  type ImportRoomsResult,
  type LiveStatus,
  type RoomView,
} from "../../lib/ipc";

type Props = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 当前监控列表，用来标记「已添加」。 */
  rooms: RoomView[];
  /** 导入完成（含部分失败）后刷新监控列表。 */
  onImported?: () => void | Promise<void>;
};

type Filter = "all" | "live" | "room" | "new" | "existing";
type Phase = "follows" | "status" | "ready" | "followError" | "statusError";

const FILTERS: Filter[] = ["all", "live", "room", "new", "existing"];

function wait(ms: number) {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}

function FollowAvatar({ user }: { user: FollowingUser }) {
  const [failed, setFailed] = useState(false);
  const initial = (user.uname || "?").slice(0, 1).toUpperCase();
  if (!user.avatarUrl || failed) {
    return (
      <span className="grid h-8 w-8 shrink-0 place-items-center rounded-full border border-line bg-raised text-[11px] font-medium text-muted">
        {initial}
      </span>
    );
  }
  return (
    <img
      src={user.avatarUrl}
      alt=""
      loading="lazy"
      referrerPolicy="no-referrer"
      onError={() => setFailed(true)}
      className="h-8 w-8 shrink-0 rounded-full border border-line object-cover"
    />
  );
}

/**
 * B 站关注导入。
 *
 * 关注列表只在打开弹窗时拉取（每页 50 条，页间留 180ms），随后用一次批量状态接口
 * 补齐直播间、在播与封面信息。导入本身只写本地库，不逐房间请求 B 站；已存在的房间
 * 只刷新展示字段，不覆盖画质、线路、备注和标签。
 */
export default function ImportFollowsDialog({ open, onOpenChange, rooms, onImported }: Props) {
  const { t } = useTranslation();
  const notify = useNotify();

  const [phase, setPhase] = useState<Phase>("follows");
  const [users, setUsers] = useState<FollowingUser[]>([]);
  const [statuses, setStatuses] = useState<Map<string, LiveStatus>>(new Map());
  const [total, setTotal] = useState(0);
  const [pageProgress, setPageProgress] = useState({ done: 0, total: 1 });
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [filter, setFilter] = useState<Filter>("all");
  const [query, setQuery] = useState("");
  const [monitor, setMonitor] = useState(true);
  const [autoRecord, setAutoRecord] = useState(false);
  const [importing, setImporting] = useState(false);
  const [result, setResult] = useState<ImportRoomsResult | null>(null);
  const [reload, setReload] = useState(0);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setPhase("follows");
    setUsers([]);
    setStatuses(new Map());
    setTotal(0);
    setPageProgress({ done: 0, total: 1 });
    setSelected(new Set());
    setFilter("all");
    setQuery("");
    setMonitor(true);
    setAutoRecord(false);
    setImporting(false);
    setResult(null);

    void (async () => {
      let all: FollowingUser[] = [];
      try {
        const first = await bilibiliFollowings(1);
        if (cancelled) return;
        all = [...first.items];
        const count = first.total || all.length;
        const pageSize = first.pageSize || 50;
        const pages = Math.max(1, Math.ceil(count / pageSize));
        setTotal(count);
        setUsers(all);
        setPageProgress({ done: 1, total: pages });
        for (let page = 2; page <= pages; page += 1) {
          await wait(180);
          if (cancelled) return;
          const next = await bilibiliFollowings(page);
          if (cancelled) return;
          all = [...all, ...next.items];
          setUsers(all);
          setPageProgress({ done: page, total: pages });
        }
      } catch {
        if (!cancelled) setPhase("followError");
        return;
      }
      if (cancelled) return;
      setPhase("status");
      try {
        const live = await bilibiliFollowStatuses(all.map((user) => user.uid));
        if (cancelled) return;
        setStatuses(new Map(live.map((item) => [item.uid, item])));
        setPhase("ready");
      } catch {
        if (!cancelled) setPhase("statusError");
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [open, reload]);

  const existing = useMemo(() => {
    const keys = new Set<string>();
    for (const view of rooms) {
      if (view.room.platform !== "bilibili") continue;
      keys.add(view.room.roomKey);
      if (view.room.platformUid) keys.add(view.room.platformUid);
    }
    return keys;
  }, [rooms]);

  const isExisting = (user: FollowingUser, status?: LiveStatus) =>
    existing.has(user.uid) ||
    (!!status?.roomId && existing.has(status.roomId)) ||
    (!!status?.shortId && existing.has(status.shortId));
  const canImport = (user: FollowingUser, status?: LiveStatus) => !!status?.roomId && !isExisting(user, status);

  const filtered = useMemo(() => {
    const text = query.trim().toLowerCase();
    return users.filter((user) => {
      const status = statuses.get(user.uid);
      if (text && !user.uname.toLowerCase().includes(text) && !(status?.title ?? "").toLowerCase().includes(text)) {
        return false;
      }
      switch (filter) {
        case "live":
          return !!status?.isLive;
        case "room":
          return !!status?.roomId;
        case "new":
          return canImport(user, status);
        case "existing":
          return isExisting(user, status);
        default:
          return true;
      }
    });
  }, [users, statuses, query, filter, existing]);

  const counts = useMemo(() => {
    let live = 0;
    let room = 0;
    let added = 0;
    let fresh = 0;
    for (const user of users) {
      const status = statuses.get(user.uid);
      if (status?.isLive) live += 1;
      if (status?.roomId) room += 1;
      if (isExisting(user, status)) added += 1;
      if (canImport(user, status)) fresh += 1;
    }
    return { all: users.length, live, room, existing: added, new: fresh };
  }, [users, statuses, existing]);

  const visibleSelectable = filtered.filter((user) => canImport(user, statuses.get(user.uid)));
  const allVisibleSelected =
    visibleSelectable.length > 0 && visibleSelectable.every((user) => selected.has(user.uid));
  const someVisibleSelected = visibleSelectable.some((user) => selected.has(user.uid));

  function toggle(user: FollowingUser) {
    if (!canImport(user, statuses.get(user.uid))) return;
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(user.uid)) next.delete(user.uid);
      else next.add(user.uid);
      return next;
    });
  }

  function toggleVisible() {
    setSelected((current) => {
      const next = new Set(current);
      if (allVisibleSelected) {
        for (const user of visibleSelectable) next.delete(user.uid);
      } else {
        for (const user of visibleSelectable) next.add(user.uid);
      }
      return next;
    });
  }

  async function retryStatuses() {
    if (phase === "status") return;
    setPhase("status");
    try {
      const live = await bilibiliFollowStatuses(users.map((user) => user.uid));
      setStatuses(new Map(live.map((item) => [item.uid, item])));
      setPhase("ready");
    } catch (err) {
      setPhase("statusError");
      notify.error(err);
    }
  }

  async function submit() {
    if (importing) return;
    const chosen = users
      .filter((user) => selected.has(user.uid))
      .map((user) => ({ user, status: statuses.get(user.uid) }))
      .filter((item): item is { user: FollowingUser; status: LiveStatus } =>
        !!item.status?.roomId && !isExisting(item.user, item.status),
      );
    if (chosen.length === 0) {
      notify.info(t("monitor.importFollow.nothingSelected"));
      return;
    }
    const items: ImportRoomInput[] = chosen.map(({ user, status }) => ({
      uid: user.uid,
      roomId: status.roomId as string,
      shortId: status.shortId,
      uname: status.uname || user.uname,
      title: status.title,
      avatarUrl: status.avatarUrl ?? user.avatarUrl,
      coverUrl: status.coverUrl,
      areaName: status.areaName,
      isLive: status.isLive,
      startedAt: status.startedAt,
      online: status.online,
    }));
    setImporting(true);
    try {
      const imported = await roomsImport(items, monitor, autoRecord);
      setResult(imported);
      setSelected(new Set());
      await onImported?.();
      if (imported.failed.length > 0) {
        notify.warning(imported.failed[0].message, t("monitor.importFollow.failedTitle"));
      } else {
        notify.success(
          t("monitor.importFollow.done"),
          t("monitor.importFollow.doneBody", { added: imported.added, existing: imported.existing }),
        );
      }
    } catch (err) {
      notify.error(err);
    } finally {
      setImporting(false);
    }
  }

  const filterLabel: Record<Filter, string> = {
    all: t("monitor.importFollow.filterAll"),
    live: t("monitor.importFollow.filterLive"),
    room: t("monitor.importFollow.filterRoom"),
    new: t("monitor.importFollow.filterNew"),
    existing: t("monitor.importFollow.filterExisting"),
  };
  const statusHint =
    phase === "follows"
      ? t("monitor.importFollow.loading", { done: pageProgress.done, total: pageProgress.total })
      : phase === "status"
        ? t("monitor.importFollow.statusLoading", { n: users.length })
        : t("monitor.importFollow.summary", { total, room: counts.room, live: counts.live });

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        title={t("monitor.importFollow.title")}
        description={t("monitor.importFollow.description")}
        size="md"
      >
        <div className="flex h-[min(560px,calc(100dvh-200px))] min-h-[360px] flex-col">
          <div className="flex items-start gap-3 rounded-[8px] border border-line bg-raised/40 px-3 py-2.5">
            <div className="min-w-0 flex-1">
              <div className="text-[12.5px] text-ink">{statusHint}</div>
              <div className="mt-1.5 h-1 overflow-hidden rounded-full bg-line">
                <div
                  className="h-full rounded-full bg-accent transition-[width] duration-300"
                  style={{
                    width: phase === "follows"
                      ? pageProgress.total > 0
                        ? Math.max(4, (pageProgress.done / pageProgress.total) * 100) + "%"
                        : "4%"
                      : phase === "status"
                        ? "100%"
                        : "100%",
                  }}
                />
              </div>
            </div>
            {phase === "followError" || phase === "statusError" ? (
              <Button
                size="sm"
                variant="secondary"
                onClick={() => {
                  if (phase === "followError") setReload((value) => value + 1);
                  else void retryStatuses();
                }}
              >
                {t("monitor.importFollow.retry")}
              </Button>
            ) : null}
          </div>

          {phase === "followError" ? (
            <div className="mt-3 rounded-[8px] border border-warn/30 bg-warn/10 px-3 py-2 text-[12px] leading-5 text-warn">
              {t("monitor.importFollow.followFailed")}
            </div>
          ) : null}
          {phase === "statusError" ? (
            <div className="mt-3 rounded-[8px] border border-warn/30 bg-warn/10 px-3 py-2 text-[12px] leading-5 text-warn">
              {t("monitor.importFollow.statusFailed")}
            </div>
          ) : null}
          {result ? (
            <div
              className={cx(
                "mt-3 rounded-[8px] border px-3 py-2 text-[12px] leading-5",
                result.failed.length > 0
                  ? "border-warn/30 bg-warn/10 text-warn"
                  : "border-live/30 bg-live/10 text-live",
              )}
            >
              {t("monitor.importFollow.doneBody", { added: result.added, existing: result.existing })}
              {result.failed.length > 0
                ? " · " + t("monitor.importFollow.failedCount", { n: result.failed.length })
                : ""}
            </div>
          ) : null}

          <div className="mt-3 flex flex-wrap items-center gap-1.5">
            {FILTERS.map((key) => (
              <PageChip key={key} active={filter === key} onClick={() => setFilter(key)}>
                {filterLabel[key]}
                <span className="tnum ml-1.5 text-[11.5px] opacity-60">{counts[key]}</span>
              </PageChip>
            ))}
            <input
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder={t("monitor.importFollow.search")}
              spellCheck={false}
              className="focus-ring ml-auto h-8 w-[170px] rounded-ctl border border-line bg-surface px-2.5 text-[12px] text-ink placeholder:text-faint"
            />
          </div>

          <div className="mt-2 flex items-center gap-2 border-y border-line py-2">
            <Checkbox
              checked={allVisibleSelected ? true : someVisibleSelected ? "indeterminate" : false}
              disabled={visibleSelectable.length === 0}
              onCheckedChange={toggleVisible}
              aria-label={t("monitor.importFollow.selectAll")}
            />
            <button
              type="button"
              onClick={toggleVisible}
              disabled={visibleSelectable.length === 0}
              className="focus-ring rounded-ctl px-1 text-[12px] text-muted hover:text-ink disabled:opacity-50"
            >
              {t("monitor.importFollow.selectAll")}
            </button>
            <span className="tnum text-[11.5px] text-faint">
              {t("monitor.importFollow.selected", { n: selected.size })}
            </span>
            {selected.size > 0 ? (
              <button
                type="button"
                onClick={() => setSelected(new Set())}
                className="focus-ring rounded-ctl px-1 text-[11.5px] text-faint hover:text-ink"
              >
                {t("monitor.importFollow.clear")}
              </button>
            ) : null}
          </div>

          <div className="min-h-0 flex-1 overflow-auto py-1">
            {phase === "follows" || phase === "status" ? (
              <div className="grid h-[190px] place-items-center text-[12px] text-faint">
                <span className="flex items-center gap-2">
                  <span className="h-3.5 w-3.5 animate-spin rounded-full border-[1.5px] border-line-strong border-t-accent" />
                  {statusHint}
                </span>
              </div>
            ) : filtered.length === 0 ? (
              <div className="grid h-[190px] place-items-center text-[12px] text-faint">
                {users.length === 0
                  ? t("monitor.importFollow.emptyFollows")
                  : t("monitor.importFollow.emptyFilter")}
              </div>
            ) : (
              filtered.map((user) => {
                const status = statuses.get(user.uid);
                const added = isExisting(user, status);
                const selectable = canImport(user, status);
                const checked = selected.has(user.uid);
                return (
                  <div
                    key={user.uid}
                    className={cx(
                      "flex items-center gap-3 rounded-[8px] px-2 py-2 transition-colors",
                      selectable ? "hover:bg-raised/60" : "opacity-60",
                    )}
                  >
                    <Checkbox
                      checked={checked}
                      disabled={!selectable || importing}
                      onCheckedChange={() => toggle(user)}
                      aria-label={user.uname}
                    />
                    <FollowAvatar user={user} />
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <span className="truncate text-[12.5px] font-medium text-ink">{user.uname}</span>
                        {status?.isLive ? (
                          <Chip tone="live" dot>
                            {t("monitor.importFollow.live")}
                          </Chip>
                        ) : null}
                        {added ? (
                          <Chip tone="accent">{t("monitor.importFollow.existing")}</Chip>
                        ) : null}
                      </div>
                      <div className="mt-0.5 truncate text-[11.5px] text-faint" title={status?.title}>
                        {status?.roomId
                          ? status.title || t("monitor.importFollow.unnamed")
                          : t("monitor.importFollow.noRoom")}
                      </div>
                    </div>
                    <div className="flex shrink-0 items-center gap-2">
                      {status?.areaName ? (
                        <span className="hidden max-w-[74px] truncate text-[11px] text-faint sm:block">
                          {status.areaName}
                        </span>
                      ) : null}
                      <span className="tnum w-[52px] text-right text-[11px] text-faint">
                        {status?.isLive && (status.online ?? 0) > 0
                          ? t("monitor.importFollow.online", { n: status.online })
                          : status?.roomId
                            ? t("monitor.importFollow.offline")
                            : "—"}
                      </span>
                    </div>
                  </div>
                );
              })
            )}
          </div>

          <div className="mt-1 grid grid-cols-2 gap-2 border-t border-line pt-3">
            <div className="flex items-center gap-2 rounded-[8px] border border-line px-2.5 py-2">
              <div className="min-w-0 flex-1">
                <div className="text-[12px] text-ink">{t("monitor.importFollow.monitor")}</div>
                <div className="mt-0.5 text-[10.5px] text-faint">{t("monitor.importFollow.monitorHint")}</div>
              </div>
              <Switch
                checked={monitor}
                onChange={(next) => {
                  setMonitor(next);
                  if (!next) setAutoRecord(false);
                }}
                label={t("monitor.importFollow.monitor")}
              />
            </div>
            <div className="flex items-center gap-2 rounded-[8px] border border-line px-2.5 py-2">
              <div className="min-w-0 flex-1">
                <div className="text-[12px] text-ink">{t("monitor.importFollow.autoRecord")}</div>
                <div className="mt-0.5 text-[10.5px] text-faint">
                  {monitor
                    ? t("monitor.importFollow.autoRecordHint")
                    : t("monitor.importFollow.autoRecordDisabled")}
                </div>
              </div>
              <Switch
                checked={autoRecord}
                disabled={!monitor}
                onChange={setAutoRecord}
                label={t("monitor.importFollow.autoRecord")}
              />
            </div>
          </div>

          <div className="mt-3 flex items-center justify-end gap-2">
            <Button size="lg" variant="ghost" onClick={() => onOpenChange(false)} disabled={importing}>
              {t("monitor.cancel")}
            </Button>
            <Button
              size="lg"
              variant="primary"
              onClick={() => void submit()}
              disabled={importing || selected.size === 0 || phase !== "ready"}
            >
              {importing
                ? t("monitor.importFollow.importing")
                : t("monitor.importFollow.action", { n: selected.size })}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
