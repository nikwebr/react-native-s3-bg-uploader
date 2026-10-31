//! `decrypt-verify` — standalone verifier for objects produced by
//! react-native-s3-bg-uploader with a chunk encryptor.
//!
//! It reads only the wire format (key + base nonce + part numbers), so it is an
//! independent decoder rather than a wrapper around the SDK code:
//!
//!   object = ct(part 1) || ct(part 2) || … || ct(part n) || ct(descriptor)
//!
//! * every data part is the plaintext of that part plus a 16-byte AEAD tag;
//! * full parts are exactly `part_size` bytes of ciphertext, so the part count is
//!   `ceil((len - 28) / part_size)` — and with `--part-size` omitted the tool
//!   tries the usual S3 part sizes and keeps the one whose descriptor
//!   authenticates;
//! * the descriptor is the last 28 bytes: AES-GCM/XChaCha of
//!   `n_data_parts (u32 LE) || effective_part_size (u64 LE)`, encrypted with
//!   `part_number = n_data_parts + 1`;
//! * the per-part nonce is derived from the base nonce and the part number
//!   (deterministically, which is what makes resume work):
//!     - custom AES-256-GCM: `nonce[0..4] ^= part_number_le` on a 12-byte GCM nonce
//!     - built-in XChaCha20-Poly1305: `nonce[20..24] ^= part_number_le`, with the
//!       part number also passed as AEAD associated data
//!
//! Exit code 0 means every part and the descriptor authenticated; 1 means nothing
//! did (wrong key/nonce, different scheme, or an unencrypted/plain upload).
//!
//! Build & run:
//!   cd tools/decrypt && cargo run --release -- <object> --key <64 hex> --nonce <48 hex>

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

// ---------------------------------------------------------------------------
// Wire format constants (mirror the two implementations)
// ---------------------------------------------------------------------------

/// AEAD tag per part: Poly1305 (built-in) and AES-GCM both add 16 bytes.
const TAG_LEN: u64 = 16;
/// Descriptor plaintext: n_data_parts (u32 LE) || effective_part_size (u64 LE).
const DESCRIPTOR_PLAINTEXT_LEN: usize = 12;
/// Descriptor part as stored: plaintext + tag.
const DESCRIPTOR_LEN: u64 = DESCRIPTOR_PLAINTEXT_LEN as u64 + TAG_LEN;

/// Highest data-part count the descriptor search will try when `--part-size` is
/// not given. Each attempt costs one AEAD verification of the 28-byte descriptor,
/// so walking the whole range is still far below a second.
const MAX_PARTS: u32 = 200_000;

/// Demo key/nonce (the ones the example app registers with) — used by
/// `--make-fixture` so a generated vector is verifiable with the README values.
const DEMO_KEY_HEX: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
const DEMO_NONCE_HEX: &str = "cafebabecafebabecafebabecafebabecafebabecafebabe";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scheme {
    /// Host-provided AES-256-GCM (custom-encryptor-rust).
    AesGcm,
    /// Built-in XChaCha20-Poly1305.
    XChaCha,
}

impl Scheme {
    fn name(self) -> &'static str {
        match self {
            Scheme::AesGcm => "aesgcm (custom encryptor)",
            Scheme::XChaCha => "xchacha (built-in)",
        }
    }
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "aesgcm" | "aes-gcm" | "custom" => Ok(Scheme::AesGcm),
            "xchacha" | "xchacha20poly1305" | "builtin" | "built-in" => Ok(Scheme::XChaCha),
            other => Err(format!(
                "unknown scheme `{other}` (use aesgcm, xchacha or auto)"
            )),
        }
    }
}

/// How the object is split, recovered from the part size + descriptor.
#[derive(Debug)]
struct Layout {
    part_size: u64,
    n_data_parts: u32,
    effective_part_size: u64,
    last_ct_len: u64,
}

// ---------------------------------------------------------------------------
// Hex
// ---------------------------------------------------------------------------

fn hex_to_fixed<const N: usize>(hex: &str, label: &str) -> Result<[u8; N], String> {
    let hex = hex.trim();
    if hex.len() != N * 2 {
        return Err(format!(
            "{label}: expected {} hex chars, got {}",
            N * 2,
            hex.len()
        ));
    }
    let mut out = [0u8; N];
    for (i, pair) in hex.as_bytes().chunks(2).enumerate() {
        let s = std::str::from_utf8(pair).map_err(|e| e.to_string())?;
        out[i] = u8::from_str_radix(s, 16).map_err(|e| format!("{label}: invalid hex: {e}"))?;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Crypto (the two schemes)
// ---------------------------------------------------------------------------

fn aesgcm_nonce(base: &[u8; 24], part_number: u32) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&base[..12]);
    let n = part_number.to_le_bytes();
    for i in 0..4 {
        nonce[i] ^= n[i];
    }
    nonce
}

