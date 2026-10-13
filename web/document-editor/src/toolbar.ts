import {
  ListStyle,
  ListType,
  RowFlex,
  type Command,
  type IRangeStyle
} from '@hufe921/canvas-editor'
import {
  editorPixelsToPoints,
  formatZoomPercent,
  hslToHex,
  normalizeColorForPicker,
  parseHexColor,
  pointsToEditorPixels
} from './toolbar-state'
import {
  fitImageSize,
  MAX_EMBEDDED_IMAGE_BYTES,
  normalizeHyperlinkUrl,
  parseTableSize
} from './insert-utils'

type ToolbarButtonId =
  | 'toolbar-undo'
  | 'toolbar-redo'
  | 'toolbar-bold'
  | 'toolbar-italic'
  | 'toolbar-underline'
  | 'toolbar-align-left'
  | 'toolbar-align-center'
  | 'toolbar-align-right'
  | 'toolbar-align-justify'
  | 'toolbar-bullet-list'
  | 'toolbar-number-list'
  | 'toolbar-color-button'
  | 'toolbar-insert-image'
  | 'toolbar-insert-table'
  | 'toolbar-insert-link'
  | 'toolbar-insert-separator'
  | 'toolbar-insert-page-break'
  | 'toolbar-zoom-out'
  | 'toolbar-zoom-in'

export interface EditorToolbar {
  setRangeStyle(style: IRangeStyle): void
  setPageScale(scale: number): void
  setReadOnly(readOnly: boolean): void
}

