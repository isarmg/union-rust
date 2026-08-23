export function adjacentPanelLayout({
  cardWidth,
  cardHeight,
  columnGap,
  rowGap,
  column,
  columnCount,
  top,
}: {
  cardWidth: number;
  cardHeight: number;
  columnGap: number;
  rowGap: number;
  column: number;
  columnCount: number;
  top: number;
}) {
  const panelColumns = Math.min(3, columnCount);
  const opensRight = column < Math.ceil(columnCount / 2);
  const requestedStart = opensRight ? column + 1 : column - panelColumns;
  const startColumn = Math.max(0, Math.min(requestedStart, columnCount - panelColumns));
  return {
    left: startColumn * (cardWidth + columnGap),
    top,
    width: panelColumns * cardWidth + (panelColumns - 1) * columnGap,
    height: 3 * cardHeight + 2 * rowGap,
    placement: opensRight ? "right" : "left",
  } as const;
}
