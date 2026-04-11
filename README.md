# react-native-s3-bg-uploader

react-native-s3-bg-uploader is a react native package built with Nitro

[![Version](https://img.shields.io/npm/v/react-native-s3-bg-uploader.svg)](https://www.npmjs.com/package/react-native-s3-bg-uploader)
[![Downloads](https://img.shields.io/npm/dm/react-native-s3-bg-uploader.svg)](https://www.npmjs.com/package/react-native-s3-bg-uploader)
[![License](https://img.shields.io/npm/l/react-native-s3-bg-uploader.svg)](https://github.com/patrickkabwe/react-native-s3-bg-uploader/LICENSE)

## Requirements

- React Native v0.76.0 or higher
- Node 18.0.0 or higher

> [!IMPORTANT]  
> To Support `Nitro Views` you need to install React Native version v0.78.0 or higher.

### iOS
- background uploading is only supported for iOS 26 or later
- enabled "background fetch" & "background processing" capabilities
- registered BGTaskSchedulerPermittedIdentifiers of value $(PRODUCT_BUNDLE_IDENTIFIER).background inside info.plist

## Installation

```bash
npm install react-native-s3-bg-uploader react-native-nitro-modules
```

## Building
For native platforms:
```bash
npm run build:ios
npm run build:android
```

For web:
```bash
npm run build:wasm
```

### Additional requirements
- [rustup](https://rust-lang.org/tools/install/)

### Example
```bash
npm run example:ios
npm run example:android
npm run example:web
```

> [!IMPORTANT]  
> The iOS example app does only work on a physical device and does not work on the simulator because the simulator does not support BGContinuedProcessingTask

### Production Builds
#### Android
Follow the instructions [here](https://reactnative.dev/docs/signed-apk-android#generating-an-upload-key) to generate an upload key and to set required gradle variables.

After that:
```bash
cd example
npx react-native build-android --mode=release
npm run android -- --mode="release"
```

#### iOS
Change build configuration to production inside XCode: Open S3BackgroundUploaderExample.xcworkspace and edit the scheme.

## Troubleshooting
### ReferenceError: Can't find variable: wasm_bindgen
Run in the root of the repo
```bash
npm run build:rust:wasm
```

## Credits

Bootstrapped with [create-nitro-module](https://github.com/patrickkabwe/create-nitro-module).

## Contributing

Pull requests are welcome. For major changes, please open an issue first to discuss what you would like to change.