fn aesgcm_encrypt(key: &[u8; 32], base: &[u8; 24], data: &[u8], part_number: u32) -> Vec<u8> {
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    Aes256Gcm::new(key.into())
        .encrypt(Nonce::from_slice(&aesgcm_nonce(base, part_number)), data)
        .expect("aes-gcm encrypt")
}

fn aesgcm_decrypt(
    key: &[u8; 32],
    base: &[u8; 24],
    ct: &[u8],
    part_number: u32,
) -> Result<Vec<u8>, String> {
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    Aes256Gcm::new(key.into())
        .decrypt(Nonce::from_slice(&aesgcm_nonce(base, part_number)), ct)
        .map_err(|_| "AES-GCM tag verification failed".to_string())
}

fn xchacha_nonce(base: &[u8; 24], part_number: u32) -> [u8; 24] {
    let mut nonce = *base;
    let n = part_number.to_le_bytes();
    for i in 0..4 {
        nonce[20 + i] ^= n[i];
    }
    nonce
}

fn xchacha_encrypt(key: &[u8; 32], base: &[u8; 24], data: &[u8], part_number: u32) -> Vec<u8> {
    use chacha20poly1305::aead::{Aead, Payload};
    use chacha20poly1305::{KeyInit, XChaCha20Poly1305};
    let nonce = xchacha_nonce(base, part_number);
    XChaCha20Poly1305::new(key.into())
        .encrypt(
            (&nonce).into(),
            Payload {
                msg: data,
                aad: &part_number.to_le_bytes(),
            },
        )
        .expect("xchacha encrypt")
}

fn xchacha_decrypt(
    key: &[u8; 32],
    base: &[u8; 24],
    ct: &[u8],
    part_number: u32,
) -> Result<Vec<u8>, String> {
    use chacha20poly1305::aead::{Aead, Payload};
    use chacha20poly1305::{KeyInit, XChaCha20Poly1305};
    let nonce = xchacha_nonce(base, part_number);
    XChaCha20Poly1305::new(key.into())
        .decrypt(
            (&nonce).into(),
            Payload {
                msg: ct,
                aad: &part_number.to_le_bytes(),
            },
        )
        .map_err(|_| "XChaCha20-Poly1305 tag verification failed".to_string())
}

fn encrypt_part(scheme: Scheme, key: &[u8; 32], base: &[u8; 24], data: &[u8], pn: u32) -> Vec<u8> {
    match scheme {
        Scheme::AesGcm => aesgcm_encrypt(key, base, data, pn),
        Scheme::XChaCha => xchacha_encrypt(key, base, data, pn),
    }
}

fn decrypt_part(
    scheme: Scheme,
    key: &[u8; 32],
    base: &[u8; 24],
    ct: &[u8],
    pn: u32,
) -> Result<Vec<u8>, String> {
    match scheme {
        Scheme::AesGcm => aesgcm_decrypt(key, base, ct, pn),
        Scheme::XChaCha => xchacha_decrypt(key, base, ct, pn),
    }
}

// ---------------------------------------------------------------------------
// Reading the object
// ---------------------------------------------------------------------------

fn read_range(file: &mut File, offset: u64, len: u64) -> Result<Vec<u8>, String> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| format!("seek to {offset}: {e}"))?;
    let mut buf = vec![0u8; len as usize];
    file.read_exact(&mut buf)
        .map_err(|e| format!("read {len} bytes at {offset}: {e}"))?;
    Ok(buf)
}

