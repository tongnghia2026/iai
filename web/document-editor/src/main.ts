import Editor, { EditorMode, type IEditorData } from '@hufe921/canvas-editor'
import '@fontsource/noto-sans/vietnamese-400.css'
import './style.css'
import {
  IPC_PROTOCOL_VERSION,
  installHostReceiver,
  makeEnvelope,
  isHostCloseShortcut,
  mergeDocumentRootFields,
  postEnvelope,
  reconcileSnapshotRevision,
  retainDocumentRootFields
} from './ipc'
import { installEditorToolbar } from './toolbar'

const CANVAS_EDITOR_VERSION = '1.0.2'
const container = document.querySelector<HTMLDivElement>('#document-editor')

if (!container) {
  throw new Error('Document editor container is missing')
}

const initialDocument: IEditorData = {
  header: [],
  main: [
    {
      value: 'Tiếng Việt — Nguyễn Thị Thu — Ắ Ề Ễ Ự'
    }
  ],
  footer: []
}

const editor = new Editor(
  container,
  initialDocument,
  { defaultFont: 'Noto Sans' }
)
const toolbar = installEditorToolbar(editor.command)

let revision = 0
let loadingDocument = false
let retainedRootFields: Record<string, unknown> = {}
let observedRevision = revision
let observedSerializedDocument = serializeDocument()

editor.listener.contentChange = () => {
  if (loadingDocument) return
  revision += 1
  postEnvelope(makeEnvelope('document_changed', undefined, { revision }))
}

editor.listener.saved = () => {
  postEnvelope(makeEnvelope('save_requested', undefined, { revision }))
}

editor.listener.rangeStyleChange = style => toolbar.setRangeStyle(style)
editor.listener.pageScaleChange = scale => toolbar.setPageScale(scale)

// A child WebView owns keyboard focus while the editor caret is active, so
// application shortcuts never reach winit. Forward Ctrl+W explicitly and let
// the Rust lifecycle gate snapshot/confirm/close the correct tab.
document.addEventListener(
  'keydown',
  event => {
    if (!isHostCloseShortcut(event)) return
    event.preventDefault()
    event.stopImmediatePropagation()
    postEnvelope(makeEnvelope('close_requested', undefined, { revision }))
  },
  { capture: true }
)

installHostReceiver({
  loadDocument(document, hostRevision) {
    if (!isEditorData(document)) return
    loadingDocument = true
    try {
      editor.command.executeSetValue(document, { isSetCursor: false })
      retainedRootFields = retainDocumentRootFields(document)
      applyIaiPageSetup(retainedRootFields)
      revision = hostRevision
      observedRevision = revision
      observedSerializedDocument = serializeDocument()
    } finally {
      loadingDocument = false
    }
  },
  requestSnapshot(requestId) {
    const snapshot = editor.command.getValue()
    const document = mergeDocumentRootFields(
      retainedRootFields,
      snapshot.data as unknown as Record<string, unknown>
    )
    const serializedDocument = JSON.stringify(document)
    revision = reconcileSnapshotRevision(
      revision,
      observedRevision,
      serializedDocument,
      observedSerializedDocument
    )
    observedRevision = revision
    observedSerializedDocument = serializedDocument
    postEnvelope(
      makeEnvelope('snapshot', requestId, {
        revision,
        document
      })
    )
  },
  focusEditor() {
    editor.command.executeFocus()
  },
  setTheme(theme) {
    document.documentElement.dataset.iaiTheme = theme
  },
  setReadOnly(value) {
    editor.command.executeMode(value ? EditorMode.READONLY : EditorMode.EDIT)
    toolbar.setReadOnly(value)
  }
})

postEnvelope(
  makeEnvelope('ready', undefined, {
    editor_version: CANVAS_EDITOR_VERSION,
    protocol_version: IPC_PROTOCOL_VERSION
  })
)

function serializeDocument(): string {
  return JSON.stringify(
    mergeDocumentRootFields(
      retainedRootFields,
      editor.command.getValue().data as unknown as Record<string, unknown>
    )
  )
}

function applyIaiPageSetup(rootFields: Record<string, unknown>): void {
  const iai = rootFields._iai
  if (typeof iai !== 'object' || iai === null || Array.isArray(iai)) return
  const pageSetup = (iai as Record<string, unknown>).page_setup
  if (
    typeof pageSetup !== 'object' ||
    pageSetup === null ||
    Array.isArray(pageSetup)
  ) {
    return
  }
  const page = pageSetup as Record<string, unknown>
  if (isPositiveFinite(page.width) && isPositiveFinite(page.height)) {
    editor.command.executePaperSize(page.width, page.height)
  }
  if (
    Array.isArray(page.margins) &&
    page.margins.length === 4 &&
    page.margins.every(isNonNegativeFinite)
  ) {
    editor.command.executeSetPaperMargin(page.margins as [number, number, number, number])
  }
}

function isPositiveFinite(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value) && value > 0
}

function isNonNegativeFinite(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0
}

function isEditorData(value: unknown): value is IEditorData {
  if (typeof value !== 'object' || value === null) return false
  const candidate = value as Partial<IEditorData>
  return (
    Array.isArray(candidate.main) &&
    (candidate.header === undefined || Array.isArray(candidate.header)) &&
    (candidate.footer === undefined || Array.isArray(candidate.footer))
  )
}
