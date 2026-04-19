import { NitroModules } from 'react-native-nitro-modules'
import type { S3BgUploader as S3BgUploaderSpec } from './specs/s3-bg-uploader.nitro'
import type {
  AggregateProgress,
  ProgressCallback,
  S3BgUploaderAPI,
  UploadProgress,
} from './specs/s3-bg-uploader.types'

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

  uploadFile(file: string | File, transferId: string, userParams?: Record<string, string>): string {
    if (typeof file !== 'string') {
      throw new TypeError(
        'S3BgUploader: On native platforms, uploadFile requires a string file path.',
      )
    }
    return Native.uploadFile(file, transferId, userParams)
  },

  cancelFile(fileKey: string) {
    Native.cancelFile(fileKey)
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

  resume() {
    Native.resume()
  },

  getProgress(transferId?: string, fileKey?: string): UploadProgress[] {
    return Native.getProgress(transferId, fileKey)
  },

  getAggregateProgress(transferId?: string): AggregateProgress {
    return Native.getAggregateProgress(transferId)
  },
}
