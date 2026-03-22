/// <reference lib="dom" />
import type { S3BgUploaderAPI, ProgressCallback } from './specs/s3-bg-uploader.types'
import { UPLOADER_JS_CODE, UPLOADER_WASM_BASE64 } from './web/wasm-assets'

// ---------------------------------------------------------------------------
// Inline worker source
// Both the wasm_bindgen JS glue and the WASM binary are embedded at build
// time by uploader/scripts/embed-wasm.js — no importScripts, no external URLs, no
// bundler configuration required by consumers.
// ---------------------------------------------------------------------------

function buildWorkerSource(jsCode: string, wasmBase64: string): string {
  return `
/* Inline wasm_bindgen JS bindings — no importScripts needed */
${jsCode}

/* Initialize WASM from embedded base64 data */
const _wasmBase64 = ${JSON.stringify(wasmBase64)};
const _wasmBytes = Uint8Array.from(atob(_wasmBase64), function(c) { return c.charCodeAt(0); });
const wasmReady = wasm_bindgen(_wasmBytes.buffer);

onmessage = async function (e) {
  await wasmReady;
  const { upload_file, set_progress_callback } = wasm_bindgen;

  set_progress_callback((progress) => {
    postMessage({
      type: 'progress',
      data: {
        totalBytes: Number(progress.totalBytes),
        uploadedBytes: Number(progress.uploadedBytes),
        completedParts: Number(progress.completedParts),
        totalParts: Number(progress.totalParts),
        percentage: Number(progress.percentage),
      },
    });
  });

  try {
    await upload_file(e.data.file);
    postMessage({ type: 'complete', success: true });
  } catch (error) {
    postMessage({ type: 'error', message: error.toString() });
  } finally {
    set_progress_callback(null);
  }
};
`
}

// ---------------------------------------------------------------------------
// Worker singleton
// ---------------------------------------------------------------------------

let worker: Worker | null = null
let progressCallback: ProgressCallback | null = null

function getWorker(): Worker {
  if (!worker) {
    const src = buildWorkerSource(UPLOADER_JS_CODE, UPLOADER_WASM_BASE64)
    const blob = new Blob([src], { type: 'application/javascript' })
    const url = URL.createObjectURL(blob)
    worker = new Worker(url)
    URL.revokeObjectURL(url) // safe to revoke after Worker is created
    worker.onmessage = (e) => {
      if (e.data.type === 'progress' && progressCallback) {
        progressCallback(e.data.data)
      }
    }
  }
  return worker
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

const S3BgUploaderWeb: S3BgUploaderAPI = {
  sum(num1: number, num2: number): number {
    return num1 + num2
  },

  async uploadFile(_filePath: string): Promise<void> {
    throw new Error('On web, use the exported uploadWebFile(file) helper instead of uploadFile().')
  },

  setProgressCallback(callback: ProgressCallback | null): void {
    progressCallback = callback
  },
}

export const S3BgUploader = S3BgUploaderWeb

/**
 * Upload a web File object via the WASM Web Worker.
 * Use this on web instead of S3BgUploader.uploadFile().
 */
export async function uploadWebFile(file: File): Promise<void> {
  const w = getWorker()
  return new Promise((resolve, reject) => {
    const onMessage = (e: MessageEvent) => {
      if (e.data.type === 'complete') {
        w.removeEventListener('message', onMessage)
        resolve()
      } else if (e.data.type === 'error') {
        w.removeEventListener('message', onMessage)
        reject(new Error(e.data.message))
      }
    }
    w.addEventListener('message', onMessage)
    w.postMessage({ file })
  })
}
