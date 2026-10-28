# Custom Encryption Example for react-native-s3-bg-uploader

This example demonstrates how to use a **custom AES-256-GCM encryptor** with `react-native-s3-bg-uploader`. The encryption runs entirely in Rust (on iOS, Android, and Web/WASM) without requiring a rebuild of the core nitro module.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  React Native App (TypeScript)                              │
│                                                             │
│  S3BgUploader.encryptAndUploadFile(file, tid, key, nonce)   │
└─────────────────────────────┬───────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│  Platform-Specific Registration                             │
│                                                             │
│  iOS:  CustomEncryptor.register(keyHex, nonceHex)           │
│  Android: CustomEncryptor.register(keyHex, nonceHex)        │
│  Web:    Statically linked into WASM (no registration)      │
└─────────────────────────────┬───────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│  Custom Encryptor (Rust)                                    │
│                                                             │
│  AES-256-GCM implementation:                                │
│  • Confidentiality: AES-256 cipher                          │
│  • Integrity: 128-bit GCM authentication tag                │
│  • Deterministic: Same key + nonce + data → same ciphertext │
└─────────────────────────────┬───────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│  S3BgUploader (Rust/Nitro)                                  │
│                                                             │
│  • Reads file chunks                                        │
│  • Encrypts each chunk with custom AES-256-GCM              │
│  • Uploads encrypted chunks to S3                           │
└─────────────────────────────────────────────────────────────┘
```

## Quick Start

### 1. Build the Custom Encryptor Library

```bash
# Navigate to the custom encryptor directory
cd custom-encryptor-rust

# Build for iOS (ARM64)
cargo build --release --target aarch64-apple-ios

# Build for Android (ARM64)
cargo build --release --target aarch64-linux-android

# Build for Web (WASM)
cargo build --release --target wasm32-unknown-unknown
```

### 2. Link the Library to Your App

#### iOS

1. Add `libcustom_encryptor.a` to your Xcode project:
   - Drag the `.a` file into your Xcode project (copy if needed)
   - Ensure it's linked in **Build Phases → Link Binary With Libraries**

2. Add the Swift integration file:
   - Copy `ios/CustomEncryptor.swift` to your Xcode project

3. Include the generated header file (from Rust's `cbindgen`):
   ```bash
   cbindgen --lang C --output custom_encryptor.h src/lib.rs
   ```

#### Android

1. Add `libcustom_encryptor.so` to your `android/app/src/main/jniLibs/` directory:
   ```
   android/app/src/main/jniLibs/
   └── arm64-v8a/
       └── libcustom_encryptor.so
   ```

2. Add the Kotlin integration file:
   - Copy `android/CustomEncryptor.kt` to your Android project

3. Declare the native function in your `Android.mk` or `CMakeLists.txt`:
   ```cmake
   add_library(custom_encryptor SHARED IMPORTED)
   set_target_properties(custom_encryptor PROPERTIES
       IMPORTED_LOCATION ${CMAKE_SOURCE_DIR}/app/src/main/jniLibs/${ANDROID_ABI}/libcustom_encryptor.so)
   target_link_libraries(your_app custom_encryptor)
   ```

#### Web (WASM)

1. Add the custom encryptor crate to your WASM build:
   ```toml
   # In your Cargo.toml
   [dependencies]
   custom-encryptor = { path = "../custom-encryptor-rust" }
   ```

2. The custom encryptor is statically linked into the WASM binary at build time.
   No runtime registration is needed — encryption happens in Rust inside WASM.

### 3. Register at App Startup

#### iOS (Swift)

```swift
import YourApp

// At app startup (e.g., in AppDelegate.swift or SceneDelegate.swift):
CustomEncryptor.register(
    keyHex: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    nonceHex: "cafebabecafebabecafebabecafebabe"
)
```

#### Android (Kotlin)

```kotlin
import com.yourcompany.yourapp.CustomEncryptor

// At app startup (e.g., in Application class or MainActivity):
CustomEncryptor.register(
    keyHex = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    nonceHex = "cafebabecafebabecafebabecafebabe"
)
```

#### Web (TypeScript)

```typescript
import { S3BgUploader } from 'react-native-s3-bg-uploader';

// At app startup:
// The custom encryptor is statically linked into WASM — no registration needed.
// Just call encryptAndUploadFile() with your key/nonce.
```

### 4. Upload with Custom Encryption

```typescript
import { S3BgUploader } from 'react-native-s3-bg-uploader';