/// Work out the split for one scheme and one candidate part size. Succeeds only
/// if the descriptor authenticates *and* agrees with the size arithmetic — a
/// 128-bit tag match plus consistent numbers is what makes this detection safe.
fn find_layout(
    file: &mut File,
    object_len: u64,
    scheme: Scheme,
    key: &[u8; 32],
    base: &[u8; 24],
    part_size: u64,
) -> Result<Layout, String> {
    if part_size <= TAG_LEN {
        return Err(format!(
            "part size {part_size} is smaller than the {TAG_LEN}-byte tag"
        ));
    }
    if object_len <= DESCRIPTOR_LEN {
        return Err("object is too small to contain a descriptor".into());
    }

    let data_total = object_len - DESCRIPTOR_LEN;
    let n_data_parts = data_total.div_ceil(part_size);
    if n_data_parts == 0 || n_data_parts > u32::MAX as u64 - 1 {
        return Err("implausible part count".into());
    }
    let last_ct_len = data_total - (n_data_parts - 1) * part_size;
    if last_ct_len == 0 || last_ct_len > part_size {
        return Err("object size does not fit this part size".into());
    }

    let descriptor_ct = read_range(file, object_len - DESCRIPTOR_LEN, DESCRIPTOR_LEN)?;
    let descriptor = decrypt_part(scheme, key, base, &descriptor_ct, (n_data_parts + 1) as u32)?;
    if descriptor.len() != DESCRIPTOR_PLAINTEXT_LEN {
        return Err("descriptor has an unexpected length".into());
    }
    let claimed_parts = u32::from_le_bytes(descriptor[0..4].try_into().unwrap());
    let claimed_effective = u64::from_le_bytes(descriptor[4..12].try_into().unwrap());

    if claimed_parts as u64 != n_data_parts {
        return Err(format!(
            "descriptor says {claimed_parts} data parts, object size implies {n_data_parts}"
        ));
    }
    if claimed_effective + TAG_LEN != part_size {
        return Err(format!(
            "descriptor says effective part size {claimed_effective}, but part size {part_size} implies {}",
            part_size - TAG_LEN
        ));
    }

    Ok(Layout {
        part_size,
        // Bounded by the check above.
        n_data_parts: n_data_parts as u32,
        effective_part_size: claimed_effective,
        last_ct_len,
    })
}

/// Find the layout without being told the part size.
///
/// The part size cannot be guessed from the server: the engine subtracts its own
/// tag *and* the descriptor from the server's value, so a 32 MiB server part size
/// yields 33,554,404-byte parts — not a round number any candidate list contains.
/// The descriptor is the way in: it is authenticated with `part_number = n + 1`
/// and states both `n` and the plaintext chunk size, so each candidate part count
/// is a single tag verification (a false positive needs a 2^-128 collision).
fn find_layout_auto(
    file: &mut File,
    object_len: u64,
    scheme: Scheme,
    key: &[u8; 32],
    base: &[u8; 24],
) -> Result<Layout, String> {
    if object_len <= DESCRIPTOR_LEN {
        return Err("object is too small to contain a descriptor".into());
    }
    let data_total = object_len - DESCRIPTOR_LEN;
    let descriptor_ct = read_range(file, object_len - DESCRIPTOR_LEN, DESCRIPTOR_LEN)?;

    for n in 1..=MAX_PARTS {
        let Ok(descriptor) = decrypt_part(scheme, key, base, &descriptor_ct, n + 1) else {
            continue;
        };
        if descriptor.len() != DESCRIPTOR_PLAINTEXT_LEN {
            continue;
        }
        let claimed_parts = u32::from_le_bytes(descriptor[0..4].try_into().unwrap());
        if claimed_parts != n {
            continue;
        }
        let effective = u64::from_le_bytes(descriptor[4..12].try_into().unwrap());
        let part_size = effective + TAG_LEN;
        // The claimed plaintext chunk size has to agree with the object: n-1 full
        // parts plus one final part that is neither empty nor over-full.
        if (n as u64 - 1) * part_size >= data_total || n as u64 * part_size < data_total {
            continue;
        }
        return Ok(Layout {
            part_size,
            n_data_parts: n,
            effective_part_size: effective,
            last_ct_len: data_total - (n as u64 - 1) * part_size,
        });
    }
    Err(format!(
        "no descriptor authenticated for 1..={MAX_PARTS} data parts \
         (wrong key/nonce, or the object was made by another scheme)"
    ))
}

/// Decrypt every data part into `out`; returns the number of plaintext bytes.
fn decode<W: Write>(
    file: &mut File,
    layout: &Layout,
    scheme: Scheme,
    key: &[u8; 32],
    base: &[u8; 24],
    out: &mut W,
) -> Result<u64, String> {
    let mut written = 0u64;
    for i in 1..=layout.n_data_parts {
        let offset = (i as u64 - 1) * layout.part_size;
        let len = if i == layout.n_data_parts {
            layout.last_ct_len
        } else {
            layout.part_size
        };
        let ct = read_range(file, offset, len)?;
        let pt = decrypt_part(scheme, key, base, &ct, i).map_err(|e| {
            format!(
                "data part {i}/{}({len} bytes at {offset}): {e}",
                layout.n_data_parts
            )
        })?;
        out.write_all(&pt).map_err(|e| format!("write: {e}"))?;
        written += pt.len() as u64;
    }
    Ok(written)
}

