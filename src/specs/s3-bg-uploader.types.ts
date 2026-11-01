export class S3BgUploaderResumeError extends Error {
  override readonly name = 'S3BgUploaderResumeError'
  constructor(message: string) {
    super(message)
    Object.setPrototypeOf(this, S3BgUploaderResumeError.prototype)
  }
}

export class S3BgUploaderDuplicateFileError extends Error {
  override readonly name = 'S3BgUploaderDuplicateFileError'
  constructor(message: string) {
    super(message)
    Object.setPrototypeOf(this, S3BgUploaderDuplicateFileError.prototype)
  }
}

export type UploadState = 'NOT_STARTED' | 'INITIALIZED' | 'RUNNING' | 'PAUSED' | 'COMPLETED' | 'FAILED' | 'CANCELLED'
export type GlobalUploaderState =
  | 'NOT_STARTED'
  | 'RUNNING_IN_BG'
  | 'RUNNING'
  | 'PAUSED'
  | 'COMPLETED'
  | 'FAILED'

export interface UploadProgress {
  /** uniquely identifies a file. Returned by startUpload. Is undefined until file is in `INITIALIZED` [state](https://uploader.ysendit.com/docs/api#UploadState). */
  fileKey?: string
  /** original file name */
  fileName: string
  /** hash of the **unencrypted** file content (also used for resume/dedup) */
  fileHash: string
  transferId: string
  /**
   * Size of the uploaded object in bytes. For encrypted uploads this is the
   * ciphertext size (plaintext + the scheme's per-part overhead + its descriptor),
   * not the size of the local file.
   */
  totalBytes: number
  /** Bytes of the uploaded object already stored (ciphertext for encrypted uploads). */
  uploadedBytes: number
  /** Count of completed part uploads for this file */
  completedParts: number
  /** Total count of parts for this file */
  totalParts: number
  /**
   * Percentage for this individual file (0–100). While the file is running this includes
   * the bytes of the part that is on the wire right now, so the bar moves during a part
   * upload (network progress) and not only at part boundaries. `100` is reported only
   * together with the `COMPLETED` state: a part can still fail after all of its bytes were
   * sent, and that part is then re-sent — its bytes drop back to the acknowledged level
   * and climb again with the retry, so a reported `100%` always means every part is
   * stored.
   */
  percentage: number
  state: UploadState
}

export interface AggregateProgress {
  /** `100` only while the aggregate is `COMPLETED`; running aggregates stay just below. */
  percentage: number
  totalSize: number
  uploadedSize: number
  /** Absent when scoped to a single transfer. */
  totalTransfers?: number
  /** Absent when scoped to a single transfer. */
  completedTransfers?: number
  totalFiles: number
  completedFiles: number
  state: GlobalUploaderState
}

export type ProgressCallback = (
  fileProgress: UploadProgress,
  sessionAggregate: AggregateProgress,
  /** Aggregate for the same transferId as fileProgress. */
  transferAggregate: AggregateProgress,
) => void

interface BaseUploaderAPI {
  /**
   * Resume all files that are not in `COMPLETED` [state](https://uploader.ysendit.com/docs/api#UploadState).
   * Must be called to start the upload.
   * 
   * Throws `S3BgUploaderResumeError` if any file in the session has not yet been re-provided via `uploadFile()` after a restart.
   */
  resume(): Promise<void>

  /** Pause all running uploads. */
  pause(): void

  /** Cancel a single file by its hash. */
  cancelFile(fileHash: string): void

  /** Cancel all files in a transfer. */
  cancelTransfer(transferId: string): void

  /** Cancel all files and wipe session state. After that, a file upload can not be restored */
  cancel(): void

  /**
   * Set the backend http endpoints.
   * The endpoints must accept and return the data documented [here](https://uploader.ysendit.com/docs/backend)
   * Must be called before calling `resume()`.
   *
   * @param sendFileHash  Whether `fileHash` is included in the `startUpload()` request body.
   *   Defaults to `true`. The hash is always computed locally (over the **unencrypted**
   *   file) for deduplication and resume detection, so disabling it only changes what the
   *   backend receives. Useful when the backend must not learn a content hash.
   */
  setConfig(startUploadApi: string, getUploadUrlsApi: string, completeApi: string, sendFileHash?: boolean): void

