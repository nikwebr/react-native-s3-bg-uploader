# Custom AES-256-GCM Encryption Example for react-native-s3-bg-uploader

This project demonstrates how to integrate a custom AES-256-GCM encryption library with [react-native-s3-bg-uploader](https://github.com/nikwebr/react-native-s3-bg-uploader) for multipart S3 uploads on iOS, Android, and web.

The example is a **bare React Native app**, not an Expo app. It uses the uploader from this
repository through the `file:../..` dependency in `example-app/package.json`.

## Architecture

The uploader and the custom encryptor are built separately. At startup, the app registers
an encryptor factory; each encrypted upload then supplies its own key and nonce.

| Platform | Custom encryptor build | Registration |
|---|---|---|
| iOS | Static libraries packaged as an XCFramework | `AppDelegate` calls `CustomEncryptor.register()` |
| Android | `libcustom_encryptor.so` for each ABI | `MainApplication` calls `HybridS3BgUploader.registerCustomEncryptorIfAvailable()` |
| Web | Separate WASM module embedded in `web-encryptor.ts` | `App.tsx` calls `registerWebEncryptor()` inside the uploader's worker |

## Project Structure

```
custom-encryption-example/
├── custom-encryptor-rust/          # Custom Rust encryption library
│   ├── Cargo.toml
│   ├── src/
│   │   └── lib.rs                  # AES-256-GCM implementation
│   ├── ios/
│   │   ├── CustomEncryptor.swift   # Swift FFI bridge
│   │   └── CustomEncryptor.podspec # CocoaPods spec
│   ├── scripts/embed-wasm.js      # Generates the browser encryptor
│   └── target/                    # Generated native libraries and WASM
├── example-app/                    # React Native example app
│   ├── ios/
│   │   ├── CustomEncryptionExample.xcodeproj/
│   │   ├── CustomEncryptionExample.xcworkspace/
│   │   ├── CustomEncryptor.podspec
│   │   ├── Podfile
│   │   └── Libraries/
│   │       └── libcustom_encryptor.xcframework/  # Generated
│   ├── android/app/src/main/jniLibs/ # Generated custom .so files per ABI
│   ├── web-encryptor.ts            # Embedded custom WASM
│   └── App.tsx                     # Shared native/web UI
├── build-rust-lib.sh               # Builds the custom iOS XCFramework
├── tools/decrypt/                  # Independent object verifier
└── README.md
```

## Build and run

### Common setup

Install Node.js **22.11.0 or newer**, npm, and Rust with `cargo` and `rustup`.
Native builds additionally need the platform tools described below.

Start in the **repository root**, not in `custom-encryption-example/`:

```bash
npm ci
npm ci --prefix custom-encryption-example/example-app
```

The root install provides the uploader's build/code-generation tools; the second install
provides this example app's dependencies. Build both the uploader and the custom encryptor
for the platform you want to run.

Before running the example app, review `START_UPLOAD_API`, `GET_UPLOAD_URLS_API`, and `COMPLETE_API` in
`example-app/App.tsx`. Replace them with your backend endpoints if needed.

> **Demo only:** the app includes demo key/nonce values and points to a demo backend.
> Do not upload confidential files. In production, generate and retain your own key material,
> use a unique nonce for each new upload under a key, and reuse the original key/nonce only
> when retrying or resuming that upload.

### iOS

Requires macOS, Xcode with an iOS 26-or-newer SDK, Xcode Command Line Tools, and CocoaPods.
The example's `Podfile` targets **iOS 26.0**. Use an iOS 26-or-newer simulator or device;
background continued-processing uploads require a **physical device**.

From the repository root:

```bash
npm run build:ios
bash custom-encryption-example/build-rust-lib.sh
(cd custom-encryption-example/example-app/ios && pod install)
```

The first command builds the uploader and runs native code generation. The second builds
the custom encryptor for device and simulator and creates
`example-app/ios/Libraries/libcustom_encryptor.xcframework`, which the podspec consumes.
There is no need to swap `.a` files manually when switching between simulator and device.

For a **Debug simulator run**, start Metro in one terminal:

```bash
cd custom-encryption-example/example-app
npm start
```

In another terminal, starting from the repository root:

```bash
xcrun simctl list devices available
cd custom-encryption-example/example-app
# Replace the simulator name with one installed on your machine.
npm run ios -- --scheme CustomEncryptionExample --simulator "iPhone 17" --no-packager
```

For a **physical device**, connect and trust the iPhone, enable Developer Mode, then open
the workspace from the repository root:

```bash
open custom-encryption-example/example-app/ios/CustomEncryptionExample.xcworkspace
```

Select the `CustomEncryptionExample` scheme, set your development team under
**Signing & Capabilities**, select the connected device, and choose **Run**.
Debug builds need Metro reachable from the device.

Alternatively, after configuring signing, build, install, and launch a **Release** build:

```bash
cd custom-encryption-example/example-app
npm run ios -- --scheme CustomEncryptionExample --device "Your iPhone" --mode Release --no-packager
```

Replace `Your iPhone` with the connected device's name. Release bundles JavaScript and
does not require Metro.

### Android

Requires Android Studio/SDK, a compatible JDK configured through `JAVA_HOME`, SDK Platform
**36**, Build Tools **36.0.0**, and NDK **27.1.12297006**. The app supports Android API **24+**.
Install these through Android Studio's SDK Manager and enable USB debugging on a device,
or start an emulator.

```bash
# Uploader .so files for all four configured ABIs, plus native code generation.
npm run build:android

# Custom encryptor .so files for those same ABIs.
cargo ndk --platform 24 \
  -t arm64-v8a -t armeabi-v7a -t x86_64 -t x86 \
  -o custom-encryption-example/example-app/android/app/src/main/jniLibs \
  build --release \
  --manifest-path custom-encryption-example/custom-encryptor-rust/Cargo.toml
```

The uploader libraries go into the repository's `android/src/main/jniLibs/`; the custom
libraries go into the example app's `jniLibs/`. Both are required. The app already contains
the native registration call; no extra Kotlin file or CMake linking changes are needed.

For a **Debug run**, start Metro in one terminal:

```bash
cd custom-encryption-example/example-app
npm start
```

In another terminal, starting from the repository root, connect one device or emulator:

```bash
adb devices
adb reverse tcp:8081 tcp:8081
cd custom-encryption-example/example-app
npm run android -- --no-packager
```

For a **Release run without Metro**, start from the repository root:

```bash
cd custom-encryption-example/example-app/android
./gradlew :app:assembleRelease --no-daemon
adb install -r app/build/outputs/apk/release/app-release.apk
adb shell am start -n com.customencryptionexample/.MainActivity
```

The example signs Release with its debug keystore for local use. Configure production
signing separately before distributing an app.

### Web

Web needs neither Xcode/Android tooling nor Metro. It needs two WASM builds: the uploader
and the custom AES encryptor.

From the repository root:

```bash
npm run build:wasm

cd custom-encryption-example/custom-encryptor-rust
cargo build --locked --features wasm --target wasm32-unknown-unknown --release
wasm-bindgen --out-dir target/wasm --target no-modules \
  target/wasm32-unknown-unknown/release/custom_encryptor.wasm
node scripts/embed-wasm.js

cd ../example-app
npm run web
```

Open **http://localhost:8082**. The uploader build regenerates the repository's
`src/web/wasm-assets.ts`; the custom build regenerates `example-app/web-encryptor.ts`.
The app registers the custom factory automatically. Wait for the
`web: custom AES-256-GCM encryptor registered (WASM)` log before starting an encrypted upload.

`npm run build:wasm` installs a `wasm-bindgen-cli` version matching the uploader's lockfile.
The custom encryptor's CLI must also match its own `Cargo.lock` (currently **0.2.129**,
the same as the uploader). If those versions diverge, install the matching CLI before
processing the custom module, for example:

