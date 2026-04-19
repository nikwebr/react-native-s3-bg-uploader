export type UploadState = 'NOT_STARTED' | 'RUNNING' | 'PAUSED' | 'COMPLETED' | 'FAILED'
export type GlobalUploaderState =
  | 'NOT_STARTED'
  | 'RUNNING_IN_BG'
  | 'RUNNING'
  | 'PAUSED'
  | 'COMPLETED'
  | 'FAILED'

export interface UploadProgress {
  /** S3 key returned by uploadFile() — the public identifier for this file. */
  fileKey: string
  transferId: string
  totalBytes: number
  uploadedBytes: number
  completedParts: number
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
   * Set the three backend endpoint URLs.
   * Must be called before any uploadFile() call.
   */
  setConfig(startUploadApi: string, getUploadUrlsApi: string, completeApi: string): void

  setProgressCallback(callback: ProgressCallback | null): void

  /**
   * Set the notification title template.
   * Placeholders: {percentage} {totalSize} {uploadedSize}
   *               {totalTransfers} {completedTransfers}
   *               {totalFiles} {completedFiles}
   */
  setTaskTitle(title: string): void

  /** Same placeholders as setTaskTitle. */
  setTaskSubtitle(subtitle: string): void

  /** Cancel a single file by its S3 fileKey. */
  cancelFile(fileKey: string): void

  /** Cancel all files in a transfer. */
  cancelTransfer(transferId: string): void

  /** Cancel everything and wipe session state. */
  cancel(): void

  /** Pause all running uploads. */
  pause(): void

  /**
   * Resume all paused / failed uploads.
   * On native platforms, the caller must re-call uploadFile() with a fresh
   * file reference when the file is no longer accessible.
   */
  resume(): void

}

export interface S3BgUploaderAPI extends BaseUploaderAPI {
  /**
   * Enqueue a file for upload.
   * @returns S3 fileKey (from startUploadApi response).
   *          Returns immediately — upload runs in the background.
   *          If the same file (same SHA-256 + transferId) is already COMPLETED,
   *          the existing fileKey is returned and no upload is started.
   */
  uploadFile(
    file: string | File,
    transferId: string,
    userParams?: Record<string, string>,
  ): string
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
  uploadFile(
    filePath: string,
    transferId: string,
    userParams?: Record<string, string>,
  ): string
  getProgress(transferId?: string, fileKey?: string): UploadProgress[]
  getAggregateProgress(transferId?: string): AggregateProgress
}

export interface WebS3BgUploaderAPI extends BaseUploaderAPI {
  uploadFile(
    file: File,
    transferId: string,
    userParams?: Record<string, string>,
  ): Promise<string>
  getProgress(transferId?: string, fileKey?: string): Promise<UploadProgress[]>
  getAggregateProgress(transferId?: string): Promise<AggregateProgress>
}
