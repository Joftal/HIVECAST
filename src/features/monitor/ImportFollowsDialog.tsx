import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Dialog, DialogContent } from "../../components/arc/dialog/dialog";
import { Checkbox } from "../../components/arc/checkbox/checkbox";
import { Button, Chip, Segmented, Switch, cx } from "../../components/ui/Controls";
import { PageChip } from "../../components/ui/PageShell";
import { useNotify } from "../../lib/notify";
import {
  bilibiliFollowings,
  bilibiliFollowStatuses,
  douyinFollowPage,
  douyinPoolAdd,
  douyuFollowPage,
  roomsImport,
  type DouyinFollow,
  type DouyinPoolAddResult,
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
type Source = "bilibili" | "douyu" | "douyin";

const FILTERS: Filter[] = ["all", "live", "room", "new", "existing"];
const SOURCES: Source[] = ["bilibili", "douyu", "douyin"];

function wait(ms: number) {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}

/** 把关注池条目转成列表用的直播状态；离线项没有 web_rid，roomId 留空。 */
function douyinStatus(item: DouyinFollow): LiveStatus {
  return {
    uid: item.uid,
    roomId: item.webRid,
    shortId: item.internalRoomId,
    uname: item.uname,
    title: item.title,
    avatarUrl: item.avatarUrl,
    coverUrl: item.coverUrl,
    areaName: item.areaName,
    isLive: item.isLive,
    videoLoop: false,
    startedAt: item.startedAt,
    online: item.online,
  };
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
 * 关注导入（B 站 / 斗鱼 / 抖音）。
 *
 * B 站关注列表每页 50 条，拉完后用一次批量状态接口补齐直播间；斗鱼关注接口直接带
 * 直播状态，每页 120 条，不再逐房间请求 betard。抖音关注每页 20 条，用 max_time
 * 游标翻页；在播且能拿到 web_rid 的直接建房，离线或缺少 web_rid 的先进关注池，
 * 由后台扫描开播后再转成正式房间。导入本身只写本地库；已存在的房间只刷新展示
 * 字段，不覆盖画质、线路、备注和标签。
 */
export default function ImportFollowsDialog({ open, onOpenChange, rooms, onImported }: Props) {
  const { t } = useTranslation();
  const notify = useNotify();

  const [source, setSource] = useState<Source>("bilibili");
  const [phase, setPhase] = useState<Phase>("follows");
  const [users, setUsers] = useState<FollowingUser[]>([]);
  const [statuses, setStatuses] = useState<Map<string, LiveStatus>>(new Map());
  const [douyinMeta, setDouyinMeta] = useState<Map<string, DouyinFollow>>(new Map());
  const [total, setTotal] = useState(0);
  const [pageProgress, setPageProgress] = useState({ done: 0, total: 1 });
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [filter, setFilter] = useState<Filter>("all");
  const [query, setQuery] = useState("");
  const [monitor, setMonitor] = useState(true);
  const [autoRecord, setAutoRecord] = useState(false);
  const [importing, setImporting] = useState(false);
  const [result, setResult] = useState<ImportRoomsResult | null>(null);
  const [poolResult, setPoolResult] = useState<DouyinPoolAddResult | null>(null);
  const [reload, setReload] = useState(0);

  // 每次重新打开都从 B 站开始；切换平台由下面的加载 effect 负责重新拉取。
  useEffect(() => {
    if (open) setSource("bilibili");
  }, [open]);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setPhase("follows");
    setUsers([]);
    setStatuses(new Map());
    setDouyinMeta(new Map());
    setTotal(0);
    setPageProgress({ done: 0, total: 1 });
    setSelected(new Set());
    setFilter("all");
    setQuery("");
    setMonitor(true);
    setAutoRecord(false);
    setImporting(false);
    setResult(null);
    setPoolResult(null);

    void (async () => {
      if (source === "bilibili") {
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
        return;
      }

      if (source === "douyin") {
        // 抖音：每页 20 条，cursor 是上一页的 min_time；关注池标记随列表一起下发。
        const meta = new Map<string, DouyinFollow>();
        const publish = () => {
          if (cancelled) return;
          const items = Array.from(meta.values());
          setDouyinMeta(new Map(meta));
          setUsers(
            items.map((item) => ({ uid: item.uid, uname: item.uname, avatarUrl: item.avatarUrl })),
          );
          setStatuses(new Map(items.map((item) => [item.uid, douyinStatus(item)])));
          setTotal(items.length);
        };
        try {
          let cursor: string | null = null;
          let pages = 0;
          for (;;) {
            const page = await douyinFollowPage(cursor);
            if (cancelled) return;
            pages += 1;
            for (const item of page.items) meta.set(item.uid, item);
            publish();
            setPageProgress({ done: pages, total: page.hasMore ? pages + 1 : pages });
            if (!page.hasMore || !page.nextCursor || page.nextCursor === cursor) break;
            cursor = page.nextCursor;
            await wait(180);
            if (cancelled) return;
          }
        } catch {
          if (!cancelled) setPhase("followError");
          return;
        }
        if (!cancelled) setPhase("ready");
        return;
      }

      // 斗鱼：关注接口已经带状态，按页累积成 LiveStatus，再用 roomId 作为稳定键。
      const live = new Map<string, LiveStatus>();
      const publish = () => {
        if (cancelled) return;
        const items = Array.from(live.values());
        setUsers(
          items.map((status) => ({
            uid: status.roomId ?? status.uid,
            uname: status.uname,
            avatarUrl: status.avatarUrl,
          })),
        );
        setStatuses(new Map(items.map((status) => [status.roomId ?? status.uid, status])));
        setTotal(items.length);
      };
      const absorb = (items: LiveStatus[]) => {
        for (const status of items) {
          live.set(status.roomId ?? status.uid, status);
        }
      };
      try {
        const first = await douyuFollowPage(1);
        if (cancelled) return;
        absorb(first.items);
        const pages = Math.max(
          1,
          first.pageCount || Math.ceil(Math.max(first.total, first.items.length) / (first.pageSize || 120)),
        );
        publish();
        setPageProgress({ done: 1, total: pages });
        for (let page = 2; page <= pages; page += 1) {
          await wait(180);
          if (cancelled) return;
          const next = await douyuFollowPage(page);
          if (cancelled) return;
          absorb(next.items);
          publish();
          setPageProgress({ done: page, total: pages });
        }
      } catch {
        if (!cancelled) setPhase("followError");
        return;
      }
      if (!cancelled) setPhase("ready");
    })();

    return () => {
      cancelled = true;
    };
  }, [open, reload, source]);

  const existing = useMemo(() => {
    const keys = new Set<string>();
    for (const view of rooms) {
      if (view.room.platform !== source) continue;
      keys.add(view.room.roomKey);
      if (view.room.platformUid) keys.add(view.room.platformUid);
    }
    // 关注池里的主播还没建房，但已经导入过，不能再重复添加。
    if (source === "douyin") {
      for (const item of douyinMeta.values()) {
        if (item.inPool || item.promotedRoomId) keys.add(item.uid);
      }
    }
    return keys;
  }, [rooms, source, douyinMeta]);

  // 先手动添加、后导入关注的抖音房间：已经建房但 batch_follow 还是 0。
  // 允许再次勾选，导入时后端只补批量标记，不会重复建房。
  const douyinBatchKeys = useMemo(() => {
    const keys = new Set<string>();
    if (source !== "douyin") return keys;
    for (const view of rooms) {
      if (view.room.platform !== "douyin" || view.room.batchFollow) continue;
      keys.add(view.room.roomKey);
      if (view.room.platformUid) keys.add(view.room.platformUid);
      if (view.room.internalRoomId) keys.add(view.room.internalRoomId);
    }
    return keys;
  }, [rooms, source]);

  const isExisting = (user: FollowingUser, status?: LiveStatus) =>
    existing.has(user.uid) ||
    (!!status?.roomId && existing.has(status.roomId)) ||
    (!!status?.shortId && existing.has(status.shortId));
  const isBatchUpgradable = (user: FollowingUser, status?: LiveStatus) =>
    source === "douyin" &&
    (douyinBatchKeys.has(user.uid) ||
      (!!status?.roomId && douyinBatchKeys.has(status.roomId)) ||
      (!!status?.shortId && douyinBatchKeys.has(status.shortId)));
  const canImport = (user: FollowingUser, status?: LiveStatus) => {
    // 抖音允许把离线关注放进关注池；已有的手动房允许补成批量房。
    if (source === "douyin") {
      if (isBatchUpgradable(user, status)) return true;
      return !isExisting(user, status) && douyinMeta.has(user.uid);
    }
    if (isExisting(user, status)) return false;
    return !!status?.roomId;
  };

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
  }, [users, statuses, query, filter, existing, douyinBatchKeys]);

  const counts = useMemo(() => {
    let live = 0;
    let room = 0;
    let added = 0;
    let fresh = 0;
    let pool = 0;
    for (const user of users) {
      const status = statuses.get(user.uid);
      if (status?.isLive) live += 1;
      if (status?.roomId) room += 1;
      if (isExisting(user, status)) added += 1;
      if (canImport(user, status)) fresh += 1;
      const meta = douyinMeta.get(user.uid);
      if (meta?.inPool && !meta.promotedRoomId) pool += 1;
    }
    return { all: users.length, live, room, existing: added, new: fresh, pool };
  }, [users, statuses, existing, douyinMeta]);

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
    // 斗鱼和抖音没有独立的「关注 -> 状态」第二阶段，整页重载即可。
    if (source !== "bilibili") {
      setReload((value) => value + 1);
      return;
    }
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
      .map((user) => ({
        user,
        status: statuses.get(user.uid),
        meta: douyinMeta.get(user.uid),
      }))
      .filter((item) => canImport(item.user, item.status));
    if (chosen.length === 0) {
      notify.info(t("monitor.importFollow.nothingSelected"));
      return;
    }

    setImporting(true);
    try {
      let imported: ImportRoomsResult | null = null;
      let pooled: DouyinPoolAddResult | null = null;

      if (source === "douyin") {
        // 在播且有 web_rid 的立即建房；离线或拿不到 web_rid 的先进关注池。
        const ready = chosen.filter(
          (item): item is { user: FollowingUser; status: LiveStatus | undefined; meta: DouyinFollow } =>
            !!item.meta?.isLive && !!item.meta.webRid,
        );
        const pending = chosen
          .map((item) => item.meta)
          .filter((meta): meta is DouyinFollow => !!meta && !(meta.isLive && meta.webRid));

        if (ready.length > 0) {
          const items: ImportRoomInput[] = ready.map(({ user, meta }) => ({
            platform: "douyin",
            uid: user.uid,
            roomId: meta.webRid as string,
            shortId: meta.internalRoomId,
            internalRoomId: meta.internalRoomId,
            uname: meta.uname || user.uname,
            title: meta.title,
            avatarUrl: meta.avatarUrl ?? user.avatarUrl,
            coverUrl: meta.coverUrl,
            areaName: meta.areaName,
            isLive: true,
            startedAt: meta.startedAt,
            online: meta.online,
            videoLoop: false,
          }));
          imported = await roomsImport(items, monitor, autoRecord);
          setResult(imported);
        }

        if (pending.length > 0) {
          pooled = await douyinPoolAdd(pending, monitor, autoRecord);
          setPoolResult(pooled);
          setDouyinMeta((current) => {
            const next = new Map(current);
            for (const item of pending) next.set(item.uid, { ...item, inPool: true });
            return next;
          });
        }
      } else {
        const items: ImportRoomInput[] = chosen.map(({ user, status }) => ({
          platform: source,
          uid: user.uid,
          roomId: status?.roomId as string,
          shortId: status?.shortId ?? null,
          uname: status?.uname || user.uname,
          title: status?.title ?? "",
          avatarUrl: status?.avatarUrl ?? user.avatarUrl,
          coverUrl: status?.coverUrl ?? null,
          areaName: status?.areaName ?? null,
          isLive: status?.isLive ?? false,
          startedAt: status?.startedAt ?? null,
          online: status?.online ?? null,
          videoLoop: status?.videoLoop ?? false,
          internalRoomId: null,
        }));
        imported = await roomsImport(items, monitor, autoRecord);
        setResult(imported);
      }

      setSelected(new Set());
      await onImported?.();
      if (imported && imported.failed.length > 0) {
        notify.warning(imported.failed[0].message, t("monitor.importFollow.failedTitle"));
      } else {
        const parts: string[] = [];
        if (imported) {
          parts.push(
            t("monitor.importFollow.doneBody", {
              added: imported.added,
              existing: imported.existing,
            }),
          );
        }
        if (pooled) {
          parts.push(
            t("monitor.importFollow.poolDoneBody", {
              added: pooled.added,
              updated: pooled.updated,
            }),
          );
        }
        notify.success(t("monitor.importFollow.done"), parts.join(" · "));
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
      ? source === "douyin"
        ? t("monitor.importFollow.loadingDouyin", { done: pageProgress.done, n: users.length })
        : source === "douyu"
          ? t("monitor.importFollow.loadingDouyu", { done: pageProgress.done, total: pageProgress.total })
          : t("monitor.importFollow.loading", { done: pageProgress.done, total: pageProgress.total })
      : phase === "status"
        ? t("monitor.importFollow.statusLoading", { n: users.length })
        : source === "douyin"
          ? t("monitor.importFollow.summaryDouyin", {
              total: users.length,
              live: counts.live,
              pool: counts.pool,
            })
          : source === "douyu"
            ? t("monitor.importFollow.summaryDouyu", { total: users.length, live: counts.live })
            : t("monitor.importFollow.summary", { total, room: counts.room, live: counts.live });
  const sourceLabel = {
    bilibili: t("monitor.importFollow.platformBilibili"),
    douyu: t("monitor.importFollow.platformDouyu"),
    douyin: t("monitor.importFollow.platformDouyin"),
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        title={
          source === "douyin"
            ? t("monitor.importFollow.titleDouyin")
            : source === "douyu"
              ? t("monitor.importFollow.titleDouyu")
              : t("monitor.importFollow.title")
        }
        description={
          source === "douyin"
            ? t("monitor.importFollow.descriptionDouyin")
            : source === "douyu"
              ? t("monitor.importFollow.descriptionDouyu")
              : t("monitor.importFollow.description")
        }
        size="lg"
      >
        <div className="flex h-[min(560px,calc(100dvh-200px))] min-h-[360px] flex-col">
          <div className="mb-3 flex items-center justify-between gap-3">
            <Segmented
              items={SOURCES.map((key) => ({ key, label: sourceLabel[key] }))}
              value={source}
              onChange={(next) => setSource(next)}
            />
            <span className="min-w-0 truncate text-[11.5px] text-faint">
              {source === "douyin"
                ? t("monitor.importFollow.platformHintDouyin")
                : source === "douyu"
                  ? t("monitor.importFollow.platformHintDouyu")
                  : t("monitor.importFollow.platformHintBilibili")}
            </span>
          </div>

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
              {source === "douyin"
                ? t("monitor.importFollow.followFailedDouyin")
                : source === "douyu"
                  ? t("monitor.importFollow.followFailedDouyu")
                  : t("monitor.importFollow.followFailed")}
            </div>
          ) : null}
          {phase === "statusError" ? (
            <div className="mt-3 rounded-[8px] border border-warn/30 bg-warn/10 px-3 py-2 text-[12px] leading-5 text-warn">
              {t("monitor.importFollow.statusFailed")}
            </div>
          ) : null}
          {result || poolResult ? (
            <div
              className={cx(
                "mt-3 rounded-[8px] border px-3 py-2 text-[12px] leading-5",
                result && result.failed.length > 0
                  ? "border-warn/30 bg-warn/10 text-warn"
                  : "border-live/30 bg-live/10 text-live",
              )}
            >
              {result
                ? t("monitor.importFollow.doneBody", {
                    added: result.added,
                    existing: result.existing,
                  })
                : null}
              {result && poolResult ? " · " : null}
              {poolResult
                ? t("monitor.importFollow.poolDoneBody", {
                    added: poolResult.added,
                    updated: poolResult.updated,
                  })
                : null}
              {result && result.failed.length > 0
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
                const meta = douyinMeta.get(user.uid);
                const pooled = !!meta?.inPool && !meta.promotedRoomId;
                const added = isExisting(user, status);
                const batchJoin = isBatchUpgradable(user, status);
                const selectable = canImport(user, status);
                const checked = selected.has(user.uid);
                return (
                  <div
                    key={user.uid}
                    className={cx(
                      "flex min-w-0 items-center gap-3 rounded-[8px] px-2 py-2 transition-colors",
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
                    <div className="min-w-0 flex-1 overflow-hidden">
                      <div className="flex min-w-0 items-center gap-2">
                        <span className="min-w-0 truncate text-[12.5px] font-medium text-ink" title={user.uname}>
                          {user.uname}
                        </span>
                        {status?.isLive ? (
                          <Chip tone="live" dot>
                            {t("monitor.importFollow.live")}
                          </Chip>
                        ) : null}
                        {pooled ? (
                          <Chip tone="accent">{t("monitor.importFollow.poolChip")}</Chip>
                        ) : added ? (
                          <Chip tone="accent">{t("monitor.importFollow.existing")}</Chip>
                        ) : null}
                        {batchJoin ? (
                          <Chip tone="accent">{t("monitor.importFollow.batchJoinChip")}</Chip>
                        ) : null}
                      </div>
                      <div className="mt-0.5 truncate text-[11.5px] text-faint" title={status?.title}>
                        {pooled
                          ? t("monitor.importFollow.poolHint")
                          : status?.roomId
                            ? status.title || t("monitor.importFollow.unnamed")
                            : t("monitor.importFollow.noRoom")}
                      </div>
                    </div>
                    <div className="flex shrink-0 items-center gap-2 whitespace-nowrap">
                      {status?.areaName ? (
                        <span className="hidden max-w-[84px] truncate text-[11px] text-faint sm:block">
                          {status.areaName}
                        </span>
                      ) : null}
                      <span className="tnum min-w-[52px] whitespace-nowrap text-right text-[11px] text-faint">
                        {pooled
                          ? t("monitor.importFollow.offline")
                          : status?.isLive && (status.online ?? 0) > 0
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