```bash
cargo install wasm-bindgen-cli --version 0.2.129 --locked --force
```

Your API and S3 bucket must allow browser requests from `http://localhost:8082` through
CORS, including the upload methods/headers and access to the response's `ETag` header.
Keep localhost for local development, or use HTTPS when hosting the app.

For a production web bundle, run this from `custom-encryption-example/example-app`:

```bash
npx webpack --config webpack.config.js --mode production
```

Serve the resulting `web-build/` directory over HTTP(S); do not open its HTML through
`file://`.

### Using the running app

Enable custom encryption, set the key and nonce, choose files, and start/resume the upload.
`encryptAndUploadFile()` requires a **64-hex-character key** and a
**48-hex-character base nonce** in this example. Native registration and web factory
registration do not contain the key; it is supplied with each upload.
Use the verifier below to authenticate and decrypt a downloaded object.

## Key Implementation Details

### Rust custom encryptor (`custom-encryptor-rust/src/lib.rs`)

AES-256-GCM chunk encryption, deterministic per part so a paused upload can be resumed
and a part re-encrypted to exactly the same ciphertext:

- **Key/nonce**: 32-byte key + 24-byte base nonce, passed as hex with each encrypted upload.
- **Per-part nonce**: `base_nonce[..12] XOR part_number` (u32 little-endian) — no random
  IVs, which is what makes resume/retry reproducible.
