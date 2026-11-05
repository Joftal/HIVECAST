export type LayoutKind = "row" | "col" | "top" | "left" | "quad" | "portrait";

const RECTS: Record<LayoutKind, Array<[number, number, number, number]>> = {
  row: [[0, 0, 6.5, 10], [7.5, 0, 6.5, 10]],
  col: [[0, 0, 14, 4.5], [0, 5.5, 14, 4.5]],
  top: [[0, 0, 14, 6], [0, 7, 6.5, 3], [7.5, 7, 6.5, 3]],
  left: [[0, 0, 8.5, 10], [9.5, 0, 4.5, 4.5], [9.5, 5.5, 4.5, 4.5]],
  quad: [[0, 0, 6.5, 4.5], [7.5, 0, 6.5, 4.5], [0, 5.5, 6.5, 4.5], [7.5, 5.5, 6.5, 4.5]],
  portrait: [[0, 0, 3, 10], [3.7, 0, 3, 10], [7.3, 0, 3, 10], [11, 0, 3, 10]],
};

/** 版式预览图：一眼看出分屏怎么切。 */
export function LayoutGlyph({ kind, className }: { kind: LayoutKind; className?: string }) {
  return (
    <svg viewBox="0 0 14 10" width="14" height="10" fill="currentColor" aria-hidden className={className}>
      {RECTS[kind].map(([x, y, width, height], index) => (
        <rect key={index} x={x} y={y} width={width} height={height} rx={1} opacity={0.85} />
      ))}
    </svg>
  );
}
