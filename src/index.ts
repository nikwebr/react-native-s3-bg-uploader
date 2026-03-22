import { NitroModules } from 'react-native-nitro-modules'
import type { S3BgUploader as S3BgUploaderSpec } from './specs/s3-bg-uploader.nitro'
import type { ProgressCallback, S3BgUploaderAPI } from './specs/s3-bg-uploader.types'

const NativeS3BgUploader = NitroModules.createHybridObject<S3BgUploaderSpec>('S3BgUploader')

export const S3BgUploader: S3BgUploaderAPI = {
  sum(num1, num2) {
      return num1 + num2;
  },
  setProgressCallback(callback: ProgressCallback | null): void {
    NativeS3BgUploader.setProgressCallback(callback)
  },
  async uploadFile(file: string | File): Promise<void> {
    if (typeof file !== 'string') {
      throw new TypeError('S3BgUploader: On native platforms, uploadFile requires a stringish file path value as file paramenter.')
    }
    return NativeS3BgUploader.uploadFile(file)
  }
}

