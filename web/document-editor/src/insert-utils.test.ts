import { describe, expect, it } from 'vitest'
import { fitImageSize, normalizeHyperlinkUrl, parseTableSize } from './insert-utils'

describe('insert value validation', () => {
  it('accepts table dimensions only inside the supported range', () => {
    expect(parseTableSize('3', '4')).toEqual([3, 4])
    expect(parseTableSize('0', '4')).toBeNull()
    expect(parseTableSize('3.5', '4')).toBeNull()
    expect(parseTableSize('3', '13')).toBeNull()
  })

  it('normalizes web and mail links while rejecting unsafe schemes', () => {
    expect(normalizeHyperlinkUrl('example.com')).toBe('https://example.com/')
    expect(normalizeHyperlinkUrl('mailto:test@example.com')).toBe('mailto:test@example.com')
    expect(normalizeHyperlinkUrl('javascript:alert(1)')).toBeNull()
    expect(normalizeHyperlinkUrl('')).toBeNull()
  })

  it('fits an image into the editor bounds without enlarging it', () => {
    expect(fitImageSize(1200, 600, 560, 720)).toEqual({ width: 560, height: 280 })
    expect(fitImageSize(100, 80, 560, 720)).toEqual({ width: 100, height: 80 })
    expect(fitImageSize(0, 80)).toBeNull()
  })
})