/// Build an object the way the upload engine does (used by `--make-fixture` and
/// the tests): full parts are `part_size` bytes of ciphertext and the final part is
/// the last chunk's ciphertext followed by the descriptor. `part_size` here is the
/// ciphertext size per full part, i.e. what the engine derives as
/// `server_part_size - tag_len - descriptor_len`.
fn build_object(
    scheme: Scheme,
    key: &[u8; 32],
    base: &[u8; 24],
    plaintext: &[u8],
    part_size: u64,
) -> (Vec<u8>, Layout) {
    let effective = part_size - TAG_LEN;
    let n_data_parts = ((plaintext.len() as u64).div_ceil(effective)).max(1) as u32;
    let mut object = Vec::new();
    for i in 1..=n_data_parts {
        let start = (i as u64 - 1) * effective;
        let end = ((i as u64) * effective).min(plaintext.len() as u64);
        let chunk = &plaintext[start as usize..end as usize];
        object.extend_from_slice(&encrypt_part(scheme, key, base, chunk, i));
    }
    let mut descriptor = [0u8; DESCRIPTOR_PLAINTEXT_LEN];
    descriptor[..4].copy_from_slice(&n_data_parts.to_le_bytes());
    descriptor[4..].copy_from_slice(&effective.to_le_bytes());
    object.extend_from_slice(&encrypt_part(
        scheme,
        key,
        base,
        &descriptor,
        n_data_parts + 1,
    ));

    let data_total = object.len() as u64 - DESCRIPTOR_LEN;
    let last_ct_len = data_total - (n_data_parts as u64 - 1) * part_size;
    (
        object,
        Layout {
            part_size,
            n_data_parts,
            effective_part_size: effective,
            last_ct_len,
        },
    )
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

struct Args {
    object: Option<PathBuf>,
    key_hex: String,
    nonce_hex: String,
    scheme: String,
    part_size: Option<u64>,
    out: Option<PathBuf>,
    expect: Option<PathBuf>,
    make_fixture: Option<PathBuf>,
    plaintext_size: u64,
}

fn usage() -> String {
    format!(
        r#"decrypt-verify — check that an uploaded object really is custom-encrypted

USAGE
  decrypt-verify <object> [--key <64 hex>] [--nonce <48 hex>] [options]

OPTIONS
  --key <hex>           256-bit key (64 hex chars). Default: the demo key.
  --nonce <hex>         192-bit base nonce (48 hex chars). Default: the demo nonce.
  --scheme <s>          auto | aesgcm | xchacha          (default: auto)
                        `auto` reports which scheme actually produced the object,
                        which is how you tell the custom encryptor apart from the
                        built-in one.
  --part-size <bytes>   Ciphertext part size, if you want to force one. Omitted:
                        the descriptor is read first, which yields the part size
                        and the part count for ANY layout (the engine's real part
                        size is server_part_size - tag - descriptor, not a round
                        number).
  --out <file>          Write the decrypted plaintext here (default: <object>.decrypted)
  --expect <file>       Compare the plaintext against this file byte for byte.
  --make-fixture <dir>  Don't verify: generate a test vector (object + plaintext)
                        with the demo key/nonce, then verify it with this tool.
  --plaintext-size <n>  Size for --make-fixture (default: 20 MiB)
  --help                This text.

EXIT CODE
  0 = every part and the descriptor authenticated;  1 = nothing did.
"#
    )
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        object: None,
        key_hex: DEMO_KEY_HEX.to_string(),
        nonce_hex: DEMO_NONCE_HEX.to_string(),
        scheme: "auto".to_string(),
        part_size: None,
        out: None,
        expect: None,
        make_fixture: None,
        plaintext_size: 20 * 1024 * 1024,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--help" | "-h" => return Err(usage()),
            "--key" => args.key_hex = value()?,
            "--nonce" => args.nonce_hex = value()?,
            "--scheme" => args.scheme = value()?,
            "--out" => args.out = Some(PathBuf::from(value()?)),
            "--expect" => args.expect = Some(PathBuf::from(value()?)),
            "--make-fixture" => args.make_fixture = Some(PathBuf::from(value()?)),
            "--part-size" => {
                args.part_size = Some(value()?.parse().map_err(|e| format!("--part-size: {e}"))?)
            }
            "--plaintext-size" => {
                args.plaintext_size = value()?
                    .parse()
                    .map_err(|e| format!("--plaintext-size: {e}"))?
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag {other}\n\n{}", usage()))
            }
            other => args.object = Some(PathBuf::from(other)),
        }
    }
    Ok(args)
}

