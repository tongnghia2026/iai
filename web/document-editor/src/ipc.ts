export const IPC_PROTOCOL_VERSION = 1

export interface IpcEnvelope {
  protocol_version: number
  request_id: string
  type: string
  [key: string]: unknown
}

export interface HostBridgeHandlers {
  loadDocument(document: unknown, revision: number): void
  requestSnapshot(requestId: string): void
  focusEditor(): void
  setTheme(theme: EditorTheme): void
  setReadOnly(value: boolean): void
}

export type EditorTheme = 'dark' | 'light'

interface WryIpc {
  postMessage(message: string): void
}

declare global {
  interface Window {
    ipc?: WryIpc
    __iaiReceive?: (message: IpcEnvelope) => void
  }
}

let nextRequestId = 1

export function reconcileSnapshotRevision(
  revision: number,
  observedRevision: number,
  serializedDocument: string,
  observedSerializedDocument: string
): number {
  return revision === observedRevision &&
    serializedDocument !== observedSerializedDocument
    ? revision + 1
    : revision
}

export function makeEnvelope(
  type: string,
  requestId = `web-${nextRequestId++}`,
  payload: Record<string, unknown> = {}
): IpcEnvelope {
  return {
    protocol_version: IPC_PROTOCOL_VERSION,
    request_id: requestId,
    type,
    ...payload
  }
}

export function postEnvelope(envelope: IpcEnvelope): void {
  window.ipc?.postMessage(JSON.stringify(envelope))
}

export interface HostShortcutEvent {
  altKey: boolean
  code: string
  ctrlKey: boolean
  metaKey: boolean
  shiftKey: boolean
}

export function isHostCloseShortcut(event: HostShortcutEvent): boolean {
  return (
    (event.ctrlKey || event.metaKey) &&
    !event.altKey &&
    !event.shiftKey &&
    event.code === 'KeyW'
  )
}

const EDITOR_ZONE_KEYS = new Set(['header', 'main', 'footer', 'graffiti'])

export function retainDocumentRootFields(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return {}
  return Object.fromEntries(
    Object.entries(value).filter(([key]) => !EDITOR_ZONE_KEYS.has(key))
  )
}

export function mergeDocumentRootFields(
  rootFields: Record<string, unknown>,
  editorData: Record<string, unknown>
): Record<string, unknown> {
  return { ...rootFields, ...editorData }
}

export function handleHostEnvelope(
  message: IpcEnvelope,
  handlers: HostBridgeHandlers,
  post: (envelope: IpcEnvelope) => void = postEnvelope
): void {
  if (
    message.protocol_version !== IPC_PROTOCOL_VERSION ||
    typeof message.request_id !== 'string' ||
    message.request_id.length === 0
  ) {
    return
  }

  switch (message.type) {
    case 'ping':
      post(makeEnvelope('pong', message.request_id))
      break
    case 'load_document':
      if (typeof message.revision === 'number') {
        handlers.loadDocument(message.document, message.revision)
      }
      break
    case 'request_snapshot':
      handlers.requestSnapshot(message.request_id)
      break
    case 'focus_editor':
      handlers.focusEditor()
      break
    case 'set_theme':
      if (message.theme === 'dark' || message.theme === 'light') {
        handlers.setTheme(message.theme)
      }
      break
    case 'set_read_only':
      if (typeof message.value === 'boolean') {
        handlers.setReadOnly(message.value)
      }
      break
  }
}

export function installHostReceiver(handlers: HostBridgeHandlers): void {
  window.__iaiReceive = message => handleHostEnvelope(message, handlers)
}