// Upload a file with custom AES-256-GCM encryption:
const fileHash = await S3BgUploader.encryptAndUploadFile(
  file,                    // File object or file path
  'transfer-1',           // Transfer ID
  'deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef', // 32-byte key (hex)
  'cafebabecafebabecafebabecafebabe'  // 12-byte nonce (hex)
);

console.log(`Encrypted file uploaded. Hash: ${fileHash}`);
```

## Security Notes

### Key Management

- **Generate keys securely**: Use a cryptographically secure random number generator.
- **Store keys securely**: Use platform-specific key storage (Keychain on iOS, Keystore on Android).
- **Never hardcode keys**: In production, fetch keys from a key management system (AWS KMS, HashiCorp Vault, etc.).

### Nonce Management

- **Unique per upload**: Each upload should use a unique nonce.
- **Deterministic per part**: The library derives per-part nonces from the base nonce + part number, ensuring deterministic encryption (resumable uploads).

### Algorithm Properties

| Property | Value |
|----------|-------|
| Cipher | AES-256 |
| Mode | GCM (Galois/Counter Mode) |
| Key size | 256 bits (32 bytes) |
| Nonce size | 96 bits (12 bytes) |
| Authentication tag | 128 bits (16 bytes) |
| Deterministic | Yes (same key + nonce + data → same ciphertext) |
| Resumable | Yes (per-part nonce derivation) |

## Verification

To verify encrypted files, use the same key and nonce to decrypt:

### Python (Example)

```python
from Crypto.Cipher import AES
from Crypto.Util.Padding import unpad
import struct

key = bytes.fromhex("deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
nonce = bytes.fromhex("cafebabecafebabecafebabecafebabe")

def decrypt_chunk(ciphertext: bytes, part_number: int) -> bytes:
    # Derive per-part nonce (same as Rust implementation)
    derived_nonce = bytearray(nonce)
    n = part_number.to_bytes(4, 'little')
    derived_nonce[0] ^= n[0]
    derived_nonce[1] ^= n[1]
    derived_nonce[2] ^= n[2]
    derived_nonce[3] ^= n[3]

    # Decrypt with AES-256-GCM
    cipher = AES.new(key, AES.MODE_GCM, nonce=bytes(derived_nonce))
    plaintext = cipher.decrypt(ciphertext)
    return plaintext

# Usage:
with open('encrypted_file.enc', 'rb') as f:
    encrypted_data = f.read()

# Split into chunks (each chunk is 16 bytes larger than plaintext due to GCM tag)
chunk_size = 1024 * 1024  # 1 MB chunks
encrypted_chunks = [encrypted_data[i:i+chunk_size+16] for i in range(0, len(encrypted_data), chunk_size+16)]

for i, chunk in enumerate(encrypted_chunks, start=1):
    plaintext = decrypt_chunk(chunk, i)
    print(f"Part {i}: {len(plaintext)} bytes")
```

## Project Structure

```
custom-encryption-example/
├── custom-encryptor-rust/
│   ├── Cargo.toml              # Rust library manifest
│   ├── src/
│   │   └── lib.rs              # AES-256-GCM encryptor implementation
│   ├── ios/
│   │   └── CustomEncryptor.swift  # iOS Swift integration
│   ├── android/
│   │   └── CustomEncryptor.kt     # Android Kotlin integration
│   └── web/
│       └── CustomEncryptor.ts     # Web TypeScript integration
├── example-app/
│   ├── App.tsx                 # React Native example app
│   └── package.json            # Dependencies
└── docs/
    └── README.md               # This documentation
```

## Troubleshooting

### "Native library not linked"

- **iOS**: Ensure `libcustom_encryptor.a` is added to Xcode's **Link Binary With Libraries**.
- **Android**: Ensure `libcustom_encryptor.so` is in `jniLibs/` and declared in your build script.

### "Failed to create custom encryptor"

- Verify the key is exactly 64 hex characters (32 bytes).
- Verify the nonce is exactly 24 hex characters (12 bytes for GCM).
- Check that the hex string contains only valid hex characters (0-9, a-f).

### "Upload failed: encryption error"

- Ensure the custom encryptor is registered **before** calling `encryptAndUploadFile()`.
- Check that the key/nonce match between registration and upload.

## License

MIT
