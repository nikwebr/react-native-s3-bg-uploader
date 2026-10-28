# react-native-s3-bg-uploader

[![Version](https://img.shields.io/npm/v/react-native-s3-bg-uploader.svg)](https://www.npmjs.com/package/react-native-s3-bg-uploader)
[![Downloads](https://img.shields.io/npm/dm/react-native-s3-bg-uploader.svg)](https://www.npmjs.com/package/react-native-s3-bg-uploader)
[![License](https://img.shields.io/npm/l/react-native-s3-bg-uploader.svg)](https://github.com/nikwebr/react-native-s3-bg-uploader/LICENSE)

<strong>📖 [Read the Documentation](https://uploader.ysendit.com/docs)</strong>

Seamless file uploads that continue even when your app goes to background. Pausable, resumable, and built for the S3 API. 
react-native-s3-bg-uploader is a react native package built with Nitro and Rust. It is compiled to native modules on iOS and Android, and to WebAssembly for the web. On iOS it uses a BGContinuedProcessingTask and on Android a foreground service.

<p float="left">
  <img src="example/ExampleApp.PNG" alt="isolated" width="200"/>
  <img src="example/BGContinuedProcessingTask.png" alt="isolated" width="200"/>
</p>


## Requirements

- React Native v0.76.0 or higher
- Node 18.0.0 or higher

### iOS
- background uploading is only supported for iOS 26 or later
- enabled "background fetch" & "background processing" capabilities
- registered BGTaskSchedulerPermittedIdentifiers of value $(PRODUCT_BUNDLE_IDENTIFIER).background inside info.plist

## Installation

```bash
npm install react-native-s3-bg-uploader react-native-nitro-modules
```

## Usage

```ts
import { S3BgUploader } from 'react-native-s3-bg-uploader'

// Configure backend endpoints once (e.g. in app startup)
S3BgUploader.setConfig(
  'https://api.example.com/upload/startUpload',
  'https://api.example.com/upload/getUploadUrls',
  'https://api.example.com/upload/completeUpload',
)

// Track progress
S3BgUploader.setProgressCallback((file, session, transfer) => {
  console.log(`${transfer.percentage}% — ${file.fileName}`)
})

// Enqueue files and start
const transferId = 'my-transfer'
await S3BgUploader.uploadFile('/path/to/file.mp4', transferId, { userId: '42' })
await S3BgUploader.resume()

// Pause / resume / cancel
S3BgUploader.pause()
await S3BgUploader.resume()
S3BgUploader.cancel()
```

For the full API reference see the [documentation](https://uploader.ysendit.com/docs).

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
- **iOS**: Xcode with Command Line Tools (`xcode-select --install`)
- **Android**: Android SDK with NDK — set `ANDROID_NDK_HOME` or `ANDROID_NDK_ROOT`, install [cargo-ndk](https://github.com/bbqsrc/cargo-ndk) (`cargo install cargo-ndk`)

### Running the Example App
```bash
npm install
npm run example:ios
npm run example:android
npm run example:web
```

> [!IMPORTANT]  
> Background uploads on iOS only work on a physical device as the simulator does not support the BGContinuedProcessingTask API.

> [!IMPORTANT]  
> Please do not upload confidential files. The example app is connected to a demo s3 bucket. Files are only deleted irregularly from this bucket.

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

### GitHub agent

The gh-aw workflow in `.github/workflows/agent-serviceuser.md` uses Pi with
DeepSeek, matching the ysendit agent setup. Repository writers can assign an
issue or pull request to `agent-serviceuser`, request its review, or mention
`@agent-serviceuser`. It also responds to new comments and reviews on assigned
threads. Reviews can comment or request changes, but cannot approve.

Set the repository Actions secrets `DEEPSEEK_API_KEY` and `GH_TOKEN`. The
interactive agent uses `GH_TOKEN` for creating package PRs and pushing updates
to PR branches, so their CI checks can start automatically. Using the default
`GITHUB_TOKEN` for these operations instead makes GitHub require approval before
running the resulting PR workflows. The agent does not update the external
documentation repository.

A separate workflow, `.github/workflows/post-merge-docs.md`, runs when a PR is
**merged into `master`**, not when opened or closed without merging. It uses Pi
with DeepSeek to check whether the merged changes require documentation updates
in [`nikwebr/react-native-s3-bg-uploader-docs`](https://github.com/nikwebr/react-native-s3-bg-uploader-docs).
It uses `pull_request_target` so merged fork contributions can trigger it with
the required secrets, with checkout restricted to the two trusted repositories'
`master` branches, never the PR head.
It also checks that the exported version in the docs site's `lib/version.ts`
matches the latest published stable GitHub Release, never a predicted version.
The existing release-time API-types/version sync remains unchanged; the
post-merge agent checks for missing updates without racing an unpublished bump.

The post-merge workflow opens at most one docs PR against `master`, restricted
to `content/**` and `lib/version.ts`, or does nothing when no changes are needed.
It cannot approve or merge PRs. `GH_TOKEN` must be a PAT with **Contents:
read/write** and **Pull requests: read/write** access to both the package and
docs repositories. The PAT is used by trusted checkout and publishing steps,
not either agent's execution environment or GitHub tools. Credentials are not
persisted in the agent checkout. New PRs are authored by the PAT account;
commit attribution is configured separately.

Both agents use read-only GitHub tools and apply changes through gh-aw safe
outputs. No Copilot token is required. AI threat detection remains disabled,
matching ysendit.

After editing the Markdown workflow, regenerate and commit its compiled workflow:

```bash
gh extension install github/gh-aw # Once; use v0.93.5 for this workflow
gh aw compile agent-serviceuser post-merge-docs
```

Commit both generated `.github/workflows/*.lock.yml` files and any changes to
`.github/aw/actions-lock.json` alongside the source. The previous Pi action is
archived in `old_github_workflows/pi-agent.yml` and no longer runs.
