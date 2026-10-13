const POINTS_PER_INCH = 72
const EDITOR_PIXELS_PER_INCH = 96

export function pointsToEditorPixels(points: number): number {
  return (points * EDITOR_PIXELS_PER_INCH) / POINTS_PER_INCH
}

export function editorPixelsToPoints(pixels: number): number {
  return (pixels * POINTS_PER_INCH) / EDITOR_PIXELS_PER_INCH
}

export function formatZoomPercent(scale: number): string {
  const safeScale = Number.isFinite(scale) && scale > 0 ? scale : 1
  return `${Math.round(safeScale * 100)}%`
}

export function normalizeColorForPicker(color: string | null): string {
  if (!color) return '#000000'
  const hex = /^#([0-9a-f]{6})$/i.exec(color.trim())
  if (hex) return `#${hex[1].toLowerCase()}`

  const rgb = /^rgba?\(\s*(\d{1,3})\s*,\s*(\d{1,3})\s*,\s*(\d{1,3})/i.exec(
    color
  )
  if (!rgb) return '#000000'
  const channels = rgb.slice(1, 4).map(value =>
    Math.min(255, Number(value)).toString(16).padStart(2, '0')
  )
  return `#${channels.join('')}`
}

export function parseHexColor(color: string): [number, number, number] | null {
  const match = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(color.trim())
  if (!match) return null
  return [
    Number.parseInt(match[1], 16),
    Number.parseInt(match[2], 16),
    Number.parseInt(match[3], 16)
  ]
}

export function rgbToHex(red: number, green: number, blue: number): string {
  const channel = (value: number): string =>
    Math.round(Math.min(255, Math.max(0, Number.isFinite(value) ? value : 0)))
      .toString(16)
      .padStart(2, '0')
  return `#${channel(red)}${channel(green)}${channel(blue)}`
}

export function hslToHex(hue: number, saturation: number, lightness: number): string {
  const normalizedHue = ((hue % 360) + 360) % 360
  const normalizedSaturation = Math.min(100, Math.max(0, saturation)) / 100
  const normalizedLightness = Math.min(100, Math.max(0, lightness)) / 100
  const chroma = (1 - Math.abs(2 * normalizedLightness - 1)) * normalizedSaturation
  const sector = normalizedHue / 60
  const secondary = chroma * (1 - Math.abs((sector % 2) - 1))
  const [red, green, blue] =
    sector < 1
      ? [chroma, secondary, 0]
      : sector < 2
        ? [secondary, chroma, 0]
        : sector < 3
          ? [0, chroma, secondary]
          : sector < 4
            ? [0, secondary, chroma]
            : sector < 5
              ? [secondary, 0, chroma]
              : [chroma, 0, secondary]
  const match = normalizedLightness - chroma / 2
  return rgbToHex((red + match) * 255, (green + match) * 255, (blue + match) * 255)
}
