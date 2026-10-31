/**
 * Verifies the browser custom encryptor without a browser.
 *
 * It runs the exact factory that App.tsx registers on web
 * (example-app/web-encryptor.ts, WebCrypto AES-256-GCM), lays the ciphertext out
 * the way the uploader does (data parts + descriptor folded into the last part,
 * with the declared overhead/footer_len values reserved in every part), and then
 * checks the result with tools/decrypt — the same verifier used for device
 * uploads. If this passes, a web upload produced by the same code verifies too.
 *
 *   node tools/verify-web-encryptor.mjs
 *
 * Node 23+ runs the TypeScript import directly (type stripping); no build step.
 */
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, writeFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..');
const {
  webEncryptorFactory,
  WEB_ENCRYPTOR_OVERHEAD,
  WEB_ENCRYPTOR_HAS_FOOTER,
  WEB_ENCRYPTOR_FOOTER_LEN,
} = await import(join(root, 'example-app/web-encryptor.ts'));

// The key/nonce come from App.tsx so this can never drift from the app.
const appSource = readFileSync(join(root, 'example-app/App.tsx'), 'utf8');
const grab = (name) => {
  const m = appSource.match(new RegExp(`${name}\\s*=\\s*'([0-9a-fA-F]+)'`));
  if (!m) throw new Error(`could not read ${name} from App.tsx`);
  return m[1];
};
const keyHex = grab('CUSTOM_KEY_HEX');
const nonceHex = grab('CUSTOM_NONCE_HEX');

const SERVER_PART_SIZE = 1024 * 1024; // stands in for the backend's part size
const PLAINTEXT_SIZE = 1_500_000; // forces two parts, last one small

// ---------------------------------------------------------------------------
// 1. Build the encryptor exactly like the web build does
//
// The app registers `webEncryptorFactory.toString()`; the worker evaluates that
// source with `new Function`. Reproduce exactly that, so this fails if the
// factory ever stops being self-contained (e.g. someone uses async/await again
// and the bundler compiles it into _asyncToGenerator2 calls the worker lacks).
// ---------------------------------------------------------------------------
const source = webEncryptorFactory.toString();
// Only the factory's own code matters: the embedded glue is a string literal
// (and its comments mention "async"), so strip literals and comments first.
const codeOnly = source
  .replace(/\/\*[\s\S]*?\*\//g, '')
  .replace(/\/\/[^\n]*/g, '')
  .replace(/"(?:[^"\\]|\\.)*"/g, '""')
  .replace(/'(?:[^'\\]|\\.)*'/g, "''")
  .replace(/`(?:[^`\\]|\\.)*`/g, '``');
if (/\b(async|await)\b/.test(codeOnly)) {
  throw new Error('factory source contains async/await — it would depend on a bundler helper in the worker');
}
if (/_(asyncToGenerator|regeneratorRuntime|toConsumableArray|objectSpread)\b/.test(codeOnly)) {
  throw new Error('factory source references a bundler helper');
}
const rebuilt = new Function('return (' + source + ')')();
if (typeof rebuilt !== 'function') throw new Error('serialized factory did not evaluate back to a function');

const hexBytes = (hex) => {
  if (hex.length % 2 !== 0) throw new Error('odd-length hex');
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
};

// Per-upload factory: the uploader calls it with the key/nonce bytes of that
// upload (no key is captured at registration any more).
const impl = rebuilt(hexBytes(keyHex), hexBytes(nonceHex));
if (!impl) throw new Error('factory returned null for valid key material');
console.log(`self-contained: yes (${source.length} bytes of source, no async/await, no helpers)`);
const overhead = WEB_ENCRYPTOR_OVERHEAD;
const footerLen = WEB_ENCRYPTOR_HAS_FOOTER ? WEB_ENCRYPTOR_FOOTER_LEN : 0;

// The uploader reserves overhead + descriptor in every part, so the usable part
// size is smaller than the slot the backend presigns.
const effective = SERVER_PART_SIZE - overhead - footerLen;
if (effective <= 0) throw new Error('part size too small for the declared overhead');

const plaintext = new Uint8Array(PLAINTEXT_SIZE);
for (let i = 0; i < plaintext.length; i++) {
  plaintext[i] = (i * 2654435761) >>> 24;
}

const nDataParts = Math.max(1, Math.ceil(PLAINTEXT_SIZE / effective));

// ---------------------------------------------------------------------------
// 2. Produce the object: encrypt every part, fold the descriptor into the last
// ---------------------------------------------------------------------------
const parts = [];
for (let part = 1; part <= nDataParts; part++) {
  const start = (part - 1) * effective;
  const end = Math.min(part * effective, plaintext.length);
  let ciphertext = await impl.encryptChunk(plaintext.subarray(start, end), part);
  if (WEB_ENCRYPTOR_HAS_FOOTER && part === nDataParts) {
    const descriptor = await impl.computeFooter(nDataParts, effective);
    if (descriptor.length !== footerLen) {
      throw new Error(`descriptor is ${descriptor.length} bytes, footerLen declares ${footerLen}`);
    }
    ciphertext = new Uint8Array([...ciphertext, ...descriptor]);
  }
  parts.push(ciphertext);
}
const object = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
{
  let at = 0;
  for (const p of parts) {
    object.set(p, at);
    at += p.length;
  }
}

const dir = mkdtempSync(join(tmpdir(), 'web-encryptor-verify-'));
const objectPath = join(dir, 'web-object.bin');
const plaintextPath = join(dir, 'web-plaintext.bin');
writeFileSync(objectPath, object);
writeFileSync(plaintextPath, plaintext);

console.log(`scheme      : AES-256-GCM (WebCrypto, as registered for the web build)`);
console.log(`key/nonce   : read from App.tsx`);
console.log(`part size   : ${SERVER_PART_SIZE} slot − ${overhead} overhead − ${footerLen} descriptor = ${effective}`);
console.log(`parts       : ${nDataParts} data part(s) + descriptor inside part ${nDataParts}`);
console.log(`plaintext   : ${plaintext.length} bytes`);
console.log(`object      : ${object.length} bytes (plaintext + ${overhead}×${nDataParts} + ${footerLen})`);
console.log(`object at   : ${objectPath}`);
console.log('');

// ---------------------------------------------------------------------------
// 3. Verify with tools/decrypt (no --part-size: the descriptor must give it away)
// ---------------------------------------------------------------------------
const tool = join(root, 'tools/decrypt/target/release/decrypt-verify');
if (!existsSync(tool)) {
  throw new Error(`verifier not built: run 'cargo build --release' in tools/decrypt`);
}
const out = execFileSync(
  tool,
  [objectPath, '--key', keyHex, '--nonce', nonceHex, '--expect', plaintextPath],
  { encoding: 'utf8' },
);
console.log(out.trim());
