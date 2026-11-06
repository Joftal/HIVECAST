import { viewSuspend } from "./ipc";

/**
 * 观看页的生命周期闸门。
 *
 * 切走观看页时要停掉所有转封装，但 Tauri 命令是异步的：用户快速来回切 Tab 时，
 * 新的观看页可能先读到「还没清掉的旧槽位」。这里把挂起串成一条链，
 * 观看页在恢复布局前先 await 这条链，保证读到的一定是挂起之后的状态。
 */
let pending: Promise<void> = Promise.resolve();

export function suspendViews(): Promise<void> {
  pending = pending.then(() => viewSuspend()).catch(() => undefined);
  return pending;
}

export function whenViewsIdle(): Promise<void> {
  return pending;
}
