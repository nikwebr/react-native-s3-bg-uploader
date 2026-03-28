/// <reference lib="dom" />
import type { ProgressCallback, S3BgUploaderAPI } from './specs/s3-bg-uploader.types'
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
        state: progress.state
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

export const S3BgUploader: S3BgUploaderAPI = {
  sum(num1: number, num2: number): number {
    return num1 + num2
  },

  uploadFile(file: File | string) {
    if (typeof file === 'string') {
      throw new TypeError('S3BgUploader: On web platform, uploadFile requires a File value as file parameter.')
    }
    const w = getWorker()
    w.postMessage({ file })
  },

  setProgressCallback(callback: ProgressCallback | null): void {
    progressCallback = callback
  },
}