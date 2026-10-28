# Custom AES-256-GCM Encryption Example for react-native-s3-bg-uploader

This project demonstrates how to integrate a custom AES-256-GCM encryption library with [react-native-s3-bg-uploader](https://github.com/Naturalclar/react-native-s3-bg-uploader) for secure multipart S3 uploads in React Native.

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│  React Native App (Example App)                                │
│  - CustomEncryptor.swift (Swift bridge)                        │
│  - CustomEncryptor.podspec (CocoaPods)                         │
└──────────────────────┬────────────────────────────────────────┘
                       │ FFI calls
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│  custom-encryptor (Rust)                                       │
│  - lib.rs: AES-256-GCM chunk encryption, part numbering        │
│  - iOS/CustomEncryptor.swift: Swift FFI bridge                 │
│  - custom_encryptor_register(): VTable registration            │
└──────────────────────┬────────────────────────────────────────┘
                       │ calls into
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│  uploader (Rust) - from s3-bg-uploader                         │
│  - core/encryption.rs: EncryptorVTable definition              │
│  - core/chunked.rs: Multipart upload logic                     │
│  - ios/bindings.rs: register_custom_encryptor()                │
└─────────────────────────────────────────────────────────────────┘
```

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
│   └── target/                     # Built libraries
│       ├── aarch64-apple-ios/      # Device static lib
│       ├── aarch64-apple-ios-sim/  # Simulator (arm64)
│       ├── x86_64-apple-ios/       # Simulator (x86_64)
│       └── universal-ios-sim/      # Fat simulator lib
├── example-app/                    # React Native example app
│   ├── ios/
│   │   ├── CustomEncryptionExample.xcodeproj/
│   │   ├── CustomEncryptionExample.xcworkspace/
│   │   ├── CustomEncryptor.podspec
│   │   ├── Podfile
│   │   └── Libraries/
│   │       └── libcustom_encryptor.a  # Copied static lib
│   └── App.tsx                     # React Native UI
└── README.md
```

## Building the Custom Encryptor

### Prerequisites
- Rust toolchain with `cargo`
- iOS targets installed: `rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios`

### Build Commands

```bash
cd custom-encryptor-rust

# Build for all iOS targets (device + simulator)
cargo build --release --target aarch64-apple-ios
cargo build --release --target aarch64-apple-ios-sim
cargo build --release --target x86_64-apple-ios

# Create universal simulator library
mkdir -p target/universal-ios-sim/release
lipo -create \
    target/aarch64-apple-ios-sim/release/libcustom_encryptor.a \
    target/x86_64-apple-ios/release/libcustom_encryptor.a \
    -output target/universal-ios-sim/release/libcustom_encryptor.a

# Copy to example app
cp target/universal-ios-sim/release/libcustom_encryptor.a \
   ../example-app/ios/Libraries/
```

### For Device Builds

```bash
# Use the device-specific library
cp target/aarch64-apple-ios/release/libcustom_encryptor.a \
   ../example-app/ios/Libraries/
```

## Building the Example App

### 1. Install Dependencies

```bash
cd example-app
npm install
cd ios
pod install
```

### 2. Build with Xcode

```bash
cd example-app/ios

# Debug (simulator) - uses universal simulator library
xcodebuild -scheme CustomEncryptionExample \
    -workspace CustomEncryptionExample.xcworkspace \
    -destination 'platform=iOS Simulator,name=iPhone 17' \
    -configuration Debug build

# Release (device) - uses device library
xcodebuild -scheme CustomEncryptionExample \
    -workspace CustomEncryptionExample.xcworkspace \
    -destination 'platform=iOS,name=iPhone 17' \
    -configuration Release build
```

## Key Implementation Details

### Rust custom encryptor (`custom-encryptor-rust/src/lib.rs`)

AES-256-GCM chunk encryption, deterministic per part so a paused upload can be resumed
and a part re-encrypted to exactly the same ciphertext:

- **Key/nonce**: 32-byte key + 24-byte base nonce, passed as hex at registration.
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
- `--key` / `--nonce` default to the demo values from `AppDelegate`. The layout — part size
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

- **Rust**: `aes-gcm`, `sha2`, `hkdf`, `rand`, `generic-array`
- **React Native**: 0.85.3+ with Nitro modules
- **iOS**: iOS 15.0+ (arm64 device + arm64/x86_64 simulator)