/// Byte-for-byte compare, streamed so it works on large files.
fn files_equal(a: &Path, b: &Path) -> Result<bool, String> {
    let (len_a, len_b) = (
        fs::metadata(a).map_err(|e| e.to_string())?.len(),
        fs::metadata(b).map_err(|e| e.to_string())?.len(),
    );
    if len_a != len_b {
        return Ok(false);
    }
    let mut fa = File::open(a).map_err(|e| e.to_string())?;
    let mut fb = File::open(b).map_err(|e| e.to_string())?;
    let mut buf_a = vec![0u8; 1 << 20];
    let mut buf_b = vec![0u8; 1 << 20];
    loop {
        let na = fa.read(&mut buf_a).map_err(|e| e.to_string())?;
        let nb = fb.read(&mut buf_b).map_err(|e| e.to_string())?;
        if na != nb || buf_a[..na] != buf_b[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}

/// Recognise the usual plain-file magic numbers — an object that starts with one
/// of these was stored unencrypted (ciphertext has no such structure).
fn sniff_plaintext(path: &Path) -> Option<&'static str> {
    let mut f = File::open(path).ok()?;
    let mut head = [0u8; 8];
    f.read_exact(&mut head).ok()?;
    match head {
        [0x50, 0x4b, 0x03, 0x04, ..]
        | [0x50, 0x4b, 0x05, 0x06, ..]
        | [0x50, 0x4b, 0x07, 0x08, ..] => Some("a ZIP container (Office document, .jar, .apk, …)"),
        [0x25, 0x50, 0x44, 0x46, ..] => Some("a PDF"),
        [0x89, b'P', b'N', b'G', ..] => Some("a PNG image"),
        [0xff, 0xd8, 0xff, ..] => Some("a JPEG image"),
        [b'G', b'I', b'F', b'8', ..] => Some("a GIF image"),
        [0x1f, 0x8b, ..] => Some("a gzip stream"),
        [0x42, 0x5a, 0x68, ..] => Some("a bzip2 stream"),
        [0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c, ..] => Some("a 7-Zip archive"),
        [0x52, 0x61, 0x72, 0x21, ..] => Some("a RAR archive"),
        [0x7f, b'E', b'L', b'F', ..] => Some("an ELF binary"),
        [_, _, _, _, b'f', b't', b'y', b'p'] => Some("an ISO base media file (MP4/MOV)"),
        _ => None,
    }
}

/// Shannon entropy of a 64 KiB sample, in bits per byte. Encrypted data sits at
/// ~8.0; plain structured data is lower (already-compressed files are the
/// exception — those are caught by the magic check above).
fn sample_entropy(path: &Path) -> Option<f64> {
    let mut f = File::open(path).ok()?;
    let mut buf = vec![0u8; 64 * 1024];
    let n = f.read(&mut buf).ok()?;
    if n == 0 {
        return None;
    }
    let mut counts = [0u64; 256];
    for &b in &buf[..n] {
        counts[b as usize] += 1;
    }
    let n = n as f64;
    let entropy = counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum();
    Some(entropy)
}

/// Explain an object that cannot be decrypted: is it simply not encrypted?
fn diagnose_not_encrypted(object: &Path, expect: Option<&Path>) -> Vec<String> {
    let mut out = Vec::new();

    if let Some(expected) = expect {
        match files_equal(object, expected) {
            Ok(true) => {
                out.push(format!(
                    "the object is byte-identical to {} — this upload was NOT encrypted",
                    expected.display()
                ));
                return out;
            }
            Ok(false) => {}
            Err(e) => out.push(format!(
                "(could not compare with {}: {e})",
                expected.display()
            )),
        }
    }

    if let Some(kind) = sniff_plaintext(object) {
        out.push(format!(
            "the object starts with a known file signature: it is {kind}, i.e. stored plaintext, not ciphertext"
        ));
    } else if let Some(h) = sample_entropy(object) {
        if h < 7.5 {
            out.push(format!(
                "the first 64 KiB have an entropy of {h:.2} bits/byte — comparable to ordinary file data, not ciphertext"
            ));
        }
    }
    out
}

fn human(n: u64) -> String {
    if n >= 1024 * 1024 && n % (1024 * 1024) == 0 {
        format!("{n} ({} MiB)", n / (1024 * 1024))
    } else {
        n.to_string()
    }
}

fn make_fixture(dir: &Path, args: &Args, key: &[u8; 32], base: &[u8; 24]) -> Result<(), String> {
    let part_size = args.part_size.unwrap_or(8 * 1024 * 1024);
    fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;

    // Deterministic, non-compressible-looking plaintext so a mismatch is obvious.
    let mut plaintext = Vec::with_capacity(args.plaintext_size as usize);
    let mut x: u64 = 0x9E3779B97F4A7C15;
    while (plaintext.len() as u64) < args.plaintext_size {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        plaintext.extend_from_slice(&x.to_le_bytes());
    }
    plaintext.truncate(args.plaintext_size as usize);

    fs::write(dir.join("plaintext.bin"), &plaintext).map_err(|e| e.to_string())?;

    for scheme in [Scheme::AesGcm, Scheme::XChaCha] {
        let (object, layout) = build_object(scheme, key, base, &plaintext, part_size);
        let name = match scheme {
            Scheme::AesGcm => "object-aesgcm.bin",
            Scheme::XChaCha => "object-xchacha.bin",
        };
        fs::write(dir.join(name), &object).map_err(|e| e.to_string())?;
        println!(
            "wrote {}/{}  ({} bytes, {} data parts of {})",
            dir.display(),
            name,
            object.len(),
            layout.n_data_parts,
            human(layout.part_size)
        );
    }
    fs::write(dir.join("plaintext.bin"), &plaintext).map_err(|e| e.to_string())?;
    let readme = format!(
        "Test vectors for decrypt-verify\n\nkey   : {}\nnonce : {}\npart  : {}\n\n\
         Verify the custom (AES-GCM) one:\n  \
         cargo run --release -- object-aesgcm.bin --key {} --nonce {} --expect plaintext.bin\n\n\
         Verify the built-in (XChaCha) one:\n  \
         cargo run --release -- object-xchacha.bin --key {} --nonce {} --scheme xchacha --expect plaintext.bin\n\n\
         Run without --scheme to let the tool report which scheme produced the object.\n",
        args.key_hex,
        args.nonce_hex,
        human(part_size),
        args.key_hex,
        args.nonce_hex,
        args.key_hex,
        args.nonce_hex,
    );
    fs::write(dir.join("README.txt"), readme).map_err(|e| e.to_string())?;
    println!(
        "wrote {}/plaintext.bin  ({} bytes)",
        dir.display(),
        plaintext.len()
    );
    println!("wrote {}/README.txt", dir.display());
    Ok(())
}

fn run() -> Result<bool, String> {
    let args = parse_args()?;
    let key = hex_to_fixed::<32>(&args.key_hex, "key")?;
    let base = hex_to_fixed::<24>(&args.nonce_hex, "nonce")?;

    if let Some(dir) = args.make_fixture.clone() {
        make_fixture(&dir, &args, &key, &base)?;
        return Ok(true);
    }

    let object_path = args
        .object
        .clone()
        .ok_or_else(|| format!("no object file given\n\n{}", usage()))?;
    let metadata = fs::metadata(&object_path)
        .map_err(|e| format!("cannot stat {}: {e}", object_path.display()))?;
    let object_len = metadata.len();
    let mut file = File::open(&object_path)
        .map_err(|e| format!("cannot open {}: {e}", object_path.display()))?;

    let schemes: Vec<Scheme> = match args.scheme.as_str() {
        "auto" => vec![Scheme::AesGcm, Scheme::XChaCha],
        other => vec![Scheme::parse(other)?],
    };
    let part_size_override = args.part_size;

    println!(
        "object      : {} ({} bytes)",
        object_path.display(),
        object_len
    );

    // Find the scheme whose descriptor authenticates. Without --part-size the
    // layout (part size + part count) is read out of the descriptor itself.
    let mut found: Option<(Scheme, Layout)> = None;
    let mut notes: Vec<String> = Vec::new();
    for scheme in &schemes {
        let attempt = match part_size_override {
            Some(p) => find_layout(&mut file, object_len, *scheme, &key, &base, p),
            None => find_layout_auto(&mut file, object_len, *scheme, &key, &base),
        };
        match attempt {
            Ok(layout) => {
                found = Some((*scheme, layout));
                break;
            }
            Err(e) => notes.push(format!("  {}: {}", scheme.name(), e)),
        }
    }

    let Some((scheme, layout)) = found else {
        println!("result      : NO scheme/part-size combination authenticated");
        let diagnosis = diagnose_not_encrypted(&object_path, args.expect.as_deref());
        if !diagnosis.is_empty() {
            println!("diagnosis   : {}", diagnosis.join("\n              "));
        }
        if notes.len() <= 12 {
            println!("tried:");
            for n in &notes {
                println!("{n}");
            }
        }
        println!(
            "\nSo the object was not produced with this key/nonce, not by either supported \
             scheme, or it was never encrypted.\nIf you expected the custom encryptor, the \
             usual cause is that the upload was skipped entirely: the SDK keys its session \
             on the transfer id + file content only, so re-uploading the same file under the \
             same transfer id after a plaintext upload counts as 'already completed' — \
             nothing is uploaded and the old plaintext object stays in place. Upload again \
             with a FRESH transfer id (or clear the app's session); the new object must then \
             be larger than the plaintext by 16 bytes per part + 28 bytes for the descriptor."
        );
        return Ok(false);
    };

    println!("scheme      : {}", scheme.name());
    println!(
        "part size   : {} — effective plaintext per part {}",
        human(layout.part_size),
        layout.effective_part_size
    );
    println!(
        "data parts  : {} (last part {} ct bytes)",
        layout.n_data_parts, layout.last_ct_len
    );
    println!(
        "descriptor  : part {} of {} bytes -> n_data_parts={}, effective_part_size={}",
        layout.n_data_parts + 1,
        DESCRIPTOR_LEN,
        layout.n_data_parts,
        layout.effective_part_size
    );

    let out_path = args.out.clone().unwrap_or_else(|| {
        object_path.with_extension(format!(
            "{}decrypted",
            object_path
                .extension()
                .map(|e| format!("{}.", e.to_string_lossy()))
                .unwrap_or_default()
        ))
    });
    let mut out = File::create(&out_path)
        .map_err(|e| format!("cannot create {}: {e}", out_path.display()))?;
    let plaintext_len = decode(&mut file, &layout, scheme, &key, &base, &mut out)?;
    out.flush().map_err(|e| e.to_string())?;
    println!(
        "authenticated: {} data parts + descriptor",
        layout.n_data_parts
    );
    println!(
        "wrote       : {} ({} bytes)",
        out_path.display(),
        plaintext_len
    );

    let mut ok = true;
    if let Some(expect_path) = &args.expect {
        let expected = fs::read(expect_path)
            .map_err(|e| format!("cannot read {}: {e}", expect_path.display()))?;
        let actual = fs::read(&out_path).map_err(|e| e.to_string())?;
        if expected == actual {
            println!(
                "expect      : MATCH ({} bytes, byte-identical)",
                expected.len()
            );
        } else {
            ok = false;
            let first_diff = expected.iter().zip(actual.iter()).position(|(a, b)| a != b);
            println!(
                "expect      : MISMATCH — {} has {} bytes, {} has {} bytes{}",
                expect_path.display(),
                expected.len(),
                out_path.display(),
                actual.len(),
                first_diff
                    .map(|i| format!(", first difference at offset {i}"))
                    .unwrap_or_default()
            );
        }
    }

    if ok {
        println!(
            "verdict     : OK — the object was encrypted with {} and this key/nonce",
            scheme.name()
        );
    } else {
        println!("verdict     : FAILED — plaintext does not match the expected file");
    }
    Ok(ok)
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("decrypt-verify-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn keys() -> ([u8; 32], [u8; 24]) {
        (
            hex_to_fixed::<32>(DEMO_KEY_HEX, "key").unwrap(),
            hex_to_fixed::<24>(DEMO_NONCE_HEX, "nonce").unwrap(),
        )
    }

    /// Plaintext detection: containers are recognised by magic, random data is not.
    #[test]
    fn detects_plaintext_containers() {
        let d = dir("sniff");
        fs::create_dir_all(&d).unwrap();

        let zip = d.join("z.bin");
        let mut bytes = vec![0x50u8, 0x4b, 0x03, 0x04];
        bytes.extend_from_slice(&[0u8; 65536]);
        fs::write(&zip, &bytes).unwrap();
        assert!(sniff_plaintext(&zip).unwrap().contains("ZIP"));

        let random = d.join("r.bin");
        let bytes: Vec<u8> = (0..65536u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
            .collect();
        fs::write(&random, &bytes).unwrap();
        assert!(sniff_plaintext(&random).is_none());
        assert!(sample_entropy(&random).unwrap() > 5.0);

        // An object that is byte-identical to the expected plaintext is reported as such.
        let diag = diagnose_not_encrypted(&zip, Some(&zip));
        assert!(diag[0].contains("NOT encrypted"), "{diag:?}");
    }

    /// Multi-part object, both schemes: descriptor + every part must round-trip.
    #[test]
    fn roundtrips_multipart_for_both_schemes() {
        let (key, base) = keys();
        let part_size = 1 * 1024 * 1024;
        let plaintext: Vec<u8> = (0..(2 * part_size as usize + 12_345))
            .map(|i| (i % 251) as u8)
            .collect();

        for scheme in [Scheme::AesGcm, Scheme::XChaCha] {
            let (object, layout) = build_object(scheme, &key, &base, &plaintext, part_size);
            assert_eq!(layout.n_data_parts, 3, "{scheme:?}");
            assert_eq!(object.len() as u64, plaintext.len() as u64 + 16 * 3 + 28);

            let path = dir(&format!("{scheme:?}")).join("object.bin");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &object).unwrap();

            let mut file = File::open(&path).unwrap();
            let found = find_layout(
                &mut file,
                object.len() as u64,
                scheme,
                &key,
                &base,
                part_size,
            )
            .expect("layout must authenticate");
            assert_eq!(found.n_data_parts, 3);
            let mut out = Vec::new();
            decode(&mut file, &found, scheme, &key, &base, &mut out).unwrap();
            assert_eq!(out, plaintext, "{scheme:?} plaintext mismatch");
        }
    }

    /// Part size is auto-detected from the descriptor when it is not supplied.
    #[test]
    fn detects_part_size_when_omitted() {
        let (key, base) = keys();
        let part_size = 5 * 1024 * 1024;
        let plaintext = vec![7u8; 7 * 1024 * 1024];
        let (object, _) = build_object(Scheme::AesGcm, &key, &base, &plaintext, part_size);

        let path = dir("detect").join("object.bin");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &object).unwrap();

        let mut file = File::open(&path).unwrap();
        let layout = find_layout_auto(&mut file, object.len() as u64, Scheme::AesGcm, &key, &base)
            .expect("the descriptor must authenticate");
        assert_eq!(layout.part_size, part_size);
        assert_eq!(layout.n_data_parts, 2);
    }

    /// The layout the engine really produces against this backend: part size
    /// 32 MiB, from which the engine subtracts its tag *and* the descriptor, so the
    /// ciphertext parts are 33,554,404 bytes. 36,631,089 bytes then need 2 parts —
    /// and before the descriptor moved into the last part, that layout failed
    /// every S3 completion with EntityTooSmall.
    #[test]
    fn detects_the_engines_non_round_part_size() {
        let (key, base) = keys();
        let server_part_size = 32u64 * 1024 * 1024;
        let part_size = server_part_size - TAG_LEN - DESCRIPTOR_LEN;
        let plaintext: Vec<u8> = (0..36_631_089usize).map(|i| (i % 251) as u8).collect();
        let (object, built) = build_object(Scheme::AesGcm, &key, &base, &plaintext, part_size);
        assert_eq!(built.n_data_parts, 2);

        let path = dir("engine-layout").join("object.bin");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &object).unwrap();
        let mut file = File::open(&path).unwrap();

        let found = find_layout_auto(&mut file, object.len() as u64, Scheme::AesGcm, &key, &base)
            .expect("descriptor must authenticate");
        assert_eq!(found.part_size, part_size);
        // Not a round number: the engine reserved both the tag (16) and the
        // descriptor (28), so a candidate list of round part sizes misses it.
        assert_eq!(found.part_size % (1024 * 1024), 1024 * 1024 - 44);
        assert_eq!(found.n_data_parts, 2);

        let mut out = Vec::new();
        decode(&mut file, &found, Scheme::AesGcm, &key, &base, &mut out).unwrap();
        assert_eq!(out, plaintext);
    }

    /// A wrong key must authenticate nothing (that is the whole point).
    #[test]
    fn wrong_key_rejects_everything() {
        let (key, base) = keys();
        let plaintext = vec![1u8; 3 * 1024 * 1024];
        let (object, _) = build_object(Scheme::AesGcm, &key, &base, &plaintext, 1 * 1024 * 1024);

        let mut wrong = key;
        wrong[0] ^= 0xff;

        let path = dir("wrongkey").join("object.bin");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &object).unwrap();
        let mut file = File::open(&path).unwrap();
        let err = find_layout(
            &mut file,
            object.len() as u64,
            Scheme::AesGcm,
            &wrong,
            &base,
            1 * 1024 * 1024,
        )
        .expect_err("must not authenticate");
        assert!(err.contains("tag verification failed"), "{err}");
    }

    /// The custom scheme must not decrypt a built-in-encrypted object (and vice
    /// versa) — this is how `auto` tells the two apart.
    #[test]
    fn schemes_are_not_interchangeable() {
        let (key, base) = keys();
        let plaintext = vec![3u8; 2 * 1024 * 1024 + 4096];
        let (object, _) = build_object(Scheme::XChaCha, &key, &base, &plaintext, 1 * 1024 * 1024);

        let path = dir("cross").join("object.bin");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &object).unwrap();
        let mut file = File::open(&path).unwrap();

        assert!(
            find_layout(
                &mut file,
                object.len() as u64,
                Scheme::AesGcm,
                &key,
                &base,
                1 * 1024 * 1024
            )
            .is_err(),
            "AES-GCM must not authenticate an XChaCha object"
        );
        assert!(
            find_layout(
                &mut file,
                object.len() as u64,
                Scheme::XChaCha,
                &key,
                &base,
                1 * 1024 * 1024
            )
            .is_ok(),
            "XChaCha must authenticate its own object"
        );
    }
}