  /**
   * Set a callback that receives progress events.
   */
  setProgressCallback(callback: ProgressCallback | null): void

  /**
   * Set the notification title template.
   * On iOS this text gets clipped if too long.
   * 
   * Placeholders: `{percentage}`, `{totalSize}`, `{uploadedSize}`,
   *               `{totalTransfers}`, `{completedTransfers}`,
   *               `{totalFiles}`, `{completedFiles}`
   */
  setTaskTitle(title: string): void

  /**
   * Set the notification subtitle template.
   * On iOS this text gets clipped if too long.
   * 
   * Placeholders: `{percentage}`, `{totalSize}`, `{uploadedSize}`,
   *               `{totalTransfers}`, `{completedTransfers}`, 
   *               `{totalFiles}`, `{completedFiles}`
   */
  setTaskSubtitle(subtitle: string): void

  /**
   * Register a host-provided custom encryptor.
   * 
   * This allows a company or application to supply their own encryption
   * implementation (written in Rust) without rebuilding the nitro module.
   * The custom encryptor is registered once at application startup and
   * applies to all encrypted uploads.
   * 
   * The host app must provide a pre-compiled native library (`.a` / `.framework`)
   * that implements the stable FFI interface defined by this package.
   * 
   * @param vtablePointer  Pointer to the custom encryptor's vtable (provided by the host's native library)
   * 
   * @example
   * ```typescript
   * // In your native code (Swift / Kotlin / Rust adapter):
   * //   const vtable = new EncryptorVTable();
   * //   vtable.create = myCreate;             // creates a context from each upload's key/nonce
   * //   vtable.encrypt_chunk = myEncryptChunk;
   * //   vtable.free = myFree;
   * //   vtable.destroy = myDestroy;
   * //   vtable.ciphertext_overhead = 16;       // bytes your scheme adds per chunk
   * //   vtable.has_footer = false;             // true => compute_footer builds a descriptor
   * //   vtable.footer_len = 0;                 // bytes the descriptor occupies in the last data part
   * //   vtable.compute_footer = myDescriptor;
   * 
   * // In TypeScript at app startup:
   * S3BgUploader.registerCustomEncryptor(vtablePointer);
   * ```
   */
  registerCustomEncryptor(vtablePointer: number): void;

}

/**
 * Web-only custom encryptor registration. Kept out of `BaseUploaderAPI` because the
 * Nitro spec extends that interface: a non-primitive parameter there is turned into
 * a C++ struct by codegen and cannot be generated.
 */
export interface WebEncryptorRegistration {
  /**
   * **Web only.** Register a custom encryptor implemented in JavaScript.
   *
   * On native platforms a custom encryptor is a compiled library registered
   * through `registerCustomEncryptor()`. A browser has no second binary: the
   * uploader's WASM module runs in a worker, so the customer's mechanism is
   * JavaScript running in that same worker, registered here.
   *
   * `source` is the source text of a factory
   * `(key: Uint8Array, nonce: Uint8Array) => WebEncryptorImpl`. It is evaluated
   * inside the worker, so it must be self-contained (no imports, no closures over
   * app state). The uploader calls it once per upload with the key/nonce bytes of
   * that `encryptAndUploadFile()` call, so different files may use different keys
   * — the same contract as the native vtable's `create`. Its callbacks may be
   * synchronous or return Promises — `crypto.subtle` is async and can be used
   * directly.
   *
   * A registered web encryptor replaces the built-in cipher for files uploaded
   * with `encryptAndUploadFile()`; `uploadFile()` stays plaintext.
   *
   * The wire format is yours, but the descriptor rule from the native ABI still
   * applies: when `hasFooter` is true, `computeFooter` must return `footerLen`
   * bytes and the uploader appends them to the **last data part** (never as an
   * S3 part of its own, which S3 would reject below 5 MiB).
   *
   * Note: the source is evaluated in the worker, so a strict CSP must allow
   * `unsafe-eval` for worker scripts (the default in local development).
   *
   * @example
   * ```typescript
   * const factory = (key: Uint8Array, nonce: Uint8Array) => ({
   *   encryptChunk: async (input: Uint8Array, partNumber: number) => { ... },
   *   computeFooter: async (n: number, size: number) => { ... },
   * })
   *
   * await S3BgUploader.registerWebEncryptor({
   *   source: factory.toString(),
   *   overhead: 16,
   *   hasFooter: true,
   *   footerLen: 28,
   * })
   *
   * // Each upload supplies its own key/nonce:
   * await S3BgUploader.encryptAndUploadFile(file, transferId, keyHex, nonceHex)
   * ```
   */
  registerWebEncryptor(options: WebEncryptorOptions): Promise<void>;
}

