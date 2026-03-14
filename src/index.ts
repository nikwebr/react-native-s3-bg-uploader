import { NitroModules } from 'react-native-nitro-modules'
import type { S3BgUploader as S3BgUploaderSpec } from './specs/s3-bg-uploader.nitro'

export const S3BgUploader =
  NitroModules.createHybridObject<S3BgUploaderSpec>('S3BgUploader')