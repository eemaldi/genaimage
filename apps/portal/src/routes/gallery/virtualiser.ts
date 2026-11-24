const gap = 12;

export function gridGeometry(width: number) {
  const columns = Math.max(1, Math.floor((width + gap) / (164 + gap)));
  const tileWidth = (Math.max(0, width) - gap * (columns - 1)) / columns;
  return { columns, rowHeight: Math.ceil(tileWidth * 0.75) + 88 };
}

/** Fixed-height rows; end is exclusive. No work proportional to item count. */
export function virtualRows(
  count: number,
  columns: number,
  rowHeight: number,
  top: number,
  height: number,
  overscan = 2,
) {
  const rows = Math.ceil(count / columns);
  const totalHeight = rows * rowHeight;
  const scroll = Math.min(Math.max(0, top), Math.max(0, totalHeight - height));
  const firstRow = Math.max(0, Math.floor(scroll / rowHeight) - overscan);
  const endRow = Math.min(
    rows,
    Math.ceil((scroll + height) / rowHeight) + overscan,
  );
  return {
    start: firstRow * columns,
    end: Math.min(count, endRow * columns),
    offset: firstRow * rowHeight,
    totalHeight,
  };
}
