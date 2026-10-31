import React from 'react';
import { Platform } from 'react-native';
import { S3BgUploader } from 'react-native-s3-bg-uploader';
import TestApp from '../../../example/e2e/TestApp';
import {
  webEncryptorFactory,
  WEB_ENCRYPTOR_OVERHEAD,
  WEB_ENCRYPTOR_HAS_FOOTER,
  WEB_ENCRYPTOR_FOOTER_LEN,
} from '../web-encryptor';

let registration: Promise<void> | undefined;

function initializeEncryptor(): Promise<void> {
  if (Platform.OS !== 'web') return Promise.resolve();
  registration ??= S3BgUploader.registerWebEncryptor({
    source: webEncryptorFactory.toString(),
    overhead: WEB_ENCRYPTOR_OVERHEAD,
    hasFooter: WEB_ENCRYPTOR_HAS_FOOTER,
    footerLen: WEB_ENCRYPTOR_FOOTER_LEN,
  });
  return registration;
}

export default function CustomTestApp(): React.JSX.Element {
  return <TestApp initializeEncryptor={initializeEncryptor} />;
}
