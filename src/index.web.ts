/// <reference lib="dom" />
import type {
  AggregateProgress,
  ProgressCallback,
  UploadProgress,
  WebS3BgUploaderAPI,
} from './specs/s3-bg-uploader.types'
import { S3BgUploaderResumeError, S3BgUploaderDuplicateFileError } from './specs/s3-bg-uploader.types'
import { UPLOADER_JS_CODE, UPLOADER_WASM_BASE64 } from './web/wasm-assets'

// ---------------------------------------------------------------------------
// Inline worker source
// Both the wasm_bindgen JS glue and the WASM binary are embedded at build
// time by uploader/scripts/embed-wasm.js — no importScripts, no external URLs,
// no bundler configuration required by consumers.
// ---------------------------------------------------------------------------


function buildWorkerSource(jsCode: string, wasmBase64: string): string {
  return `
/* Inline wasm_bindgen JS bindings — no importScripts needed */
${jsCode}

/* Initialize WASM from embedded base64 data */
const _wasmBase64 = ${JSON.stringify(wasmBase64)};
const _wasmBytes = Uint8Array.from(atob(_wasmBase64), function(c) { return c.charCodeAt(0); });
const _wasmReady = wasm_bindgen(_wasmBytes.buffer);

/* Load persisted session from IndexedDB, then signal ready */
const sessionReady = _wasmReady.then(function() {
  return wasm_bindgen.wasm_load_session();
});

onmessage = async function (e) {
  await sessionReady;
  const wasm = wasm_bindgen;

  const { type, requestId, payload } = e.data;

  switch (type) {
    case 'setConfig': {
      wasm.wasm_set_config(payload.startUploadApi, payload.getUploadUrlsApi, payload.completeApi);
      postMessage({ type: 'ack', requestId });
      break;
    }

    case 'setProgressCallback': {
      if (payload.enabled) {
        wasm.set_progress_callback(function(data) {
          postMessage({
            type: 'progress',
            data: {
              fileProgress: data.fileProgress,
              sessionAggregate: data.sessionAggregate,
              transferAggregate: data.transferAggregate,
            },
          });
        });
      } else {
        wasm.set_progress_callback(null);
      }
      postMessage({ type: 'ack', requestId });
      break;
    }

    case 'setTaskTitle':
    case 'setTaskSubtitle': {
      /* No-op on web — notifications not supported */
      postMessage({ type: 'ack', requestId });
      break;
    }

    case 'uploadFile': {
      try {
        const fileKey = await wasm.upload_file(payload.file, payload.transferId, payload.userParams ?? null);
        postMessage({ type: 'result', requestId, ok: true, value: fileKey });
      } catch (err) {
        postMessage({ type: 'result', requestId, ok: false, error: String(err) });
      }
      break;
    }

    case 'cancelFile': {
      wasm.wasm_cancel_file(payload.fileHash);
      postMessage({ type: 'ack', requestId });
      break;
    }

    case 'cancelTransfer': {
      wasm.wasm_cancel_transfer(payload.transferId);
      postMessage({ type: 'ack', requestId });
      break;
    }

    case 'cancelAll': {
      wasm.wasm_cancel_all();
      postMessage({ type: 'ack', requestId });
      break;
    }

    case 'pause': {
      wasm.wasm_pause_all();
      postMessage({ type: 'ack', requestId });
      break;
    }

    case 'resume': {
      try {
        wasm.wasm_resume_all();
        postMessage({ type: 'ack', requestId });
      } catch (err) {
        postMessage({ type: 'result', requestId, ok: false, error: String(err) });
      }
      break;
    }

    case 'getProgress': {
      const result = wasm.wasm_get_progress(
        payload.transferId ?? undefined,
        payload.fileKey ?? undefined,
      );
      postMessage({ type: 'result', requestId, ok: true, value: result });
      break;
    }

    case 'getAggregateProgress': {
      const result = wasm.wasm_get_aggregate_progress(payload.transferId ?? undefined);
      postMessage({ type: 'result', requestId, ok: true, value: result });
      break;
    }

    default:
      postMessage({ type: 'error', requestId, error: 'Unknown message type: ' + type });
  }
};
`
}

