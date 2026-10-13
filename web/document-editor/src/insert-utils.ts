export const MAX_TABLE_ROWS = 20
export const MAX_TABLE_COLUMNS = 12
export const MAX_EMBEDDED_IMAGE_BYTES = 5 * 1024 * 1024
export const MAX_INSERTED_IMAGE_WIDTH = 560
export const MAX_INSERTED_IMAGE_HEIGHT = 720

export interface ImageSize {
  width: number
  height: number
}

export function parseTableSize(rows: string, columns: string): [number, number] | null {
  const rowCount = Number(rows)
  const columnCount = Number(columns)
  if (
    !Number.isInteger(rowCount) ||
    !Number.isInteger(columnCount) ||
    rowCount < 1 ||
    rowCount > MAX_TABLE_ROWS ||
    columnCount < 1 ||
    columnCount > MAX_TABLE_COLUMNS
  ) {
    return null
  }
  return [rowCount, columnCount]
}

export function normalizeHyperlinkUrl(value: string): string | null {
  const trimmed = value.trim()
  if (!trimmed) return null

  const candidate = /^[a-z][a-z\d+.-]*:/i.test(trimmed) ? trimmed : `https://${trimmed}`
  try {
    const url = new URL(candidate)
    return url.protocol === 'http:' || url.protocol === 'https:' || url.protocol === 'mailto:'
      ? url.href
      : null
  } catch {
    return null
  }
}

export function fitImageSize(
  width: number,
  height: number,
  maxWidth = MAX_INSERTED_IMAGE_WIDTH,
  maxHeight = MAX_INSERTED_IMAGE_HEIGHT
): ImageSize | null {
  if (
    !Number.isFinite(width) ||
    !Number.isFinite(height) ||
    !Number.isFinite(maxWidth) ||
    !Number.isFinite(maxHeight) ||
    width <= 0 ||
    height <= 0 ||
    maxWidth <= 0 ||
    maxHeight <= 0
  ) {
    return null
  }
  const scale = Math.min(1, maxWidth / width, maxHeight / height)
  return {
    width: Math.max(1, Math.round(width * scale)),
    height: Math.max(1, Math.round(height * scale))
  }
}

