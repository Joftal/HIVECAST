"use client";

import { useEffect, useRef, useState, type CSSProperties, type FocusEvent, type PointerEvent } from "react";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import { CircleCheck, Undo2 } from "lucide-react";
import styles from "./undo-toast.module.css";

export interface UndoToastProps {
  open: boolean;
  title: string;
  description?: string;
  actionLabel: string;
  /** How long the undo window stays open, in milliseconds. */
  duration?: number;
  onUndo: () => void;
  onDismiss: () => void;
  /** Accessible name of the live region. */
  label?: string;
}

/**
 * A bottom-center confirmation with a draining countdown ring and a single Undo action.
 * The timer pauses while the pointer or keyboard focus is inside, so the action is always reachable.
 * Built in-house from the Arc tokens because UIArc's Undo toast is a Pro block without a public registry JSON.
 */
export function UndoToast({
  open,
  title,
  description,
  actionLabel,
  duration = 8000,
  onUndo,
  onDismiss,
  label,
}: UndoToastProps) {
  const reduce = useReducedMotion() ?? false;
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const paused = hovered || focused;
  const [remaining, setRemaining] = useState(1);
  const left = useRef(duration);
  const undoRef = useRef(onUndo);
  const dismissRef = useRef(onDismiss);
  undoRef.current = onUndo;
  dismissRef.current = onDismiss;

  useEffect(() => {
    if (!open) return;
    left.current = duration;
    setRemaining(1);
  }, [open, duration]);

  // rAF 负责倒计时环；WebView 在窗口失去焦点时可能暂停 rAF，再加一个定时器兜底，
  // 保证撤销窗口一定会结束，不会留下一条永远不消失的 toast。
  useEffect(() => {
    if (!open || paused) return;
    const timer = window.setTimeout(() => dismissRef.current(), Math.max(0, left.current));
    return () => window.clearTimeout(timer);
  }, [open, paused]);

  useEffect(() => {
    if (!open || paused) return;
    let frame = 0;
    let last = performance.now();
    let painted = 0;
    const tick = (now: number) => {
      left.current = Math.max(0, left.current - Math.max(0, now - last));
      last = now;
      if (now - painted > 80) {
        painted = now;
        setRemaining(duration > 0 ? left.current / duration : 0);
      }
      if (left.current <= 0) {
        dismissRef.current();
        return;
      }
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [open, paused, duration]);

  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.key.toLowerCase() !== "z") return;
      const target = event.target as HTMLElement | null;
      if (target?.closest("input, textarea, [contenteditable='true']")) return;
      event.preventDefault();
      undoRef.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open]);

  const onEnter = (_event: PointerEvent<HTMLDivElement>) => setHovered(true);
  const onLeave = (_event: PointerEvent<HTMLDivElement>) => setHovered(false);
  const onFocus = (_event: FocusEvent<HTMLDivElement>) => setFocused(true);
  const onBlur = (event: FocusEvent<HTMLDivElement>) => {
    if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setFocused(false);
  };

  return (
    <AnimatePresence>
      {open ? (
        <motion.div
          className={styles.viewport}
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: reduce ? 0 : 0.16 }}
        >
          <motion.div
            className={styles.card}
            role="status"
            aria-live="polite"
            aria-label={label}
            initial={reduce ? { opacity: 0 } : { opacity: 0, y: 14, scale: 0.98 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={reduce ? { opacity: 0 } : { opacity: 0, y: 8, scale: 0.98 }}
            transition={reduce ? { duration: 0 } : { type: "spring", stiffness: 460, damping: 36 }}
            onPointerEnter={onEnter}
            onPointerLeave={onLeave}
            onFocusCapture={onFocus}
            onBlurCapture={onBlur}
          >
            <span
              className={styles.ring}
              style={{ "--progress": remaining } as CSSProperties}
              aria-hidden="true"
            >
              <CircleCheck className={styles.ringIcon} size={15} strokeWidth={1.9} />
            </span>
            <span className={styles.copy}>
              <span className={styles.title}>{title}</span>
              {description ? <span className={styles.description}>{description}</span> : null}
            </span>
            <button type="button" className={styles.action} onClick={() => undoRef.current()}>
              <Undo2 size={13} strokeWidth={1.9} aria-hidden="true" />
              {actionLabel}
            </button>
          </motion.div>
        </motion.div>
      ) : null}
    </AnimatePresence>
  );
}

export default UndoToast;
