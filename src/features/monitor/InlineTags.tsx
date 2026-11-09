import { useEffect, useRef, useState } from "react";
import type { FocusEvent, KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";
import { TagInput } from "../../components/arc/tag-input/tag-input";
import TagBadge from "../../components/ui/TagBadge";
import { roomSetRules } from "../../lib/ipc";
import type { RoomRecord } from "../../lib/bindings/RoomRecord";
import { useNotify } from "../../lib/notify";
import { parseTags, toRulesInput } from "./roomRuleInput";

/** 监控列表里的标签：点击进入 Arc TagInput，Enter/逗号添加，失焦保存。 */
export default function InlineTags({ room }: { room: RoomRecord }) {
  const { t } = useTranslation();
  const notify = useNotify();
  const [editing, setEditing] = useState(false);
  const [tags, setTags] = useState(() => parseTags(room.tags));
  const [saving, setSaving] = useState(false);
  const tagsRef = useRef(tags);
  tagsRef.current = tags;

  useEffect(() => {
    const next = parseTags(room.tags);
    setTags(next);
    tagsRef.current = next;
  }, [room.tags, room.id]);

  const save = async () => {
    if (saving) return;
    const next = tagsRef.current;
    const text = next.join(", ");
    if (text === room.tags) {
      setEditing(false);
      return;
    }
    setSaving(true);
    try {
      await roomSetRules(room.id, toRulesInput(room, { tags: text }));
      setEditing(false);
    } catch (err) {
      notify.error(err);
    } finally {
      setSaving(false);
    }
  };

  const onBlur = (event: FocusEvent<HTMLDivElement>) => {
    if (!event.currentTarget.contains(event.relatedTarget as Node | null)) void save();
  };

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "Escape") return;
    const input = event.target as HTMLInputElement;
    if (input.value) return;
    event.preventDefault();
    event.stopPropagation();
    void save();
  };

  if (editing) {
    return (
      <div onBlur={onBlur} onKeyDownCapture={onKeyDown} className="min-w-0">
        <TagInput
          label={t("monitor.colTags")}
          value={tags}
          onValueChange={(next) => {
            tagsRef.current = next;
            setTags(next);
          }}
          placeholder={t("monitor.addTag")}
          compact
          autoFocus
        />
      </div>
    );
  }

  return (
    <button
      type="button"
      onClick={() => setEditing(true)}
      aria-label={t("monitor.editTags")}
      title={tags.length > 0 ? tags.join(", ") : t("monitor.addTag")}
      className="group flex min-h-8 w-full min-w-0 items-center gap-1 overflow-hidden rounded-ctl px-1.5 text-left transition-colors hover:bg-raised"
    >
      {tags.length === 0 ? (
        <span className="min-w-0 truncate text-[11.5px] text-faint">{t("monitor.addTag")}</span>
      ) : (
        <>
          {tags.slice(0, 2).map((tag) => (
            <TagBadge key={tag} tag={tag} className="max-w-[64px] shrink-0" />
          ))}
          {tags.length > 2 ? (
            <span className="shrink-0 text-[9.5px] text-faint">+{tags.length - 2}</span>
          ) : null}
        </>
      )}
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
