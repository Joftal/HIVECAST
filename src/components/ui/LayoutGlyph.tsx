export type LayoutKind = "row" | "col" | "top" | "left" | "quad" | "portrait";

/** 16×12 画布：四边留 0.4，所有切缝统一 0.8，保证缩到 18px 时四块间距看起来一致。 */
const RECTS: Record<LayoutKind, Array<[number, number, number, number]>> = {
  row: [
    [0.4, 0.4, 7.2, 11.2],
    [8.4, 0.4, 7.2, 11.2],
  ],
  col: [
    [0.4, 0.4, 15.2, 5.2],
    [0.4, 6.4, 15.2, 5.2],
  ],
  top: [
    [0.4, 0.4, 15.2, 6.4],
    [0.4, 7.6, 7.2, 4],
    [8.4, 7.6, 7.2, 4],
  ],
  left: [
    [0.4, 0.4, 9.2, 11.2],
    [10.4, 0.4, 5.2, 5.2],
    [10.4, 6.4, 5.2, 5.2],
  ],
  quad: [
    [0.4, 0.4, 7.2, 5.2],
    [8.4, 0.4, 7.2, 5.2],
    [0.4, 6.4, 7.2, 5.2],
    [8.4, 6.4, 7.2, 5.2],
  ],
  portrait: [
    [0.4, 0.4, 3.2, 11.2],
    [4.4, 0.4, 3.2, 11.2],
    [8.4, 0.4, 3.2, 11.2],
    [12.4, 0.4, 3.2, 11.2],
  ],
};

/** 版式预览图：一眼看出分屏怎么切。 */
export function LayoutGlyph({ kind, className }: { kind: LayoutKind; className?: string }) {
  return (
    <svg viewBox="0 0 16 12" width="18" height="13.5" fill="currentColor" aria-hidden className={className}>
      {RECTS[kind].map(([x, y, width, height], index) => (
        <rect key={index} x={x} y={y} width={width} height={height} rx={1.2} opacity={0.9} />
      ))}
    </svg>
  );
}
