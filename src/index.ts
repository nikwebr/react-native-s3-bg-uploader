import { NitroModules } from 'react-native-nitro-modules'
import type { S3BgUploader as S3BgUploaderSpec } from './specs/s3-bg-uploader.nitro'
import type {
  AggregateProgress,
  ProgressCallback,
  S3BgUploaderAPI,
  UploadProgress,
  WebEncryptorOptions,
} from './specs/s3-bg-uploader.types'
import { S3BgUploaderResumeError, S3BgUploaderDuplicateFileError } from './specs/s3-bg-uploader.types'

const Native = NitroModules.createHybridObject<S3BgUploaderSpec>('S3BgUploader')

/** Progress + configuration types, so consumers need no deep import path. */
export type {
  AggregateProgress,
  GlobalUploaderState,
  ProgressCallback,
  UploadProgress,
  UploadState,
} from './specs/s3-bg-uploader.types'


export const S3BgUploader: S3BgUploaderAPI = {
  setConfig(startUploadApi, getUploadUrlsApi, completeApi, sendFileHash) {
    Native.setConfig(startUploadApi, getUploadUrlsApi, completeApi, sendFileHash)
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

  registerCustomEncryptor(vtablePointer: number) {
    Native.registerCustomEncryptor(vtablePointer)
  },

  registerWebEncryptor(_options: WebEncryptorOptions): Promise<void> {
    return Promise.reject(
      new Error(
        'S3BgUploader: registerWebEncryptor is web-only. On native platforms register a compiled encryptor with registerCustomEncryptor().',
      ),
    )
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

  async encryptAndUploadFile(file: string | File, transferId: string, encryptionKey: string, encryptionNonce: string, userParams?: Record<string, string>): Promise<string> {
    if (typeof file !== 'string') {
      throw new TypeError(
        'S3BgUploader: On native platforms, encryptAndUploadFile requires a string file path.',
      )
    }
    try {
      return await Native.encryptAndUploadFile(file, transferId, encryptionKey, encryptionNonce, userParams)
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

  getProgress(transferId?: string, fileHash?: string): UploadProgress[] {
    return Native.getProgress(transferId, fileHash)
  },

  getAggregateProgress(transferId?: string): AggregateProgress {
    return Native.getAggregateProgress(transferId)
  },
}
