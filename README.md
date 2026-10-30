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

## Custom encryptors

A host app can plug in its own chunk-encryption scheme without rebuilding this package, by
registering an `EncryptorVTable` (see `uploader/src/core/encryption.rs` and the generated
`ios/rust/uploader.h`). The uploader reads every field unconditionally:

| field | meaning |
|---|---|
| `create(key, key_len, nonce, nonce_len)` | build the encryptor for **one upload** from the key material of that `encryptAndUploadFile(...)` call. Return `NULL` to fall back to the built-in cipher for that upload |
| `encrypt_chunk(ctx, input, input_len, part_number)` | encrypt one S3 part and return a heap allocation in an `EncryptResult` |
| `free(ctx, data, len, capacity)` | release a result — the uploader copies the bytes out first, so the host may allocate however it likes |
| `destroy(ctx)` | release a context built by `create` — exactly once per upload, when the file's session entry is dropped (finished, cancelled or evicted) |
| `ciphertext_overhead` | bytes your scheme adds per chunk (tag, IV prefix, …); the engine reads `part_size - overhead` plaintext per part so the ciphertext fills the URL slot exactly. Use `0` for a stream cipher |
| `has_footer` | `true` if the wire format ends with a synthesised descriptor, appended to the last data part |
| `footer_len` | bytes that descriptor occupies; the engine reserves them in every part so the final part still fits its URL slot. Must be non-zero when `has_footer` is `true` |
| `compute_footer(ctx, n_data_parts, effective_part_size)` | builds that descriptor; only called when `has_footer` is `true` (Rust hosts use `no_descriptor` when it is `false`, C/C++/Swift hosts may pass `NULL`) |

The descriptor is **never an S3 part of its own** — it is appended to the final data part. S3
requires every part except the last to be at least 5 MiB, so a tiny trailing part would demote
the real last part to an intermediate one and `CompleteMultipartUpload` would fail with
`EntityTooSmall` for every file whose final part is below that minimum (which is most files).
Riding inside the last part keeps that part last, where any size is allowed.

Anything that works part-by-part is expressible: an AEAD tag (`overhead` = tag size), an IV
prefix (`overhead` = IV size), a stream cipher (`overhead = 0`), with or without a descriptor.
With `has_footer = false`, `footer_len` is `0`, `compute_footer` is never called and the engine
reserves nothing extra.

Register the factory once at startup from native code. No key material is passed there —
only the table:

```swift
CustomEncryptor.register()                                   // iOS / Swift
HybridS3BgUploader.registerCustomEncryptor(vtablePointer)    // Android / Kotlin
S3BgUploader.registerCustomEncryptor(vtablePointer)          // JS, if you prefer to register late
```

Every encrypted upload then supplies **its own** key and nonce:

```ts
await S3BgUploader.encryptAndUploadFile(path, transferId, keyHex, nonceHex)
```

The uploader decodes those hex strings and calls `create`, which builds one encryptor instance
per file. That means:

- two uploads in flight at the same time may use different keys (no global key state);
- new uploads are marked encrypted before they become schedulable or are first persisted,
  so they cannot temporarily run as plaintext during initialization;
- the instance is stored with that file's session entry, so pause/resume re-encrypts with the
  same key while the process remains alive. Keys and encryptor instances are not persisted:
  after a restart, re-provide encrypted files with `encryptAndUploadFile()` and the original
  key/nonce. Plain `uploadFile()` reprovisioning and background uploads without the required
  encryption configuration fail instead of sending plaintext;
- a scheme that keeps its key on the device (Keychain / Keystore) may ignore the bytes and
  return a context anyway. Returning `NULL` instead makes the uploader use its built-in
  XChaCha20-Poly1305 for that upload.

Plain `uploadFile(...)` stays plaintext.

#### Web

A browser has no second binary, so the mechanism is JavaScript running in the uploader's worker,
registered as a factory with the same per-upload contract:

```ts
await S3BgUploader.registerWebEncryptor({
  source: myFactory.toString(),   // (key: Uint8Array, nonce: Uint8Array) => { encryptChunk, computeFooter? }
  overhead: 16,
  hasFooter: true,
  footerLen: 28,
});

await S3BgUploader.encryptAndUploadFile(file, transferId, keyHex, nonceHex);
```

The factory is called **once per upload** with that upload's decoded key/nonce, and the instance it
returns encrypts that file only — so a browser build has the same per-upload key semantics as the
native vtable's `create`. Return `null` to fall back to the built-in cipher for that upload. The
callbacks may return Promises (`crypto.subtle` is async).

### What the ABI cannot express

`encrypt_chunk` is called once per S3 part with that part's plaintext only. The engine never
hands over the whole file: it seeks to `(part_number - 1) * part_size`, reads that one part,
and keeps at most a few parts in flight. The descriptor callback sees only `n_data_parts` and
`effective_part_size` — never the data.

So a scheme that needs a **single pass over the whole file** cannot be expressed:

- block chaining across parts (e.g. CBC/CTR state carried from part to part),
- a global trailer or digest computed over the entire plaintext,
- anything that needs the complete ciphertext before it can emit a part.

That part-independence is deliberate: every part must be re-encryptable from
`key + base_nonce + part_number` alone, which is what makes pause/resume cheap and
deterministic — a scheme carrying global state has no way to resume or retry a single part.

A worked example that also shows the linking model (the host crate must **not** depend on the
`uploader` crate — it imports `register_custom_encryptor` so the framework stays the single
copy of the uploader) lives at `custom-encryption-example/` (AES-256-GCM, descriptor in the
final part), together with `tools/decrypt` — a standalone verifier that decodes an uploaded object
from the wire format and reports which encryptor produced it.

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