export interface WebEncryptorImpl {
  /** Encrypt one part; `partNumber` is 1-based and stable across resumes. */
  encryptChunk: (input: Uint8Array, partNumber: number) => Uint8Array | ArrayBuffer | Promise<Uint8Array | ArrayBuffer>;
  /** Descriptor appended to the last data part. Required when `hasFooter`. */
  computeFooter?: (nDataParts: number, effectivePartSize: number) => Uint8Array | ArrayBuffer | Promise<Uint8Array | ArrayBuffer>;
}

export interface WebEncryptorOptions {
  /**
   * Source text of `(key: Uint8Array, nonce: Uint8Array) => WebEncryptorImpl`,
   * evaluated inside the uploader's worker. Called **once per upload** with that
   * upload's key material, so two files in flight may use different keys.
   * Return `null` to use the built-in cipher for that upload.
   */
  source: string;
  /** Bytes the scheme adds to every part (AEAD tag, IV prefix, …). */
  overhead: number;
  /** Whether the wire format ends with a descriptor inside the last data part. */
  hasFooter?: boolean;
  /** Bytes that descriptor occupies; required (and non-zero) when `hasFooter`. */
  footerLen?: number;
}

export interface S3BgUploaderAPI extends BaseUploaderAPI, WebEncryptorRegistration {
  /**
   * Enqueue a file for upload. Can be called in any `GlobalUploaderState`.
   *
   * throws `S3BgUploaderDuplicateFileError` if a file with the same hash is already part of the session
   * @param userParams these params are added to the `startUpload()` backend call
   * @param transferId group files in transfers to get aggregated progress reporting
   * @returns hash of the file (considers file content, size & transferId)
   */
  uploadFile(
    file: string | File,
    transferId: string,
    userParams?: Record<string, string>,
  ): Promise<string>

  /**
   * Enqueue a file for encrypted upload. Each chunk uses XChaCha20-Poly1305 AEAD
   * by default, or a registered custom encryptor.
   * The encryption is resumable: the same key and nonce must be re-provided after a restart.
   *
   * throws `S3BgUploaderDuplicateFileError` if a file with the same hash is already part of the session
   * @param encryptionKey 64 hex characters (32 bytes) encryption key for the built-in cipher
   * @param encryptionNonce 48 hex characters (24 bytes) base nonce; per-part nonce is derived deterministically
   * @param userParams these params are added to the `startUpload()` backend call
   * @param transferId group files in transfers to get aggregated progress reporting
   * @returns hash of the file (considers file content, size & transferId)
   */
  encryptAndUploadFile(
    file: string | File,
    transferId: string,
    encryptionKey: string,
    encryptionNonce: string,
    userParams?: Record<string, string>,
  ): Promise<string>

  /**
   * Returns per-file progress, optionally filtered.
   * @param transferId  Only return files in this transfer.
   * @param fileHash    Only return the entry for this file.
   */
  getProgress(transferId?: string, fileHash?: string): UploadProgress[]

  /**
   * Returns aggregate progress for the whole session, or just one transfer.
   * @param transferId  Scope to this transfer (omits totalTransfers / completedTransfers).
   */
  getAggregateProgress(transferId?: string): AggregateProgress
}

