export interface UploadProgress {
  totalBytes: number
  uploadedBytes: number
  completedParts: number
  totalParts: number
  percentage: number
  state: UploadState
}

export type UploadState = 'RUNNING' | 'PAUSED' | 'FINISHED' | 'FAILED'

export type ProgressCallback = (progress: UploadProgress) => void

interface BaseUploaderAPI {
  sum(num1: number, num2: number): number
  setProgressCallback(callback: ProgressCallback | null): void
}

export interface S3BgUploaderAPI extends BaseUploaderAPI {
  uploadFile(file: string | File): void
}

export interface NativeS3BgUploaderAPI extends BaseUploaderAPI {
  uploadFile(filePath: string): void
}

export interface WebS3BgUploaderAPI extends BaseUploaderAPI {
  uploadFile(file: File): Promise<void>
}
