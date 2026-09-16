import { describe, expect, it } from 'vitest'
import {
  editorPixelsToPoints,
  formatZoomPercent,
  hslToHex,
  normalizeColorForPicker,
  parseHexColor,
  pointsToEditorPixels,
  rgbToHex
} from './toolbar-state'

describe('toolbar value mapping', () => {
  it('converts Word point sizes to Canvas Editor pixels and back', () => {
    expect(pointsToEditorPixels(12)).toBe(16)
    expect(editorPixelsToPoints(16)).toBe(12)
  })

  it('formats valid and invalid zoom scales', () => {
    expect(formatZoomPercent(1.25)).toBe('125%')
    expect(formatZoomPercent(Number.NaN)).toBe('100%')
  })

  it('normalizes Canvas Editor colors for the native color input', () => {
    expect(normalizeColorForPicker('#A1B2C3')).toBe('#a1b2c3')
    expect(normalizeColorForPicker('rgba(12, 34, 255, 0.5)')).toBe('#0c22ff')
    expect(normalizeColorForPicker('invalid')).toBe('#000000')
  })

  it('maps custom RGB channels to and from a hex color', () => {
    expect(parseHexColor('#0c22ff')).toEqual([12, 34, 255])
    expect(parseHexColor('#xyzxyz')).toBeNull()
    expect(rgbToHex(12, 34, 255)).toBe('#0c22ff')
    expect(rgbToHex(-1, 128.4, 300)).toBe('#0080ff')
    expect(hslToHex(0, 100, 50)).toBe('#ff0000')
    expect(hslToHex(120, 100, 50)).toBe('#00ff00')
    expect(hslToHex(240, 100, 50)).toBe('#0000ff')
  })
})