export interface NativeS3BgUploaderAPI extends BaseUploaderAPI {
  /**
   * Enqueue a file for upload. Can be called in any `GlobalUploaderState`.
   *
   * throws `S3BgUploaderDuplicateFileError` if a file with the same hash is already part of the session
   * @param userParams these params are added to the `startUpload()` backend call
   * @param transferId group files in transfers to get aggregated progress reporting
   * @returns hash of the file (considers file content, size & transferId)
   */
  uploadFile(
    filePath: string,
    transferId: string,
    userParams?: Record<string, string>,
  ): Promise<string>

  /**
   * Enqueue a file for encrypted upload. Each chunk uses XChaCha20-Poly1305 AEAD
   * by default, or a registered custom encryptor.
   * The encryption is resumable: the same key and nonce must be re-provided after a restart.
   *
   * throws `S3BgUploaderDuplicateFileError` if a file with the same hash is already part of the session
   * @param encryptionKey 64 hex characters (32 bytes) encryption key for the built-in cipher
   * @param encryptionNonce 48 hex characters (24 bytes) base nonce; per-part nonce is derived deterministically
   * @param userParams these params are added to the `startUpload()` backend call
   * @param transferId group files in transfers to get aggregated progress reporting
   * @returns hash of the file (considers file content, size & transferId)
   */
  encryptAndUploadFile(
    filePath: string,
    transferId: string,
    encryptionKey: string,
    encryptionNonce: string,
    userParams?: Record<string, string>,
  ): Promise<string>

  /**
   * Returns per-file progress, optionally filtered.
   * @param transferId  Only return files in this transfer.
   * @param fileHash    Only return the entry for this file.
   */
  getProgress(transferId?: string, fileHash?: string): UploadProgress[]

  /**
   * Returns aggregate progress for the whole session, or just one transfer.
   * @param transferId  Scope to this transfer (omits totalTransfers / completedTransfers).
   */
  getAggregateProgress(transferId?: string): AggregateProgress
}

export interface WebS3BgUploaderAPI extends BaseUploaderAPI, WebEncryptorRegistration {
  /**
   * Enqueue a file for upload. Can be called in any `GlobalUploaderState`.
   *
   * throws `S3BgUploaderDuplicateFileError` if a file with the same hash is already part of the session
   * @param userParams these params are added to the `startUpload()` backend call
   * @param transferId group files in transfers to get aggregated progress reporting
   * @returns hash of the file (considers file content, size & transferId)
   */
  uploadFile(
    file: File,
    transferId: string,
    userParams?: Record<string, string>,
  ): Promise<string>

  /**
   * Enqueue a file for encrypted upload. Each chunk uses XChaCha20-Poly1305 AEAD
   * by default, or a registered custom encryptor.
   * The encryption is resumable: the same key and nonce must be re-provided after a restart.
   *
   * throws `S3BgUploaderDuplicateFileError` if a file with the same hash is already part of the session
   * @param encryptionKey 64 hex characters (32 bytes) encryption key for the built-in cipher
   * @param encryptionNonce 48 hex characters (24 bytes) base nonce; per-part nonce is derived deterministically
   * @param userParams these params are added to the `startUpload()` backend call
   * @param transferId group files in transfers to get aggregated progress reporting
   * @returns hash of the file (considers file content, size & transferId)
   */
  encryptAndUploadFile(
    file: File,
    transferId: string,
    encryptionKey: string,
    encryptionNonce: string,
    userParams?: Record<string, string>,
  ): Promise<string>

  /**
   * Returns per-file progress, optionally filtered.
   * @param transferId  Only return files in this transfer.
   * @param fileHash    Only return the entry for this file.
   */
  getProgress(transferId?: string, fileHash?: string): Promise<UploadProgress[]>

  /**
   * Returns aggregate progress for the whole session, or just one transfer.
   * @param transferId  Scope to this transfer (omits totalTransfers / completedTransfers).
   */
  getAggregateProgress(transferId?: string): Promise<AggregateProgress>
}