export function installEditorToolbar(command: Command): EditorToolbar {
  const buttons = new Map<ToolbarButtonId, HTMLButtonElement>()
  const button = (id: ToolbarButtonId): HTMLButtonElement => {
    const element = requireElement<HTMLButtonElement>(id)
    buttons.set(id, element)
    return element
  }

  const undo = button('toolbar-undo')
  const redo = button('toolbar-redo')
  const bold = button('toolbar-bold')
  const italic = button('toolbar-italic')
  const underline = button('toolbar-underline')
  const alignLeft = button('toolbar-align-left')
  const alignCenter = button('toolbar-align-center')
  const alignRight = button('toolbar-align-right')
  const alignJustify = button('toolbar-align-justify')
  const bulletList = button('toolbar-bullet-list')
  const numberList = button('toolbar-number-list')
  const colorButton = button('toolbar-color-button')
  const insertImage = button('toolbar-insert-image')
  const insertTable = button('toolbar-insert-table')
  const insertLink = button('toolbar-insert-link')
  const insertSeparator = button('toolbar-insert-separator')
  const insertPageBreak = button('toolbar-insert-page-break')
  const zoomOut = button('toolbar-zoom-out')
  const zoomIn = button('toolbar-zoom-in')

  const font = requireElement<HTMLSelectElement>('toolbar-font')
  const size = requireElement<HTMLSelectElement>('toolbar-size')
  const color = requireElement<HTMLInputElement>('toolbar-color')
  const colorIndicator = requireElement<HTMLSpanElement>('toolbar-color-indicator')
  const colorPreview = requireElement<HTMLSpanElement>('toolbar-color-preview')
  const colorPopover = requireElement<HTMLDivElement>('toolbar-color-popover')
  const colorSwatches = requireElement<HTMLDivElement>('toolbar-color-swatches')
  const colorOk = requireElement<HTMLButtonElement>('toolbar-color-ok')
  const colorCancel = requireElement<HTMLButtonElement>('toolbar-color-cancel')
  const imageFile = requireElement<HTMLInputElement>('toolbar-image-file')
  const tableDialog = requireElement<HTMLDialogElement>('toolbar-table-dialog')
  const tableRows = requireElement<HTMLInputElement>('toolbar-table-rows')
  const tableColumns = requireElement<HTMLInputElement>('toolbar-table-cols')
  const tableError = requireElement<HTMLParagraphElement>('toolbar-table-error')
  const tableCancel = requireElement<HTMLButtonElement>('toolbar-table-cancel')
  const linkDialog = requireElement<HTMLDialogElement>('toolbar-link-dialog')
  const linkText = requireElement<HTMLInputElement>('toolbar-link-text')
  const linkUrl = requireElement<HTMLInputElement>('toolbar-link-url')
  const linkError = requireElement<HTMLParagraphElement>('toolbar-link-error')
  const linkCancel = requireElement<HTMLButtonElement>('toolbar-link-cancel')
  const lineSpacing = requireElement<HTMLSelectElement>('toolbar-line-spacing')
  const zoomValue = requireElement<HTMLOutputElement>('toolbar-zoom-value')

  const mutationControls: Array<HTMLButtonElement | HTMLSelectElement | HTMLInputElement> = [
    undo,
    redo,
    bold,
    italic,
    underline,
    alignLeft,
    alignCenter,
    alignRight,
    alignJustify,
    bulletList,
    numberList,
    colorButton,
    insertImage,
    insertTable,
    insertLink,
    insertSeparator,
    insertPageBreak,
    font,
    size,
    color,
    lineSpacing
  ]

  let readOnly = false
  let rangeStyle: IRangeStyle | null = null
  let committedColor = '#000000'
  let pendingColor = committedColor

  const mutate = (action: () => void): void => {
    if (readOnly) return
    action()
    command.executeFocus()
  }

  // Buttons must not clear the editor selection before their command runs.
  for (const item of buttons.values()) {
    item.addEventListener('mousedown', event => event.preventDefault())
  }

  undo.addEventListener('click', () => mutate(() => command.executeUndo()))
  redo.addEventListener('click', () => mutate(() => command.executeRedo()))
  bold.addEventListener('click', () => mutate(() => command.executeBold()))
  italic.addEventListener('click', () => mutate(() => command.executeItalic()))
  underline.addEventListener('click', () => mutate(() => command.executeUnderline()))

  font.addEventListener('change', () => mutate(() => command.executeFont(font.value)))
  size.addEventListener('change', () => {
    const points = Number(size.value)
    if (Number.isFinite(points) && points > 0) {
      mutate(() => command.executeSize(pointsToEditorPixels(points)))
    }
  })
  installColorSwatches(colorSwatches, selected => setPendingColor(selected))
  colorButton.addEventListener('click', () => {
    if (readOnly) return
    if (colorPopover.hidden) openColorPopover()
    else closeColorPopover()
  })
  color.addEventListener('input', () => {
    if (parseHexColor(color.value)) setPendingColor(color.value)
  })
  color.addEventListener('change', () => {
    if (!parseHexColor(color.value)) color.value = pendingColor
  })
  colorOk.addEventListener('click', () => {
    committedColor = pendingColor
    closeColorPopover()
    mutate(() => command.executeColor(committedColor))
  })
  colorCancel.addEventListener('click', () => {
    setPendingColor(committedColor)
    closeColorPopover()
    command.executeFocus()
  })
  document.addEventListener('pointerdown', event => {
    if (
      !colorPopover.hidden &&
      !colorPopover.contains(event.target as Node) &&
      !colorButton.contains(event.target as Node)
    ) {
      setPendingColor(committedColor)
      closeColorPopover()
    }
  })
  document.addEventListener('keydown', event => {
    if (event.key === 'Escape' && !colorPopover.hidden) {
      event.preventDefault()
      setPendingColor(committedColor)
      closeColorPopover()
      colorButton.focus()
    }
  })

  insertImage.addEventListener('click', () => {
    if (!readOnly) imageFile.click()
  })
  imageFile.addEventListener('change', () => {
    const file = imageFile.files?.[0]
    imageFile.value = ''
    if (!file) return
    void insertImageFile(file, command).catch(error => {
      window.alert(error instanceof Error ? error.message : 'Không thể chèn ảnh.')
      command.executeFocus()
    })
  })

  insertTable.addEventListener('click', () => {
    if (readOnly) return
    hideDialogError(tableError)
    tableDialog.showModal()
    tableRows.focus()
    tableRows.select()
  })
  tableCancel.addEventListener('click', () => cancelDialog(tableDialog, command))
  tableDialog.addEventListener('cancel', () => command.executeFocus())
  tableDialog.querySelector('form')?.addEventListener('submit', event => {
    event.preventDefault()
    const dimensions = parseTableSize(tableRows.value, tableColumns.value)
    if (!dimensions) {
      showDialogError(tableError, 'Nhập 1–20 hàng và 1–12 cột.')
      return
    }
    tableDialog.close('ok')
    mutate(() => command.executeInsertTable(...dimensions))
  })

  insertLink.addEventListener('click', () => {
    if (readOnly) return
    hideDialogError(linkError)
    linkText.value = command.getRangeText().replace(/\n/g, ' ').trim()
    linkUrl.value = ''
    linkDialog.showModal()
    ;(linkText.value ? linkUrl : linkText).focus()
  })
  linkCancel.addEventListener('click', () => cancelDialog(linkDialog, command))
  linkDialog.addEventListener('cancel', () => command.executeFocus())
  linkDialog.querySelector('form')?.addEventListener('submit', event => {
    event.preventDefault()
    const url = normalizeHyperlinkUrl(linkUrl.value)
    if (!url) {
      showDialogError(linkError, 'Địa chỉ phải dùng http, https hoặc mailto.')
      linkUrl.focus()
      return
    }
    const text = linkText.value.trim() || url
    linkDialog.close('ok')
    mutate(() => command.executeHyperlink({ url, valueList: [{ value: text }] }))
  })

  insertSeparator.addEventListener('click', () =>
    mutate(() => command.executeSeparator([]))
  )
  insertPageBreak.addEventListener('click', () => mutate(() => command.executePageBreak()))

  alignLeft.addEventListener('click', () =>
    mutate(() => command.executeRowFlex(RowFlex.LEFT))
  )
  alignCenter.addEventListener('click', () =>
    mutate(() => command.executeRowFlex(RowFlex.CENTER))
  )
  alignRight.addEventListener('click', () =>
    mutate(() => command.executeRowFlex(RowFlex.RIGHT))
  )
  alignJustify.addEventListener('click', () =>
    mutate(() => command.executeRowFlex(RowFlex.ALIGNMENT))
  )

  bulletList.addEventListener('click', () =>
    mutate(() =>
      command.executeList(
        rangeStyle?.listType === ListType.UL ? null : ListType.UL,
        ListStyle.DISC
      )
    )
  )
  numberList.addEventListener('click', () =>
    mutate(() =>
      command.executeList(
        rangeStyle?.listType === ListType.OL ? null : ListType.OL,
        ListStyle.DECIMAL
      )
    )
  )
  lineSpacing.addEventListener('change', () => {
    const spacing = Number(lineSpacing.value)
    if (Number.isFinite(spacing) && spacing > 0) {
      mutate(() => command.executeRowMargin(spacing))
    }
  })

  zoomOut.addEventListener('click', () => {
    command.executePageScaleMinus()
    command.executeFocus()
  })
  zoomIn.addEventListener('click', () => {
    command.executePageScaleAdd()
    command.executeFocus()
  })

  const render = (): void => {
    for (const control of mutationControls) control.disabled = readOnly
    undo.disabled = readOnly || !rangeStyle?.undo
    redo.disabled = readOnly || !rangeStyle?.redo
    if (!rangeStyle) return

    setPressed(bold, rangeStyle.bold)
    setPressed(italic, rangeStyle.italic)
    setPressed(underline, rangeStyle.underline)
    setPressed(alignLeft, rangeStyle.rowFlex === RowFlex.LEFT)
    setPressed(alignCenter, rangeStyle.rowFlex === RowFlex.CENTER)
    setPressed(alignRight, rangeStyle.rowFlex === RowFlex.RIGHT)
    setPressed(
      alignJustify,
      rangeStyle.rowFlex === RowFlex.ALIGNMENT || rangeStyle.rowFlex === RowFlex.JUSTIFY
    )
    setPressed(bulletList, rangeStyle.listType === ListType.UL)
    setPressed(numberList, rangeStyle.listType === ListType.OL)

    setSelectValue(font, rangeStyle.font)
    setSelectValue(size, formatNumber(editorPixelsToPoints(rangeStyle.size)))
    setSelectValue(lineSpacing, formatNumber(rangeStyle.rowMargin))
    committedColor = normalizeColorForPicker(rangeStyle.color)
    if (colorPopover.hidden) setPendingColor(committedColor)
    colorIndicator.style.backgroundColor = committedColor
  }

  function openColorPopover(): void {
    pendingColor = committedColor
    setPendingColor(pendingColor)
    colorPopover.hidden = false
    colorButton.setAttribute('aria-expanded', 'true')
    colorPopover.querySelector<HTMLButtonElement>('.color-swatch')?.focus()
  }

  function closeColorPopover(): void {
    colorPopover.hidden = true
    colorButton.setAttribute('aria-expanded', 'false')
  }

  function setPendingColor(value: string): void {
    pendingColor = normalizeColorForPicker(value)
    color.value = pendingColor
    colorPreview.style.backgroundColor = pendingColor
    for (const swatch of colorSwatches.querySelectorAll<HTMLButtonElement>('.color-swatch')) {
      swatch.classList.toggle('is-selected', swatch.dataset.color === pendingColor)
    }
  }

  render()

  return {
    setRangeStyle(style) {
      rangeStyle = style
      render()
    },
    setPageScale(scale) {
      zoomValue.value = formatZoomPercent(scale)
      zoomValue.textContent = zoomValue.value
    },
    setReadOnly(value) {
      readOnly = value
      if (value) {
        closeColorPopover()
        if (tableDialog.open) tableDialog.close('cancel')
        if (linkDialog.open) linkDialog.close('cancel')
      }
      render()
    }
  }
}

