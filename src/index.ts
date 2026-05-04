import { NitroModules } from 'react-native-nitro-modules'
import type { S3BgUploader as S3BgUploaderSpec } from './specs/s3-bg-uploader.nitro'
import type {
  AggregateProgress,
  ProgressCallback,
  S3BgUploaderAPI,
  UploadProgress,
} from './specs/s3-bg-uploader.types'
import { S3BgUploaderResumeError, S3BgUploaderDuplicateFileError } from './specs/s3-bg-uploader.types'

const Native = NitroModules.createHybridObject<S3BgUploaderSpec>('S3BgUploader')

export const S3BgUploader: S3BgUploaderAPI = {
  setConfig(startUploadApi, getUploadUrlsApi, completeApi) {
    Native.setConfig(startUploadApi, getUploadUrlsApi, completeApi)
  },

  setProgressCallback(callback: ProgressCallback | null) {
    Native.setProgressCallback(callback)
  },

  setTaskTitle(title: string) {
    Native.setTaskTitle(title)
  },

  setTaskSubtitle(subtitle: string) {
    Native.setTaskSubtitle(subtitle)
  },

  async uploadFile(file: string | File, transferId: string, userParams?: Record<string, string>): Promise<string> {
    if (typeof file !== 'string') {
      throw new TypeError(
        'S3BgUploader: On native platforms, uploadFile requires a string file path.',
      )
    }
    try {
      return await Native.uploadFile(file, transferId, userParams)
    } catch (e: unknown) {
      const msg = (e as Error)?.message ?? String(e)
      if (msg.includes('DUPLICATE_FILE')) throw new S3BgUploaderDuplicateFileError(msg)
      throw e
    }
  },

  cancelFile(fileHash: string) {
    Native.cancelFile(fileHash)
  },

  cancelTransfer(transferId: string) {
    Native.cancelTransfer(transferId)
  },

  cancel() {
    Native.cancel()
  },

  pause() {
    Native.pause()
  },

  async resume(): Promise<void> {
    try {
      await Native.resume()
    } catch (e: unknown) {
      throw new S3BgUploaderResumeError((e as Error)?.message ?? String(e))
    }
  },

  getProgress(transferId?: string, fileKey?: string): UploadProgress[] {
    return Native.getProgress(transferId, fileKey)
  },

  getAggregateProgress(transferId?: string): AggregateProgress {
    return Native.getAggregateProgress(transferId)
  },
}