// ---------------------------------------------------------------------------
// Worker singleton + message routing
// ---------------------------------------------------------------------------

let worker: Worker | null = null
let progressCallback: ProgressCallback | null = null

// Map from requestId → { resolve, reject } for request/response pairing
const pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>()
let nextId = 1

function getWorker(): Worker {
  if (!worker) {
    const src = buildWorkerSource(UPLOADER_JS_CODE, UPLOADER_WASM_BASE64)
    const blob = new Blob([src], { type: 'application/javascript' })
    const url = URL.createObjectURL(blob)
    worker = new Worker(url)
    URL.revokeObjectURL(url)

    worker.onmessage = (e) => {
      const { type, requestId, data, ok, value, error } = e.data

      if (type === 'progress') {
        if (progressCallback) {
          const { fileProgress, sessionAggregate, transferAggregate } = data as {
            fileProgress: UploadProgress
            sessionAggregate: AggregateProgress
            transferAggregate: AggregateProgress
          }
          progressCallback(fileProgress, sessionAggregate, transferAggregate)
        }
        return
      }

      const handler = pending.get(requestId)
      if (!handler) return

      pending.delete(requestId)

      if (type === 'ack') {
        handler.resolve(undefined)
      } else if (type === 'result') {
        if (ok) {
          handler.resolve(value)
        } else {
          handler.reject(new Error(error))
        }
      } else if (type === 'error') {
        handler.reject(new Error(error))
      }
    }

    worker.onerror = (e) => {
      for (const h of pending.values()) h.reject(new Error(e.message))
      pending.clear()
    }
  }
  return worker
}

function sendRequest(type: string, payload: Record<string, unknown> = {}): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const requestId = nextId++
    pending.set(requestId, { resolve, reject })
    getWorker().postMessage({ type, requestId, payload })
  })
}

// ---------------------------------------------------------------------------
// Public API (WebS3BgUploaderAPI)
// ---------------------------------------------------------------------------

export const S3BgUploader: WebS3BgUploaderAPI = {
  setConfig(startUploadApi: string, getUploadUrlsApi: string, completeApi: string): void {
    sendRequest('setConfig', { startUploadApi, getUploadUrlsApi, completeApi })
  },

  setProgressCallback(callback: ProgressCallback | null): void {
    progressCallback = callback
    sendRequest('setProgressCallback', { enabled: callback !== null })
  },

  setTaskTitle(_title: string): void {
    /* No-op on web — background notifications not supported */
  },

  setTaskSubtitle(_subtitle: string): void {
    /* No-op on web — background notifications not supported */
  },

  async uploadFile(file: File, transferId: string, userParams?: Record<string, string>): Promise<string> {
    if (typeof file === 'string') {
      throw new TypeError('S3BgUploader: On web platform, uploadFile requires a File value, not a string path.')
    }
    try {
      return await sendRequest('uploadFile', { file, transferId, userParams: userParams ?? null }) as Promise<string>
    } catch (e: unknown) {
      const msg = (e as Error)?.message ?? String(e)
      if (msg.includes('DUPLICATE_FILE')) throw new S3BgUploaderDuplicateFileError(msg)
      throw e
    }
  },

  cancelFile(fileHash: string): void {
    sendRequest('cancelFile', { fileHash })
  },

  cancelTransfer(transferId: string): void {
    sendRequest('cancelTransfer', { transferId })
  },

  cancel(): void {
    sendRequest('cancelAll')
  },

  pause(): void {
    sendRequest('pause')
  },

  async resume(): Promise<void> {
    try {
      await sendRequest('resume')
    } catch (e: unknown) {
      throw new S3BgUploaderResumeError((e as Error)?.message ?? String(e))
    }
  },

  async getProgress(transferId?: string, fileKey?: string): Promise<UploadProgress[]> {
    return sendRequest('getProgress', { transferId: transferId ?? null, fileKey: fileKey ?? null }) as Promise<UploadProgress[]>
  },

  async getAggregateProgress(transferId?: string): Promise<AggregateProgress> {
    return sendRequest('getAggregateProgress', { transferId: transferId ?? null }) as Promise<AggregateProgress>
  },
}
