import type { RoomRecord } from "../../lib/bindings/RoomRecord";
import type { RoomRulesInput } from "../../lib/ipc";

export function parseTags(tags: string): string[] {
  return tags
    .split(/[,，]/)
    .map((tag) => tag.trim())
    .filter(Boolean);
}

/** 把房间当前规则拼成保存载荷；内联备注/标签只覆盖自己的字段，其余保持原值。 */
export function toRulesInput(
  room: RoomRecord,
  patch: Partial<RoomRulesInput> = {},
): RoomRulesInput {
  return {
    note: room.note,
    tags: room.tags,
    titleInclude: room.titleInclude,
    titleExclude: room.titleExclude,
    splitMaxDurationSec: room.splitMaxDurationSec,
    splitMaxSizeMb: room.splitMaxSizeMb,
    notifyLive: room.notifyLive,
    notifyError: room.notifyError,
    ...patch,
  };
}
