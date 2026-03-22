import { type HybridObject } from 'react-native-nitro-modules'
import type { NativeS3BgUploaderAPI } from './s3-bg-uploader.types'

export interface S3BgUploader extends HybridObject<{ ios: 'swift', android: 'kotlin' }>, NativeS3BgUploaderAPI {}
