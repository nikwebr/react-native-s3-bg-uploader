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
  /** hash of the file */
  fileHash: string
  transferId: string
  totalBytes: number
  uploadedBytes: number
  /** Count of completed part uploads for this file */
  completedParts: number
  /** Total count of parts for this file */
  totalParts: number
  /** Percentage for this individual file (0–100). */
  percentage: number
  state: UploadState
}

export interface AggregateProgress {
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
   */
  setConfig(startUploadApi: string, getUploadUrlsApi: string, completeApi: string): void

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
}

export interface S3BgUploaderAPI extends BaseUploaderAPI {
  /**
   * Enqueue a file for upload.
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
   * Returns per-file progress, optionally filtered.
   * @param transferId  Only return files in this transfer.
   * @param fileKey     Only return the entry for this fileKey.
   */
  getProgress(transferId?: string, fileKey?: string): UploadProgress[]
  
  /**
   * Returns aggregate progress for the whole session, or just one transfer.
   * @param transferId  Scope to this transfer (omits totalTransfers / completedTransfers).
   */
  getAggregateProgress(transferId?: string): AggregateProgress
}

export interface NativeS3BgUploaderAPI extends BaseUploaderAPI {
  /**
   * Enqueue a file for upload.
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
   * Returns per-file progress, optionally filtered.
   * @param transferId  Only return files in this transfer.
   * @param fileKey     Only return the entry for this fileKey.
   */
  getProgress(transferId?: string, fileKey?: string): UploadProgress[]

  /**
   * Returns aggregate progress for the whole session, or just one transfer.
   * @param transferId  Scope to this transfer (omits totalTransfers / completedTransfers).
   */
  getAggregateProgress(transferId?: string): AggregateProgress
}

export interface WebS3BgUploaderAPI extends BaseUploaderAPI {
  /**
   * Enqueue a file for upload.
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
   * Returns per-file progress, optionally filtered.
   * @param transferId  Only return files in this transfer.
   * @param fileKey     Only return the entry for this fileKey.
   */
  getProgress(transferId?: string, fileKey?: string): Promise<UploadProgress[]>

  /**
   * Returns aggregate progress for the whole session, or just one transfer.
   * @param transferId  Scope to this transfer (omits totalTransfers / completedTransfers).
   */
  getAggregateProgress(transferId?: string): Promise<AggregateProgress>
}
