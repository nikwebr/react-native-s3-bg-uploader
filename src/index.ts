import { NitroModules } from 'react-native-nitro-modules'
import type { S3BgUploader as S3BgUploaderSpec } from './specs/s3-bg-uploader.nitro'

export const S3BgUploader =
  NitroModules.createHybridObject<S3BgUploaderSpec>('S3BgUploader')

/**
 * Native stub – not available on iOS/Android.
 * On web these are replaced by the real WASM implementation in index.web.ts.
 */
export async function uploadWebFile(_file: unknown): Promise<void> {
  throw new Error('uploadWebFile is only available on web.')
}