- **Overhead**: 16 bytes (the GCM tag), declared as `ciphertext_overhead = 16`.
- **Descriptor**: 28 bytes (12 plaintext + tag), declared as `footer_len = 28`. It is the
  encrypted `n_data_parts (4B LE) || effective_part_size (8B LE)`, built by
  `custom_compute_footer` with `part_number = n_data_parts + 1`, and it is **appended to the
  final data part** — never an S3 part of its own. S3 requires every part except the last to be
  at least 5 MiB, so a separate 28-byte trailing part demotes the real last part and
  `CompleteMultipartUpload` fails with `EntityTooSmall` — that is exactly how encrypted uploads
  used to fail. Each plaintext chunk is therefore `overhead + footer_len` smaller than its S3
  slot, and the engine reserves both.
- **Whole-object layout**: parts `1..n-1` are the ciphertext of `part_size - 44` plaintext bytes
  each; part `n` is the ciphertext of the last chunk **followed by** the 28-byte descriptor. A
  decoder needs nothing but the key/nonce: the descriptor is the object's last 28 bytes and
  states both `n` and the chunk size.
- **Ownership**: the host allocates the ciphertext; the uploader copies the bytes out and
  then calls the vtable's `free`. The ABI therefore does not assume a shared allocator —
  a C/C++/Swift host can allocate however it likes.

### The vtable ABI

`EncryptorVTable` is the whole interface — the uploader reads every field unconditionally:

| field | meaning |
|---|---|
| `create(key, key_len, nonce, nonce_len)` | builds one context from an upload's key material |
| `encrypt_chunk(ctx, input, len, part_number)` | encrypt one S3 part |
| `free(ctx, data, len, capacity)` | release a result the uploader copied out |
| `destroy(ctx)` | release the context |
| `ciphertext_overhead` | bytes added per chunk (tag/IV/…); `0` for a stream cipher |
| `has_footer` | `true` if the object ends with a descriptor (appended to the last data part) |
| `footer_len` | bytes that descriptor occupies (28 here); reserved in every part |
| `compute_footer(ctx, n_data_parts, effective_part_size)` | builds that descriptor |

That covers any scheme that works part-by-part: AEAD with a tag (`overhead` = tag size), a
scheme with an IV prefix (`overhead` = IV size), a stream cipher needing no per-chunk bytes
(`overhead = 0`) — with or without a descriptor. When `has_footer` is `false`, `footer_len` is
`0`, `compute_footer` is never called and the engine reserves nothing extra.

It cannot express a scheme that needs a **single pass over the whole file**: `encrypt_chunk`
receives one part at a time and the descriptor callback sees only `n_data_parts` and
`effective_part_size`, never the data. Chaining state across parts (CBC/CTR), a global digest
over the whole plaintext, or anything that needs the full ciphertext before emitting a part is
out of scope. That part-independence is what keeps resume cheap: any part can be re-encrypted
from `key + base_nonce + part_number` alone.

### Linking model

`libcustom_encryptor.a` contains only the AES-GCM code. It declares the uploader's
`register_custom_encryptor` as an `extern "C"` **import**, so it resolves from
`libuploader.framework` at link time: the app holds exactly one copy of the uploader, and
library updates arrive by relinking.

Do **not** add an `uploader = { path = ... }` dependency: that statically embeds a second,
older copy of the whole uploader, and a static definition beats a dylib — the embedded copy
then shadows `libuploader.framework` and library fixes never take effect.

### Registration

iOS — `example-app/ios/CustomEncryptionExample/AppDelegate.swift`:

```swift
CustomEncryptor.register()   // factory only — no key material
```

This calls the C entry point `custom_encryptor_register`, which registers the
`EncryptorVTable`. It must run before the first upload. The key and nonce are **not** stored
or hardcoded anywhere native: every `encryptAndUploadFile(file, transferId, keyHex, nonceHex)`
call passes its own, and the uploader hands those bytes to the table's `create` callback, which
returns one context per upload. Two uploads may therefore use different keys.

Android registers the same table from `MainApplication` via
`HybridS3BgUploader.registerCustomEncryptorIfAvailable()`, which loads `libcustom_encryptor.so`,
reads its vtable pointer and registers it — again without any key.

(`ios/CustomEncryptor.podspec` compiles the Swift glue — keep its `source_files` path pointing
at the real file, or the pod silently compiles nothing.)

Android — call the equivalent from `MainApplication`/the host's startup path.

## Verifying an uploaded object

`tools/decrypt` is a standalone verifier (its own crate; only `aes-gcm` + `chacha20poly1305`)
that decodes an object the way a customer's own decoder would: it re-implements the wire format
— nonce derivation, part splitting, descriptor — instead of calling this example's code, so it
cannot share a bug with it.

```bash
cd tools/decrypt
# auto-detects the scheme and part size, writes <object>.decrypted and compares
cargo run --release -- ~/Downloads/uploaded.bin --expect ~/path/to/original.bin
```

