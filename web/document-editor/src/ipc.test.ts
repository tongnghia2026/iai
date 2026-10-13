import { describe, expect, it } from 'vitest'
import {
  handleHostEnvelope,
  isHostCloseShortcut,
  mergeDocumentRootFields,
  IPC_PROTOCOL_VERSION,
  makeEnvelope,
  reconcileSnapshotRevision,
  retainDocumentRootFields,
  type HostBridgeHandlers,
  type IpcEnvelope
} from './ipc'

describe('IPC envelope', () => {
  it('preserves host-owned root metadata around editor snapshots', () => {
    const loaded = {
      header: [],
      main: [{ value: 'cũ' }],
      footer: [],
      _iai: { schema_version: 1 },
      future: { preserved: true }
    }
    const rootFields = retainDocumentRootFields(loaded)
    expect(rootFields).toEqual({
      _iai: { schema_version: 1 },
      future: { preserved: true }
    })
    expect(
      mergeDocumentRootFields(rootFields, {
        header: [],
        main: [{ value: 'mới' }],
        footer: []
      })
    ).toEqual({
      _iai: { schema_version: 1 },
      future: { preserved: true },
      header: [],
      main: [{ value: 'mới' }],
      footer: []
    })
  })

  it('recognizes only the host close shortcut', () => {
    const shortcut = {
      altKey: false,
      code: 'KeyW',
      ctrlKey: true,
      metaKey: false,
      shiftKey: false
    }
    expect(isHostCloseShortcut(shortcut)).toBe(true)
    expect(isHostCloseShortcut({ ...shortcut, ctrlKey: false })).toBe(false)
    expect(isHostCloseShortcut({ ...shortcut, shiftKey: true })).toBe(false)
    expect(isHostCloseShortcut({ ...shortcut, code: 'KeyS' })).toBe(false)
  })

  it('advances revision when a snapshot detects a missed content event', () => {
    expect(reconcileSnapshotRevision(4, 4, '{"main":[2]}', '{"main":[1]}')).toBe(5)
    expect(reconcileSnapshotRevision(5, 4, '{"main":[2]}', '{"main":[1]}')).toBe(5)
    expect(reconcileSnapshotRevision(4, 4, '{"main":[1]}', '{"main":[1]}')).toBe(4)
  })

  it('keeps protocol, request id, type and payload stable', () => {
    expect(makeEnvelope('ping', 'rust-7', { nonce: 9 })).toEqual({
      protocol_version: IPC_PROTOCOL_VERSION,
      request_id: 'rust-7',
      type: 'ping',
      nonce: 9
    })
  })

  it('routes load, snapshot and focus without changing request ids', () => {
    const calls: unknown[] = []
    const posted: IpcEnvelope[] = []
    const handlers: HostBridgeHandlers = {
      loadDocument: (document, revision) =>
        calls.push(['load', document, revision]),
      requestSnapshot: requestId => calls.push(['snapshot', requestId]),
      focusEditor: () => calls.push(['focus']),
      setTheme: theme => calls.push(['theme', theme]),
      setReadOnly: value => calls.push(['read-only', value])
    }

    handleHostEnvelope(
      makeEnvelope('load_document', 'rust-load', {
        revision: 4,
        document: { main: [{ value: 'Việt Nam' }] }
      }),
      handlers,
      envelope => posted.push(envelope)
    )
    handleHostEnvelope(
      makeEnvelope('request_snapshot', 'rust-snapshot'),
      handlers,
      envelope => posted.push(envelope)
    )
    handleHostEnvelope(
      makeEnvelope('focus_editor', 'rust-focus'),
      handlers,
      envelope => posted.push(envelope)
    )
    handleHostEnvelope(
      makeEnvelope('ping', 'rust-ping'),
      handlers,
      envelope => posted.push(envelope)
    )
    handleHostEnvelope(
      makeEnvelope('set_theme', 'rust-theme', { theme: 'dark' }),
      handlers,
      envelope => posted.push(envelope)
    )
    handleHostEnvelope(
      makeEnvelope('set_read_only', 'rust-read-only', { value: true }),
      handlers,
      envelope => posted.push(envelope)
    )

    expect(calls).toEqual([
      ['load', { main: [{ value: 'Việt Nam' }] }, 4],
      ['snapshot', 'rust-snapshot'],
      ['focus'],
      ['theme', 'dark'],
      ['read-only', true]
    ])
    expect(posted).toEqual([
      makeEnvelope('pong', 'rust-ping')
    ])
  })
})
