## Custom Encryptor

AES-256-GCM custom encryptor for react-native-s3-bg-uploader, implemented in Rust.

The same Rust code runs on all three platforms:

- **iOS**: Compiled to `libcustom_encryptor.a`, linked via XCFramework
- **Android**: Compiled to `libcustom_encryptor.so` per ABI, linked via JNI
- **Web**: Compiled to WebAssembly and embedded in `example-app/web-encryptor.ts`

### Building for Web

Prerequisites: Rust toolchain with `wasm32-unknown-unknown` target, `wasm-bindgen-cli`.

```bash
cd custom-encryptor-rust

# 1. Compile to WASM
cargo build --features wasm --target wasm32-unknown-unknown --release

# 2. Generate JS glue (no-modules target for IIFE format)
wasm-bindgen --out-dir target/wasm --target no-modules \
  target/wasm32-unknown-unknown/release/custom_encryptor.wasm

# 3. Embed into web-encryptor.ts
node scripts/embed-wasm.js
```

This produces `../example-app/web-encryptor.ts` with the WASM binary and JS glue embedded as base64 strings. The factory function is self-contained and can be serialized via `.toString()` for the uploader's worker.

### Building for iOS/Android

See the main project README for native build instructions.