```
object      : uploaded.bin (2101461 bytes)
scheme      : aesgcm (custom encryptor)
part size   : 1048548 — effective plaintext per part 1048532
data parts  : 3 (last part 4337 ct bytes)
descriptor  : part 4 of 28 bytes -> n_data_parts=3, effective_part_size=1048532
authenticated: 3 data parts + descriptor
wrote       : uploaded.bin.decrypted (2101385 bytes)
expect      : MATCH (2101385 bytes, byte-identical)
verdict     : OK — the object was encrypted with aesgcm (custom encryptor) and this key/nonce
```

How to read the result:

- **`scheme: aesgcm (custom encryptor)` + `OK`** — the custom encryptor really ran. Every part
  and the descriptor passed AEAD verification, so those bytes cannot have been produced by any
  other scheme or key.
- **Exit code** `0` = verified, `1` = nothing authenticated, `2` = usage/IO error.
- It also tells you *which* implementation produced the object: with `--scheme auto` (the
  default), a report of `xchacha (built-in)` means the upload fell back to the built-in
  encryptor — i.e. registration never happened.
- `--key` / `--nonce` default to the demo values from `example-app/App.tsx`. The layout — part size
  *and* part count — is read out of the descriptor, so `--part-size` is only needed to force a
  specific split while debugging: the engine's real part size is `server_part_size - 44`, which
  is not a round number.
- A plain (unencrypted) upload authenticates nothing — the message says so, and you can compare
  the object bytes with the original directly.

Sanity-check the tool itself before trusting it on a real object:

```bash
cargo run --release -- --make-fixture /tmp/vec          # AES-GCM + XChaCha vectors + plaintext
cargo run --release -- /tmp/vec/object-aesgcm.bin --expect /tmp/vec/plaintext.bin
```

Or have the example's own Rust crate emit a fixture built by its real encryption code:

```bash
cd custom-encryptor-rust
CUSTOM_ENCRYPTOR_FIXTURE_DIR=/tmp/cfv cargo test --lib write_decrypt_fixture -- --nocapture
cd ../tools/decrypt
cargo run --release -- /tmp/cfv/adapter-object.bin --expect /tmp/cfv/adapter-plaintext.bin
```

## Custom Encryption Flow

1. **Startup**: native code registers the encryptor (`custom_encryptor_register` →
   `register_custom_encryptor`), which the uploader stores process-wide.
2. **Upload**: the app calls `encryptAndUploadFile(file, transferId, keyHex, nonceHex)`. The
   uploader decodes the hex key/nonce and calls the registered `create` callback, which builds
   the AES-256-GCM context for that file — so the key is per upload, not per process. The call
   is also what marks the file as encrypted; plain `uploadFile()` stays plaintext and is never
   touched by a registered scheme.
3. **Layout**: because the registered encryptor declares `overhead = 16`, `has_footer = true`
   and `footer_len = 28`, the engine sizes each plaintext chunk as `part_size - 44` and appends
   the descriptor to the final part — no extra S3 part exists.
4. **Per part**: `read_chunk` asks the registered encryptor to encrypt the plaintext; for the
   last part it then appends `compute_footer(total_data_parts, effective_part_size)`.
5. **Complete**: the parts are committed; the object is self-describing (key + nonce suffice
   to decode).

## Troubleshooting

### Library changes have no effect (stale/duplicate uploader)
- **Cause**: the custom-encryptor crate depends on `uploader` (rlib) and re-exports
  `register_custom_encryptor`, so a second, frozen copy of the uploader gets statically linked
  and shadows `libuploader.framework`.
- **Fix**: no `uploader` dependency; declare `register_custom_encryptor` as `extern "C"`.
  Check with `nm -u <App>.debug.dylib | grep _register_custom_encryptor` — it must be
  **undefined** (imported), and `nm -gU <App>.debug.dylib` must not define `_pause_all`.

### Custom encryptor has no effect
- **Cause**: registration never ran — usually the podspec's `source_files` didn't match the
  Swift file (so the glue was never compiled), or nothing called it at startup.
- **Fix**: `source_files` must point at the real path; call `CustomEncryptor.register()` in
  `AppDelegate`. Verify `_OBJC_CLASS_$_CustomEncryptor` is present in the app binary.

### Architecture Mismatch: `built for 'iOS'` vs `iOS-simulator`
- **Cause**: Device library used for simulator build (or vice versa)
- **Fix**: Build all three targets and create universal library for simulator

### Simulator Install Fails with Exit Code 137
- **Cause**: Simulator process crash or resource limits
- **Fix**: Restart simulator, ensure sufficient disk space

## Dependencies

- **Rust**: `aes-gcm`, `getrandom`; `wasm-bindgen` and `js-sys` for web
- **React Native**: 0.85.3+ with Nitro modules
- **iOS example app**: iOS 26.0+ (arm64 device + arm64/x86_64 simulator)