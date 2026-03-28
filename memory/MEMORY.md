# react-native-s3-bg-uploader

## Projektstruktur
- Library root: `/Users/I766487/devPrivat/react-native-s3-bg-uploader/`
- Example App: `example/App.tsx`
- TypeScript-Specs: `src/specs/s3-bg-uploader.types.ts` + `s3-bg-uploader.nitro.ts`
- Native iOS: `ios/HybridS3BgUploader.swift`
- Web: `src/index.web.ts` (lazy-loads WASM pkg)
- Native entry: `src/index.ts`
- Rust core: `uploader/src/` (ios.rs, wasm.rs, core/)

## Wichtige Kommandos
- `npm run codegen` – nitrogen generieren + build (aus root)
- `npm run example:ios` – iOS build (Rust + codegen + pod + run)
- `npm run build:rust:ios` – nur Rust für iOS bauen

## Architektur
### iOS (Rust C-FFI)
- `upload_file(path: *const c_char) -> i32` – blockierend, läuft in Promise.async Thread
- `set_progress_callback(cb: Option<ProgressCallback>)` – C-Funktionszeiger
- Callback-Signatur: `(totalBytes: u64, uploadedBytes: u64, completedParts: u32, totalParts: u32, percentage: f64)`
- Swift braucht `@convention(c)` Wrapper + globale Variable für Closure-Speicherung

### Web (WASM via wasm-bindgen)
- `upload_file(file: web_sys::File)` – async, nimmt Browser File-Objekt
- `set_progress_callback(cb: Option<Function>)` – JS-Funktion
- Progress-Objekt: `{totalBytes, uploadedBytes, completedParts, totalParts, percentage}`
- WASM pkg liegt nach Build in `uploader/pkg/uploader` (wasm-pack build --target web)

## Nitro Codegen
- Nach Änderung der Nitro-Spec muss `npm run codegen` laufen
- Generierte Dateien in `nitrogen/generated/` – NICHT manuell editieren
- Swift-Spec: `HybridS3BgUploaderSpec.swift` in nitrogen/generated/ios/swift/

## File Picker
- `react-native-document-picker` installiert (in root workspace)
- iOS: `pickSingle({ copyTo: 'cachesDirectory' })` → `fileCopyUri` verwenden
- URI-zu-Pfad: `file://` prefix entfernen + URL-Decode für Rust
- Web: versteckter `<input type="file">` via ref.click()

## Exports
- `S3BgUploader` – HybridObject (iOS) / Web-Stub
- `uploadWebFile(file: File)` – nur Web (echte Impl in index.web.ts, Stub in index.ts)
- `UploadProgress`, `ProgressCallback` Types

## WASM Build
- `cd uploader && wasm-pack build --target web` → `pkg/` Ordner
- Dann: `uploader/pkg/uploader` importierbar

## Ausstehend nach dem Stand dieses Gesprächs
- `npm run codegen` ausführen damit Nitro Swift-Bridge `uploadFile` + `setProgressCallback` kennt
- WASM bauen: `wasm-pack build --target web --features wasm`
- Pod install nach Codegen: `npm run pod` in example/
