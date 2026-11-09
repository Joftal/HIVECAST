import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { roomSetRules } from "../../lib/ipc";
import type { RoomRecord } from "../../lib/bindings/RoomRecord";
import { useNotify } from "../../lib/notify";
import { toRulesInput } from "./roomRuleInput";

/** 监控列表里的主播备注：点击进入内联输入，Enter/失焦保存，Escape 取消。 */
export default function InlineNote({ room }: { room: RoomRecord }) {
  const { t } = useTranslation();
  const notify = useNotify();
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(room.note);
  const [saving, setSaving] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    setDraft(room.note);
  }, [room.note, room.id]);

  useEffect(() => {
    if (editing) inputRef.current?.focus();
  }, [editing]);

  const save = async () => {
    if (saving) return;
    const next = draft.trim();
    if (next === room.note) {
      setEditing(false);
      return;
    }
    setSaving(true);
    try {
      await roomSetRules(room.id, toRulesInput(room, { note: next }));
      setDraft(next);
      setEditing(false);
    } catch (err) {
      notify.error(err);
    } finally {
      setSaving(false);
    }
  };

  if (editing) {
    return (
      <input
        ref={inputRef}
        value={draft}
        disabled={saving}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={() => void save()}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            void save();
          } else if (event.key === "Escape") {
            event.preventDefault();
            setDraft(room.note);
            setEditing(false);
          }
        }}
        placeholder={t("monitor.notePlaceholder")}
        aria-label={t("monitor.colNote")}
        className="focus-ring h-7 w-full rounded-ctl border border-line bg-surface px-1.5 text-[11.5px] text-ink transition-colors placeholder:text-faint hover:border-line-strong"
      />
    );
  }

  return (
    <button
      type="button"
      onClick={() => {
        setDraft(room.note);
        setEditing(true);
      }}
      aria-label={t("monitor.editNote")}
      className="group flex h-8 w-full min-w-0 items-center gap-1 rounded-ctl px-1.5 text-left transition-colors hover:bg-raised"
    >
      <span className={room.note ? "min-w-0 truncate text-[12px] text-ink-soft" : "min-w-0 truncate text-[11.5px] text-faint"}>
        {room.note || t("monitor.addNote")}
      </span>
      <svg
        width="11"
        height="11"
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.4"
        className="ml-auto shrink-0 text-faint opacity-0 transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100"
        aria-hidden
      >
        <path d="M11.2 2.8a1.2 1.2 0 0 1 1.7 1.7l-7.1 7.1-2.4.7.7-2.4 7.1-7.1Z" strokeLinejoin="round" />
      </svg>
    </button>
  );
}
