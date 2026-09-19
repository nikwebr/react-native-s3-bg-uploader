#!/usr/bin/env python3
"""
Decrypt a file uploaded by react-native-s3-bg-uploader (XChaCha20-Poly1305).

Download the S3 object first, then run:

    python3 scripts/decrypt.py --key <64-hex> --nonce <48-hex> input.enc output

The script is fully self-contained: it reads the embedded footer to detect the
chunk count and effective part size — no extra arguments are needed.

Requirements:
    pip install pynacl
"""

import argparse
import struct
import sys
from typing import Optional, Tuple

try:
    from nacl.bindings.crypto_aead import (
        crypto_aead_xchacha20poly1305_ietf_decrypt,
        crypto_aead_xchacha20poly1305_ietf_KEYBYTES,
        crypto_aead_xchacha20poly1305_ietf_NPUBBYTES,
    )
except ImportError:
    sys.exit("Missing dependency. Run:  pip install pynacl")

TAG_SIZE = 16          # Poly1305 AEAD tag per chunk
FOOTER_PLAIN = 12      # 4 bytes n_data_parts (LE u32) + 8 bytes effective_part_size (LE u64)
FOOTER_SIZE = FOOTER_PLAIN + TAG_SIZE  # 28 bytes


def _derive_nonce(base_nonce: bytes, part_number: int) -> bytes:
    n = part_number.to_bytes(4, "little")
    nonce = bytearray(base_nonce)
    nonce[20] ^= n[0]
    nonce[21] ^= n[1]
    nonce[22] ^= n[2]
    nonce[23] ^= n[3]
    return bytes(nonce)


def _decrypt_chunk(
    key: bytes,
    base_nonce: bytes,
    ciphertext: bytes,
    part_number: int,
) -> bytes:
    nonce = _derive_nonce(base_nonce, part_number)
    aad = part_number.to_bytes(4, "little")
    return crypto_aead_xchacha20poly1305_ietf_decrypt(ciphertext, aad, nonce, key)


def _read_footer(
    key: bytes,
    base_nonce: bytes,
    footer_bytes: bytes,
    max_parts: int,
) -> Optional[Tuple[int, int]]:
    """
    Try part numbers n = 1..max_parts.  Footer is encrypted with part_number = n+1
    and its 12-byte plaintext encodes (n_data_parts LE u32, effective_part_size LE u64).
    Returns (n_data_parts, effective_part_size) on the first valid match.
    """
    for n in range(1, max_parts + 1):
        footer_part = n + 1
        nonce = _derive_nonce(base_nonce, footer_part)
        aad = footer_part.to_bytes(4, "little")
        try:
            plain = crypto_aead_xchacha20poly1305_ietf_decrypt(footer_bytes, aad, nonce, key)
            if len(plain) == FOOTER_PLAIN:
                n_parts = struct.unpack_from("<I", plain, 0)[0]
                eff_part = struct.unpack_from("<Q", plain, 4)[0]
                if n_parts == n and eff_part > 0:
                    return n_parts, eff_part
        except Exception:
            pass
    return None


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Decrypt a react-native-s3-bg-uploader XChaCha20-Poly1305 file"
    )
    parser.add_argument("--key", required=True, help="64 hex chars (32-byte key)")
    parser.add_argument("--nonce", required=True, help="48 hex chars (24-byte base nonce)")
    parser.add_argument(
        "--max-parts",
        type=int,
        default=100_000,
        help="Upper bound for brute-force part-count detection (default: 100000)",
    )
    parser.add_argument("input", help="Encrypted file downloaded from S3")
    parser.add_argument("output", help="Destination for decrypted output")
    args = parser.parse_args()

    try:
        key = bytes.fromhex(args.key)
    except ValueError as e:
        sys.exit(f"Invalid --key hex: {e}")
    try:
        base_nonce = bytes.fromhex(args.nonce)
    except ValueError as e:
        sys.exit(f"Invalid --nonce hex: {e}")

    if len(key) != crypto_aead_xchacha20poly1305_ietf_KEYBYTES:
        sys.exit(f"--key must be 32 bytes (64 hex chars), got {len(key)}")
    if len(base_nonce) != crypto_aead_xchacha20poly1305_ietf_NPUBBYTES:
        sys.exit(f"--nonce must be 24 bytes (48 hex chars), got {len(base_nonce)}")

    print(f"Reading {args.input} …")
    with open(args.input, "rb") as fh:
        data = fh.read()

    if len(data) < FOOTER_SIZE:
        sys.exit(f"File too small ({len(data)} B) — not a valid encrypted file")

    footer_bytes = data[-FOOTER_SIZE:]

    print("Detecting chunk layout from footer …")
    result = _read_footer(key, base_nonce, footer_bytes, args.max_parts)
    if result is None:
        sys.exit(
            "Could not read footer — check that --key and --nonce are correct "
            "and the file was not truncated or corrupted."
        )

    n_data_parts, effective_part_size = result
    plaintext_size = len(data) - n_data_parts * TAG_SIZE - FOOTER_SIZE

    print(f"  chunks             : {n_data_parts}")
    print(f"  effective part size: {effective_part_size:,} B")
    print(f"  plaintext size     : {plaintext_size:,} B")

    if plaintext_size < 0:
        sys.exit(f"Computed plaintext size is negative ({plaintext_size}) — file may be corrupted")

    ciphertext_data = data[:-FOOTER_SIZE]
    output = bytearray()
    offset = 0

    for part_num in range(1, n_data_parts + 1):
        plain_start = (part_num - 1) * effective_part_size
        plain_size = min(effective_part_size, plaintext_size - plain_start)
        cipher_size = plain_size + TAG_SIZE

        chunk = ciphertext_data[offset : offset + cipher_size]
        if len(chunk) != cipher_size:
            sys.exit(
                f"Part {part_num}: expected {cipher_size} B at offset {offset}, "
                f"got {len(chunk)} — file may be truncated"
            )

        try:
            plain_chunk = _decrypt_chunk(key, base_nonce, chunk, part_num)
        except Exception as exc:
            sys.exit(f"Part {part_num}: decryption failed — {exc}")

        output.extend(plain_chunk)
        offset += cipher_size

        if part_num % 50 == 0 or part_num == n_data_parts:
            pct = part_num * 100 // n_data_parts
            print(f"\r  [{pct:3d}%] {part_num}/{n_data_parts} parts", end="", flush=True)

    print()

    with open(args.output, "wb") as fh:
        fh.write(output)

    print(f"Done. {len(output):,} bytes written to {args.output}")


if __name__ == "__main__":
    main()