function requireElement<T extends HTMLElement>(id: string): T {
  const element = document.getElementById(id)
  if (!element) throw new Error(`Toolbar element #${id} is missing`)
  return element as T
}

function setPressed(button: HTMLButtonElement, pressed: boolean): void {
  button.classList.toggle('is-active', pressed)
  button.setAttribute('aria-pressed', String(pressed))
}

function setSelectValue(select: HTMLSelectElement, value: string): void {
  if (![...select.options].some(option => option.value === value)) {
    select.add(new Option(value, value))
  }
  select.value = value
}

function formatNumber(value: number): string {
  return Number(value.toFixed(2)).toString()
}

const PALETTE_HUES = [0, 28, 52, 110, 160, 190, 220, 250, 285, 325] as const
const PALETTE_TONES = [
  [70, 88],
  [75, 72],
  [80, 55],
  [80, 42],
  [75, 30]
] as const
const TEXT_COLORS = [
  ...Array.from({ length: 10 }, (_, index) => {
    const channel = Math.round((index / 9) * 255)
    return `#${channel.toString(16).padStart(2, '0').repeat(3)}`
  }),
  ...PALETTE_TONES.flatMap(([saturation, lightness]) =>
    PALETTE_HUES.map(hue => hslToHex(hue, saturation, lightness))
  )
]

