// CustomEncryptor.kt
// Android integration for custom AES-256-GCM encryption in S3BgUploader.
//
// This file demonstrates how to register a custom encryptor from Kotlin.
// The Rust library is statically linked into your app binary.

package com.yourcompany.yourapp // Replace with your package

import android.util.Log

// ---------------------------------------------------------------------------
// External C functions from the Rust library
// ---------------------------------------------------------------------------

// Declare the native function from the Rust library.
// The library should be linked via your build system (CMake/ndk-build).
external fun custom_encryptor_register(
    keyHex: String,
    nonceHex: String
)

// ---------------------------------------------------------------------------
// Kotlin API
// ---------------------------------------------------------------------------

/**
 * Register a custom AES-256-GCM encryptor with S3BgUploader.
 *
 * Call this once at app startup, before any uploads.
 *
 * @param keyHex 64 hex characters (32 bytes) — the AES-256 key.
 * @param nonceHex 48 hex characters (24 bytes) — the base nonce.
 *
 * @example
 * ```kotlin
 * CustomEncryptor.register(
 *     keyHex = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
 *     nonceHex = "cafebabecafebabecafebabecafebabecafebabecafebabe"
 * )
 * ```
 */
object CustomEncryptor {

    private const val TAG = "CustomEncryptor"

    /**
     * Register with hex-encoded key and nonce.
     */
    @JvmStatic
    fun register(keyHex: String, nonceHex: String) {
        try {
            custom_encryptor_register(keyHex, nonceHex)
            Log.i(TAG, "Custom AES-256-GCM encryptor registered successfully")
        } catch (e: UnsatisfiedLinkError) {
            Log.e(TAG, "Failed to register custom encryptor: native library not linked", e)
        } catch (e: Exception) {
            Log.e(TAG, "Failed to register custom encryptor", e)
        }
    }

    /**
     * Register with raw key/nonce bytes (converted to hex).
     */
    @JvmStatic
    fun register(key: ByteArray, nonce: ByteArray) {
        val keyHex = key.joinToString("") { "%02x".format(it) }
        val nonceHex = nonce.joinToString("") { "%02x".format(it) }
        register(keyHex, nonceHex)
    }
}
