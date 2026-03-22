export interface UploadProgress {
  totalBytes: number
  uploadedBytes: number
  completedParts: number
  totalParts: number
  percentage: number
}

export type ProgressCallback = (progress: UploadProgress) => void

export interface S3BgUploaderAPI {
  sum(num1: number, num2: number): number
  uploadFile(filePath: string): Promise<void>
  setProgressCallback(callback: ProgressCallback | null): void
}