function installColorSwatches(container: HTMLElement, select: (color: string) => void): void {
  for (const swatchColor of TEXT_COLORS) {
    const swatch = document.createElement('button')
    swatch.type = 'button'
    swatch.className = 'color-swatch'
    swatch.dataset.color = swatchColor
    swatch.title = swatchColor
    swatch.setAttribute('aria-label', `Chọn màu ${swatchColor}`)
    swatch.style.setProperty('--swatch-color', swatchColor)
    swatch.addEventListener('click', () => select(swatchColor))
    container.append(swatch)
  }
}

function cancelDialog(dialog: HTMLDialogElement, command: Command): void {
  dialog.close('cancel')
  command.executeFocus()
}

function showDialogError(element: HTMLElement, message: string): void {
  element.textContent = message
  element.hidden = false
}

function hideDialogError(element: HTMLElement): void {
  element.textContent = ''
  element.hidden = true
}

async function insertImageFile(file: File, command: Command): Promise<void> {
  if (!file.type.startsWith('image/')) {
    throw new Error('Tệp đã chọn không phải là ảnh được hỗ trợ.')
  }
  if (file.size > MAX_EMBEDDED_IMAGE_BYTES) {
    throw new Error('Ảnh phải nhỏ hơn hoặc bằng 5 MB để tài liệu có thể lưu an toàn.')
  }

  const dataUrl = await readFileAsDataUrl(file)
  const naturalSize = await loadImageSize(dataUrl)
  const fittedSize = fitImageSize(naturalSize.width, naturalSize.height)
  if (!fittedSize) throw new Error('Không đọc được kích thước ảnh.')

  command.executeImage({
    value: dataUrl,
    width: fittedSize.width,
    height: fittedSize.height,
    extension: { fileName: file.name }
  })
  command.executeFocus()
}

function readFileAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.addEventListener('load', () => {
      if (typeof reader.result === 'string') resolve(reader.result)
      else reject(new Error('Không đọc được dữ liệu ảnh.'))
    })
    reader.addEventListener('error', () => reject(new Error('Không đọc được tệp ảnh.')))
    reader.readAsDataURL(file)
  })
}

function loadImageSize(source: string): Promise<{ width: number; height: number }> {
  return new Promise((resolve, reject) => {
    const image = new Image()
    image.addEventListener('load', () =>
      resolve({ width: image.naturalWidth, height: image.naturalHeight })
    )
    image.addEventListener('error', () => reject(new Error('Định dạng ảnh không đọc được.')))
    image.src = source
  })
}
