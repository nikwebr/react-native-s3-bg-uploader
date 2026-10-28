/**
 * Custom Encryption Example App
 *
 * Demonstrates how to use a custom AES-256-GCM encryptor with
 * react-native-s3-bg-uploader.
 *
 * # Architecture
 *
 * 1. **Custom Encryptor (Rust)**: AES-256-GCM implementation that runs in native code.
 * 2. **Registration**: Called at app startup from Swift/Kotlin/TypeScript.
 * 3. **Upload**: Files are encrypted chunk-by-chunk in Rust before uploading to S3.
 * 4. **Verification**: Decrypted files can be verified with the same key/nonce.
 *
 * # Quick Start
 *
 * ```bash
 * # 1. Build the custom encryptor library
 * cd custom-encryptor-rust
 * cargo build --release --target aarch64-apple-ios  # iOS
 * cargo build --release --target aarch64-linux-android  # Android
 *
 * # 2. Link the library to your app
 * # iOS: Add libcustom_encryptor.a to Xcode
 * # Android: Add libcustom_encryptor.so to jniLibs
 *
 * # 3. Register at app startup
 * CustomEncryptor.register()  # iOS + Android: factory only, no key material
 *
 * # 4. Upload with encryption
 * S3BgUploader.encryptAndUploadFile(file, transferId, keyHex, nonceHex)
 * ```
 */

import React, { useState, useCallback, useRef, useEffect } from 'react';
import {
  Text,
  View,
  StyleSheet,
  TouchableOpacity,
  Platform,
  ActivityIndicator,
  ScrollView,
  PermissionsAndroid,
  TextInput,
} from 'react-native';
import { S3BgUploader } from 'react-native-s3-bg-uploader';
import { pick, types } from '@react-native-documents/picker';
// Browser-only: the WASM implementation of the custom encryptor, registered
// in the effect below. The Rust code in custom-encryptor-rust/ is compiled to
// WebAssembly and embedded here, so the same encryption logic runs on all platforms.
import {
  webEncryptorFactory,
  WEB_ENCRYPTOR_OVERHEAD,
  WEB_ENCRYPTOR_HAS_FOOTER,
  WEB_ENCRYPTOR_FOOTER_LEN,
} from './web-encryptor';
import type { UploadProgress, AggregateProgress } from 'react-native-s3-bg-uploader';

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

// S3 backend endpoints (replace with your own)
const START_UPLOAD_API = 'https://development1.ysendit.com/upload/MobileS3/startUpload';
const GET_UPLOAD_URLS_API = 'https://development1.ysendit.com/upload/MobileS3/getUploadUrls';
const COMPLETE_API = 'https://development1.ysendit.com/upload/MobileS3/complete';

// Custom encryption key and nonce (64 hex chars = 32 bytes, 48 hex chars = 24 bytes)
// In production, generate these securely and store in a key management system.
const CUSTOM_KEY_HEX = 'deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef';
const CUSTOM_NONCE_HEX = 'cafebabecafebabecafebabecafebabecafebabecafebabe';

// Transfer ID counter
let transferCounter = 0;
function nextTransferId(): string {
  return `transfer-${++transferCounter}`;
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B';
  const k = 1024;
  const sizes = ['B', 'KB', 'MB', 'GB'];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return `${(bytes / Math.pow(k, i)).toFixed(1)} ${sizes[i]}`;
}

function uriToPath(uri: string): string {
  return uri.startsWith('file://') ? decodeURIComponent(uri.slice(7)) : uri;
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface QueuedFile {
  id: string;
  name: string;
  path?: string;
  webFile?: File;
  transferId: string;
  fileHash?: string;
  fileKey?: string;
  progress?: UploadProgress;
  isLoading?: boolean;
  isMissing?: boolean;
  /** Whether this file uses custom encryption. */
  customEncrypted?: boolean;
  /** Custom encryption metadata for verification. */
  encryptionInfo?: {
    keyHex: string;
    nonceHex: string;
    algorithm: string;
  };
}

type SessionState = 'idle' | 'running' | 'paused';

// ---------------------------------------------------------------------------
// App Component
// ---------------------------------------------------------------------------

export default function App(): React.JSX.Element {
  const [queue, setQueue] = useState<QueuedFile[]>([]);
  const [sessionProgress, setSessionProgress] = useState<AggregateProgress | null>(null);
  const [transferProgress, setTransferProgress] = useState<Record<string, AggregateProgress>>({});
  const [sessionState, setSessionState] = useState<SessionState>('idle');
  const [errorMsg, setErrorMsg] = useState('');
  const [transferIdInput, setTransferIdInput] = useState(nextTransferId());
  const [customEncryptionEnabled, setCustomEncryptionEnabled] = useState(false);
  const [customKeyInput, setCustomKeyInput] = useState(CUSTOM_KEY_HEX);
  const [customNonceInput, setCustomNonceInput] = useState(CUSTOM_NONCE_HEX);

  const [screenLogs, setScreenLogs] = useState<string[]>([]);
  const screenLogsRef = useRef<string[]>([]);

  const addLog = useCallback((msg: string) => {
    const line = `${new Date().toISOString().slice(11, 23)} ${msg}`;
    screenLogsRef.current = [...screenLogsRef.current.slice(-80), line];
    setScreenLogs([...screenLogsRef.current]);
  }, []);

  const webInputRef = useRef<HTMLInputElement | null>(null);
  const pendingTransferIdRef = useRef<string>('');

  // ------------------------------------------------------------------
  // Initialize: Register custom encryptor + set up S3BgUploader
  // ------------------------------------------------------------------
  useEffect(() => {
    // Register custom AES-256-GCM encryptor at app startup
    if (customEncryptionEnabled) {
      registerCustomEncryptor();
    }

    // Set up S3 backend configuration
    S3BgUploader.setConfig(
      START_UPLOAD_API,
      GET_UPLOAD_URLS_API,
      COMPLETE_API
    );

    S3BgUploader.setTaskSubtitle(
      '{percentage} | {uploadedSize}/{totalSize} | {completedTransfers}/{totalTransfers} transfers'
    );

    // Request notifications permission on Android 13+
    if (Platform.OS === 'android' && Platform.Version >= 33) {
      PermissionsAndroid.request(PermissionsAndroid.PERMISSIONS.POST_NOTIFICATIONS);
    }
  }, []);

  // Web uses the same Rust AES-256-GCM encryptor compiled to WASM and embedded
  // in web-encryptor.ts. The wire format is identical across all platforms,
  // which means tools/decrypt verifies a browser upload with the same key and nonce.
  useEffect(() => {
    if (Platform.OS !== 'web') return;
    let cancelled = false;
    (async () => {
      try {
        await S3BgUploader.registerWebEncryptor({
          // Serialized factory: evaluated inside the uploader's worker, where the
          // WASM encryptor is initialized synchronously. The uploader calls it
          // once per upload with that upload's key/nonce bytes — no key is baked
          // into the registration.
          source: webEncryptorFactory.toString(),
          overhead: WEB_ENCRYPTOR_OVERHEAD,
          hasFooter: WEB_ENCRYPTOR_HAS_FOOTER,
          footerLen: WEB_ENCRYPTOR_FOOTER_LEN,
        });
        if (!cancelled) addLog('web: custom AES-256-GCM encryptor registered (WASM)');
      } catch (e) {
        if (!cancelled) {
          const msg = (e as Error)?.message ?? String(e);
          addLog(`web: registerWebEncryptor failed: ${msg}`);
          setErrorMsg(`registerWebEncryptor failed: ${msg}`);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [customKeyInput, customNonceInput, addLog]);


  // The custom AES-256-GCM *factory* is registered from native code at app
  // startup — iOS: AppDelegate → CustomEncryptor.register(); Android: the
  // equivalent in MainApplication. Nothing is registered from JS here (the Nitro
  // host object is not an RCT bridge module).
  //
  // The key/nonce fields below are the real inputs: every encrypted upload passes
  // them to encryptAndUploadFile(...), and the uploader hands those bytes to the
  // registered scheme's create callback. Two uploads may therefore use different
  // keys, and a resumed upload reuses the key stored with its session.
  const registerCustomEncryptor = useCallback(() => {
    console.log(
      '[CustomEncryption] Custom AES-256-GCM factory is registered natively at ' +
        'app startup; each upload passes its own key/nonce.',
    );
  }, []);

  // ------------------------------------------------------------------
  // Progress callback
  // ------------------------------------------------------------------
  const startListening = useCallback(() => {
    S3BgUploader.setProgressCallback((fp, sessionAgg, transferAgg) => {
      const line = `cb file=${fp.state} pct=${fp.percentage?.toFixed?.(1)} session=${sessionAgg.state} sessionPct=${sessionAgg.percentage?.toFixed?.(1)} files=${sessionAgg.completedFiles}/${sessionAgg.totalFiles}`;
      console.log(`[LAGDBG ${Date.now()}] ${line}`);
      addLog(line);
      setSessionProgress({ ...sessionAgg });
      setTransferProgress((prev) => ({ ...prev, [fp.transferId]: { ...transferAgg } }));

      setQueue((prev) =>
        prev.map((item) => {
          if (item.fileHash && item.fileHash === fp.fileHash) {
            return { ...item, fileKey: fp.fileKey ?? item.fileKey, isMissing: false, progress: { ...fp } };
          }
          return item;
        }),
      );

      if (sessionAgg.state === 'COMPLETED') {
        setSessionState('idle');
        S3BgUploader.setProgressCallback(null);
      } else if (sessionAgg.state === 'RUNNING' || sessionAgg.state === 'RUNNING_IN_BG') {
        setSessionState('running');
      } else if (sessionAgg.state === 'FAILED' || sessionAgg.state === 'PAUSED') {
        setSessionState((prev) => (prev === 'running' ? 'paused' : prev));
      }
    });
  }, [addLog]);

  // Restore any active session persisted by the native layer (e.g. after a pause or app restart).
  // Without this, files removed from queue when the upload promise rejects on pause are never
  // re-added, so subsequent resumes make native progress but the UI never updates.
  useEffect(() => {
    const load = async () => {
      const sessionFiles = await Promise.resolve(S3BgUploader.getProgress());
      if (sessionFiles.length === 0) return;

      const loaded: QueuedFile[] = sessionFiles.map((fp) => ({
        id: `session-${fp.fileHash}`,
        name: fp.fileName || fp.fileKey?.split('/').pop() || fp.fileHash.slice(0, 12),
        transferId: fp.transferId,
        fileHash: fp.fileHash,
        fileKey: fp.fileKey,
        progress: fp,
        isMissing: fp.state !== 'COMPLETED',
      }));
      setQueue(loaded);

      const hasActive = sessionFiles.some((f) => f.state !== 'COMPLETED');
      if (hasActive) {
        setSessionState('paused');
        startListening();
      }
    };
    load();
  }, [startListening]);
  // ------------------------------------------------------------------
  const runUpload = useCallback(
    async (id: string, fileOrPath: File | string, transferId: string, customEncrypted = false) => {
      try {
        const keyHex = customKeyInput || CUSTOM_KEY_HEX;
        const nonceHex = customNonceInput || CUSTOM_NONCE_HEX;

        const fileHash = customEncrypted
          ? await S3BgUploader.encryptAndUploadFile(
              fileOrPath,
              transferId,
              keyHex,
              nonceHex
            )
          : await S3BgUploader.uploadFile(fileOrPath, transferId);

        setQueue((prev) => {
          const withoutDuplicate = prev.filter(
            (f) => f.id === id || f.fileHash !== fileHash,
          );
          return withoutDuplicate.map((q) =>
            q.id === id ? { ...q, fileHash, isLoading: false } : q,
          );
        });
      } catch (e) {
        setQueue((prev) => prev.filter((q) => q.id !== id));
        setErrorMsg((e as Error)?.message ?? String(e));
      }
    },
    [customKeyInput, customNonceInput],
  );

  // ------------------------------------------------------------------
  // File picking
  // ------------------------------------------------------------------
  const handleAddFilesNative = useCallback(async () => {
    try {
      const results = await pick({ mode: 'open', type: [types.allFiles], allowMultiSelection: true });
      const tid = transferIdInput.trim() || nextTransferId();
      const newItems: QueuedFile[] = results.map((r) => ({
        id: `${Date.now()}-${Math.random()}`,
        name: r.name ?? r.uri,
        path: uriToPath(r.uri),
        transferId: tid,
        customEncrypted: customEncryptionEnabled,
        encryptionInfo: customEncryptionEnabled
          ? {
              keyHex: customKeyInput || CUSTOM_KEY_HEX,
              nonceHex: customNonceInput || CUSTOM_NONCE_HEX,
              algorithm: 'AES-256-GCM',
            }
          : undefined,
        isLoading: true,
      }));
      setQueue((prev) => [...prev, ...newItems]);
      startListening();
      newItems.forEach((item) =>
        runUpload(item.id, item.path!, item.transferId, item.customEncrypted)
      );
    } catch (e) {
      setErrorMsg((e as Error)?.message ?? String(e));
    }
  }, [transferIdInput, customEncryptionEnabled, customKeyInput, customNonceInput, startListening, runUpload]);

  const handleAddFilesWeb = useCallback(() => {
    pendingTransferIdRef.current = transferIdInput.trim() || nextTransferId();
    webInputRef.current?.click();
  }, [transferIdInput]);

  const handleWebFileChange = useCallback(
    (event: React.ChangeEvent<HTMLInputElement>) => {
      const files = event.target.files;
      if (!files || files.length === 0) return;
      const tid = pendingTransferIdRef.current;
      const newItems: QueuedFile[] = Array.from(files).map((f) => ({
        id: `${Date.now()}-${Math.random()}`,
        name: f.name,
        webFile: f,
        transferId: tid,
        customEncrypted: customEncryptionEnabled,
        encryptionInfo: customEncryptionEnabled
          ? {
              keyHex: customKeyInput || CUSTOM_KEY_HEX,
              nonceHex: customNonceInput || CUSTOM_NONCE_HEX,
              algorithm: 'AES-256-GCM',
            }
          : undefined,
        isLoading: true,
      }));
      setQueue((prev) => [...prev, ...newItems]);
      startListening();
      newItems.forEach((item) =>
        runUpload(item.id, item.webFile!, item.transferId, item.customEncrypted)
      );
      event.target.value = '';
    },
    [customEncryptionEnabled, customKeyInput, customNonceInput, startListening, runUpload],
  );

  const handleAddFiles = Platform.OS === 'web' ? handleAddFilesWeb : handleAddFilesNative;

  // ------------------------------------------------------------------
  // Session controls
  // ------------------------------------------------------------------
  const handleUploadAll = useCallback(async () => {
    setErrorMsg('');
    setSessionState('running');
    try {
      await S3BgUploader.resume();
    } catch (e: unknown) {
      setErrorMsg((e as Error)?.message ?? String(e));
      setSessionState('paused');
    }
  }, []);

  const handlePause = useCallback(() => {
    console.log(`[LAGDBG ${Date.now()}] handlePause -> pause()`);
    addLog('PAUSE');
    S3BgUploader.pause();
    setSessionState('paused');
  }, [addLog]);

  const handleResume = useCallback(async () => {
    setErrorMsg('');
    try {
      console.log(`[LAGDBG ${Date.now()}] handleResume -> resume()`);
      addLog('RESUME called');
      await S3BgUploader.resume();
      console.log(`[LAGDBG ${Date.now()}] handleResume resume() returned`);
      addLog('RESUME returned');
      setSessionState('running');
    } catch (e: unknown) {
      setErrorMsg((e as Error)?.message ?? String(e));
    }
  }, [addLog]);

  const handleCancelAll = useCallback(() => {
    S3BgUploader.cancel();
    setQueue([]);
    setSessionProgress(null);
    setTransferProgress({});
    setSessionState('idle');
    setErrorMsg('');
    S3BgUploader.setProgressCallback(null);
  }, []);

  const handleCancelFile = useCallback((item: QueuedFile) => {
    if (item.fileHash) S3BgUploader.cancelFile(item.fileHash);
    setQueue((prev) => prev.filter((q) => q.id !== item.id));
  }, []);

  const handleCancelTransfer = useCallback((transferId: string) => {
    S3BgUploader.cancelTransfer(transferId);
    setQueue((prev) => prev.filter((q) => q.transferId !== transferId));
  }, []);

  // ------------------------------------------------------------------
  // Derived state
  // ------------------------------------------------------------------
  const sessionPercent = sessionProgress ? Math.round(sessionProgress.percentage) : 0;
  const transferIds = Array.from(new Set(queue.map((f) => f.transferId)));
  const hasMissing = queue.some((f) => f.isMissing);
  const hasReady = queue.some((f) => f.fileHash && !f.isLoading && !f.isMissing);
  const isLoading = queue.some((f) => f.isLoading);

  // ------------------------------------------------------------------
  // Render
  // ------------------------------------------------------------------
  return (
    <ScrollView contentContainerStyle={styles.container}>
      <Text style={styles.title}>Custom Encryption Example</Text>

      {Platform.OS === 'web' && (
        <input
          ref={webInputRef}
          type="file"
          multiple
          style={{ display: 'none' }}
          onChange={handleWebFileChange as unknown as React.ChangeEventHandler<HTMLInputElement>}
        />
      )}

      {/* Custom Encryption Toggle */}
      <View style={styles.encryptionSection}>
        <Text style={styles.sectionTitle}>Custom Encryption</Text>
        <View style={styles.row}>
          <TouchableOpacity
            style={[styles.encryptToggle, customEncryptionEnabled && styles.encryptToggleOn]}
            onPress={() => setCustomEncryptionEnabled((v) => !v)}>
            <Text style={styles.encryptToggleText}>
              {customEncryptionEnabled ? '🔒 Custom AES-256-GCM' : '🔓 Standard (no encryption)'}
            </Text>
          </TouchableOpacity>
        </View>

        {customEncryptionEnabled && (
          <View style={styles.cryptoCard}>
            <Text style={styles.cryptoCardTitle}>Encryption Parameters</Text>

            <Text style={styles.cryptoLabel}>Key (32 bytes, hex)</Text>
            <TextInput
              style={styles.cryptoInput}
              value={customKeyInput}
              onChangeText={setCustomKeyInput}
              placeholder="deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
              placeholderTextColor="#6b7280"
              autoCapitalize="none"
              autoCorrect={false}
              editable
            />

            <Text style={styles.cryptoLabel}>Nonce (24 bytes, hex)</Text>
            <TextInput
              style={styles.cryptoInput}
              value={customNonceInput}
              onChangeText={setCustomNonceInput}
              placeholder="cafebabecafebabecafebabecafebabecafebabecafebabe"
              placeholderTextColor="#6b7280"
              autoCapitalize="none"
              autoCorrect={false}
              editable
            />

            <View style={styles.cryptoInfoRow}>
              <Text style={styles.cryptoInfoLabel}>Algorithm:</Text>
              <Text style={styles.cryptoInfoValue}>AES-256-GCM</Text>
            </View>
            <View style={styles.cryptoInfoRow}>
              <Text style={styles.cryptoInfoLabel}>Registered:</Text>
              <Text style={styles.cryptoInfoValue}>
                {Platform.OS === 'web'
                  ? 'registerWebEncryptor() (WASM)'
                  : 'app startup (AppDelegate)'}
              </Text>
            </View>
            <View style={styles.cryptoInfoRow}>
              <Text style={styles.cryptoInfoLabel}>Tag size:</Text>
              <Text style={styles.cryptoInfoValue}>16 bytes (128 bits)</Text>
            </View>
            <View style={styles.cryptoInfoRow}>
              <Text style={styles.cryptoInfoLabel}>Platform:</Text>
              <Text style={styles.cryptoInfoValue}>
                {Platform.OS === 'web'
                  ? 'WASM (Rust AES-256-GCM)'
                  : `${Platform.OS} (native library)`}
              </Text>
            </View>
          </View>
        )}
      </View>

      {/* Transfer ID + Add Files */}
      <View style={styles.row}>
        <TextInput
          style={styles.transferInput}
          value={transferIdInput}
          onChangeText={setTransferIdInput}
          placeholder="Transfer-ID"
          placeholderTextColor="#aaa"
          editable={sessionState === 'idle'}
        />
        <TouchableOpacity style={styles.addButton} onPress={handleAddFiles}>
          <Text style={styles.addButtonText}>+ Files</Text>
        </TouchableOpacity>
      </View>

      {/* Missing-files banner */}
      {hasMissing && (
        <View style={styles.missingBanner}>
          <Text style={styles.missingBannerText}>
            ⚠ Files from last session are missing — reselect or cancel.
          </Text>
        </View>
      )}

      {/* Queue grouped by transfer */}
      {transferIds.map((tid) => {
        const filesInTransfer = queue.filter((f) => f.transferId === tid);
        const tp = transferProgress[tid];
        const tPct = tp ? Math.round(tp.percentage) : 0;
        return (
          <View key={tid} style={styles.transferBlock}>
            <View style={styles.transferHeader}>
              <View style={styles.transferHeaderLeft}>
                <Text style={styles.transferLabel}>{tid}</Text>
                {tp && (
                  <>
                    <Text style={styles.transferStats}>
                      {tPct}% · {formatBytes(tp.uploadedSize)}/{formatBytes(tp.totalSize)}
                      {'  '}{tp.completedFiles}/{tp.totalFiles} Files
                    </Text>
                    <View style={styles.transferTrack}>
                      <View
                        style={[
                          styles.transferFill,
                          // eslint-disable-next-line react-native/no-inline-styles
                          { width: `${tPct}%` as unknown as number },
                          tp.state === 'COMPLETED' && styles.fillDone,
                          tp.state === 'FAILED' && styles.fillFail,
                        ]}
                      />
                    </View>
                  </>
                )}
              </View>
              <TouchableOpacity style={styles.cancelTransferBtn} onPress={() => handleCancelTransfer(tid)}>
                <Text style={styles.cancelBtnText}>Cancel</Text>
              </TouchableOpacity>
            </View>

            {filesInTransfer.map((item) => {
              const pct = item.progress ? Math.round(item.progress.percentage) : 0;
              const state = item.progress?.state;
              return (
                <View key={item.id} style={[styles.fileRow, item.isMissing && styles.fileRowMissing]}>
                  <View style={styles.fileInfo}>
                    <View style={styles.fileNameRow}>
                      <Text style={styles.fileName} numberOfLines={1}>{item.name}</Text>
                      {item.customEncrypted && <Text style={styles.customEncryptBadge}>🔒 Custom</Text>}
                      {item.isMissing && <Text style={styles.missingTag}>Missing</Text>}
                      {item.isLoading && <ActivityIndicator size="small" color="#007AFF" />}
                    </View>

                    {item.isLoading ? (
                      <Text style={styles.fileStatusText}>Processing…</Text>
                    ) : item.isMissing ? (
                      <Text style={styles.missingHint}>Reselect or cancel</Text>
                    ) : item.progress && item.progress.state !== 'NOT_STARTED' ? (
                      <View style={styles.fileProgressRow}>
                        <View style={styles.miniTrack}>
                          <View
                            style={[
                              styles.miniFill,
                              // eslint-disable-next-line react-native/no-inline-styles
                              { width: `${pct}%` as unknown as number },
                              state === 'COMPLETED' && styles.fillDone,
                              state === 'FAILED' && styles.fillFail,
                            ]}
                          />
                        </View>
                        <Text style={styles.filePct}>{pct}%</Text>
                        <Text style={styles.fileBytes}>
                          {formatBytes(item.progress.uploadedBytes)}/{formatBytes(item.progress.totalBytes)}
                        </Text>
                        <Text style={styles.fileStateLabel}>{state}</Text>
                      </View>
                    ) : (
                      <Text style={styles.fileStatusText}>Ready</Text>
                    )}

                    {item.customEncrypted && (
                      <View style={styles.cryptoMetaCard}>
                        <Text style={styles.cryptoMetaTitle}>Encryption Details</Text>
                        <Text style={styles.cryptoMetaLabel}>Algorithm: {item.encryptionInfo?.algorithm}</Text>
                        <Text style={styles.cryptoMetaLabel}>Key: {item.encryptionInfo?.keyHex?.slice(0, 16)}...</Text>
                        <Text style={styles.cryptoMetaLabel}>Nonce: {item.encryptionInfo?.nonceHex?.slice(0, 16)}...</Text>
                        {item.fileKey && (
                          <Text style={styles.cryptoMetaLabel}>S3 Key: {item.fileKey}</Text>
                        )}
                      </View>
                    )}
                  </View>

                  {!item.isLoading && (
                    <TouchableOpacity
                      style={[styles.cancelFileBtn, item.isMissing && styles.cancelFileBtnMissing]}
                      onPress={() => handleCancelFile(item)}>
                      <Text style={styles.cancelFileBtnText}>✕</Text>
                    </TouchableOpacity>
                  )}
                </View>
              );
            })}
          </View>
        );
      })}

      {queue.length === 0 && (
        <Text style={styles.emptyHint}>Select files and assign them to a transfer.</Text>
      )}

      {/* Session progress bar */}
      {sessionProgress && (
        <View style={styles.sessionProgress}>
          <View style={styles.progressBarTrack}>
            <View
              style={[
                styles.progressBarFill,
                // eslint-disable-next-line react-native/no-inline-styles
                { width: `${sessionPercent}%` as unknown as number },
              ]}
            />
          </View>
          <Text style={styles.progressPercent}>{sessionPercent}%</Text>
          <Text style={styles.progressDetails}>
            {formatBytes(sessionProgress.uploadedSize)} / {formatBytes(sessionProgress.totalSize)}
            {'  ·  '}
            {sessionProgress.completedFiles}/{sessionProgress.totalFiles} Files
          </Text>
        </View>
      )}

      {/* Action buttons */}
      <View style={styles.actionRow}>
        {hasReady && sessionState !== 'running' && (
          <TouchableOpacity
            style={[styles.actionBtn, styles.uploadBtn, isLoading && styles.buttonDisabled]}
            onPress={handleUploadAll}
            disabled={isLoading}>
            <Text style={styles.actionBtnText}>Upload</Text>
          </TouchableOpacity>
        )}

        {sessionState === 'running' && (
          <TouchableOpacity style={[styles.actionBtn, styles.pauseBtn]} onPress={handlePause}>
            <Text style={styles.actionBtnText}>Pause</Text>
          </TouchableOpacity>
        )}

        {sessionState === 'paused' && (
          <TouchableOpacity
            style={[styles.actionBtn, styles.resumeBtn, (isLoading || hasMissing) && styles.buttonDisabled]}
            onPress={handleResume}
            disabled={isLoading || hasMissing}>
            <Text style={styles.actionBtnText}>Resume</Text>
          </TouchableOpacity>
        )}

        {(sessionState !== 'idle' || queue.length > 0) && (
          <TouchableOpacity style={[styles.actionBtn, styles.cancelAllBtn]} onPress={handleCancelAll}>
            <Text style={styles.actionBtnText}>Cancel all</Text>
          </TouchableOpacity>
        )}
      </View>

      {errorMsg ? <Text style={styles.errorText}>{errorMsg}</Text> : null}

      {/* On-screen log panel — visible without Metro */}
      {screenLogs.length > 0 && (
        <View style={styles.logBox}>
          <View style={styles.logHeader}>
            <Text style={styles.logTitle}>Log</Text>
            <TouchableOpacity onPress={() => { screenLogsRef.current = []; setScreenLogs([]); }}>
              <Text style={styles.logClear}>Clear</Text>
            </TouchableOpacity>
          </View>
          {screenLogs.slice(-30).map((l, i) => (
            <Text key={i} style={styles.logLine}>{l}</Text>
          ))}
        </View>
      )}
    </ScrollView>
  );
}

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------
const styles = StyleSheet.create({
  container: { flexGrow: 1, padding: 20, backgroundColor: '#f0f2f5' },
  title: { fontSize: 26, fontWeight: '700', marginBottom: 20, color: '#111', textAlign: 'center' },
  sectionTitle: { fontSize: 18, fontWeight: '600', color: '#333', marginBottom: 12 },
  encryptionSection: {
    backgroundColor: '#fff',
    borderRadius: 10,
    padding: 16,
    marginBottom: 16,
    borderWidth: 1,
    borderColor: '#e0e0e0',
  },
  row: { flexDirection: 'row', alignItems: 'center', marginBottom: 16, gap: 8 },
  transferInput: {
    flex: 1, height: 42, borderWidth: 1.5, borderColor: '#007AFF',
    borderRadius: 8, paddingHorizontal: 10, fontSize: 14, color: '#111', backgroundColor: '#fff',
  },
  addButton: { backgroundColor: '#007AFF', borderRadius: 8, paddingVertical: 10, paddingHorizontal: 16 },
  addButtonText: { color: '#fff', fontSize: 15, fontWeight: '600' },
  missingBanner: {
    backgroundColor: '#fff3cd', borderRadius: 8, borderLeftWidth: 4, borderLeftColor: '#FF9500',
    paddingHorizontal: 12, paddingVertical: 8, marginBottom: 12,
  },
  missingBannerText: { fontSize: 13, color: '#856404' },
  transferBlock: {
    backgroundColor: '#fff', borderRadius: 10, marginBottom: 12,
    overflow: 'hidden', borderWidth: 1, borderColor: '#e0e0e0',
  },
  transferHeader: {
    flexDirection: 'row', justifyContent: 'space-between', alignItems: 'center',
    backgroundColor: '#e8f0fe', paddingHorizontal: 12, paddingVertical: 8,
  },
  transferHeaderLeft: { flex: 1, marginRight: 8 },
  transferLabel: { fontWeight: '700', fontSize: 13, color: '#1a3c7a' },
  transferStats: { fontSize: 11, color: '#4a6fa5', marginTop: 2, marginBottom: 4 },
  transferTrack: { height: 5, backgroundColor: '#c5d5f0', borderRadius: 3, overflow: 'hidden', marginBottom: 2 },
  transferFill: { height: '100%', backgroundColor: '#007AFF', borderRadius: 3 },
  cancelTransferBtn: { backgroundColor: '#FF3B30', borderRadius: 6, paddingVertical: 4, paddingHorizontal: 10 },
  cancelBtnText: { color: '#fff', fontSize: 12, fontWeight: '600' },
  fileRow: {
    flexDirection: 'row', alignItems: 'center',
    paddingHorizontal: 12, paddingVertical: 10,
    borderTopWidth: 1, borderTopColor: '#f0f0f0',
  },
  fileRowMissing: { backgroundColor: '#fff8e1', borderLeftWidth: 3, borderLeftColor: '#FF9500' },
  fileNameRow: { flexDirection: 'row', alignItems: 'center', gap: 6, marginBottom: 4 },
  fileInfo: { flex: 1, marginRight: 8 },
  fileName: { fontSize: 14, color: '#222', flexShrink: 1 },
  missingTag: {
    fontSize: 10, color: '#FF9500', fontWeight: '700', backgroundColor: '#fff3cd',
    borderRadius: 4, paddingHorizontal: 5, paddingVertical: 1, overflow: 'hidden',
  },
  customEncryptBadge: {
    fontSize: 11, color: '#34C759', fontWeight: '600', backgroundColor: '#e8f5e9',
    borderRadius: 4, paddingHorizontal: 5, paddingVertical: 2,
  },
  fileStatusText: { fontSize: 12, color: '#999' },
  missingHint: { fontSize: 12, color: '#FF9500', fontStyle: 'italic' },
  fileProgressRow: { flexDirection: 'row', alignItems: 'center', gap: 6 },
  miniTrack: { flex: 1, height: 6, backgroundColor: '#e0e0e0', borderRadius: 3, overflow: 'hidden' },
  miniFill: { height: '100%', backgroundColor: '#007AFF', borderRadius: 3 },
  fillDone: { backgroundColor: '#34C759' },
  fillFail: { backgroundColor: '#FF3B30' },
  filePct: { fontSize: 12, color: '#555', minWidth: 34, textAlign: 'right' },
  fileBytes: { fontSize: 11, color: '#777' },
  fileStateLabel: { fontSize: 11, color: '#888', minWidth: 70 },
  cancelFileBtn: {
    width: 30, height: 30, borderRadius: 15, backgroundColor: '#FF3B30',
    justifyContent: 'center', alignItems: 'center',
  },
  cancelFileBtnMissing: { backgroundColor: '#FF9500' },
  cancelFileBtnText: { color: '#fff', fontSize: 14, fontWeight: '700' },
  emptyHint: { textAlign: 'center', color: '#999', marginVertical: 24, fontSize: 14 },
  sessionProgress: { marginVertical: 16, alignItems: 'center' },
  progressBarTrack: {
    width: '100%', height: 10, backgroundColor: '#ddd', borderRadius: 5,
    overflow: 'hidden', marginBottom: 8,
  },
  progressBarFill: { height: '100%', backgroundColor: '#007AFF', borderRadius: 5 },
  progressPercent: { fontSize: 22, fontWeight: '700', color: '#007AFF', marginBottom: 4 },
  progressDetails: { fontSize: 13, color: '#666' },
  actionRow: { flexDirection: 'row', flexWrap: 'wrap', gap: 10, justifyContent: 'center', marginTop: 8 },
  actionBtn: { borderRadius: 10, paddingVertical: 12, paddingHorizontal: 24, minWidth: 120, alignItems: 'center' },
  actionBtnText: { color: '#fff', fontSize: 16, fontWeight: '600' },
  uploadBtn: { backgroundColor: '#007AFF' },
  pauseBtn: { backgroundColor: '#FF9500' },
  resumeBtn: { backgroundColor: '#34C759' },
  cancelAllBtn: { backgroundColor: '#FF3B30' },
  buttonDisabled: { opacity: 0.45 },
  errorText: { marginTop: 14, color: '#FF3B30', fontSize: 14, textAlign: 'center' },
  encryptToggle: {
    borderRadius: 8, paddingVertical: 10, paddingHorizontal: 12,
    backgroundColor: '#e0e0e0', borderWidth: 1.5, borderColor: '#bbb',
  },
  encryptToggleOn: { backgroundColor: '#34C75922', borderColor: '#34C759' },
  encryptToggleText: { fontSize: 13, fontWeight: '600', color: '#333' },
  encryptBadge: { fontSize: 12 },

  // Custom encryption card styles
  cryptoCard: {
    backgroundColor: '#1a1a2e', borderRadius: 10, padding: 14,
    marginBottom: 14, borderWidth: 1, borderColor: '#3a3a5c',
  },
  cryptoCardTitle: { fontSize: 13, fontWeight: '700', color: '#a0a8d0', marginBottom: 10 },
  cryptoLabel: { fontSize: 11, color: '#6b7280', marginBottom: 2, marginTop: 6 },
  cryptoInput: {
    fontFamily: Platform.OS === 'ios' ? 'Menlo' : 'monospace',
    fontSize: 11, color: '#e2e8f0', letterSpacing: 0.3,
    backgroundColor: '#0f0f1e', borderRadius: 4, padding: 6,
    borderWidth: 1, borderColor: '#3a3a5c', marginBottom: 8,
  },
  cryptoInfoRow: { flexDirection: 'row', marginBottom: 4 },
  cryptoInfoLabel: { fontSize: 11, color: '#6b7280', fontWeight: '600' },
  cryptoInfoValue: { fontSize: 11, color: '#a0a8d0' },
  cryptoMetaCard: {
    backgroundColor: '#f0f2f5', borderRadius: 6, padding: 8,
    marginTop: 6, borderWidth: 1, borderColor: '#e0e0e0',
  },
  cryptoMetaTitle: { fontSize: 11, fontWeight: '600', color: '#333', marginBottom: 4 },
  cryptoMetaLabel: { fontSize: 10, color: '#666', fontFamily: Platform.OS === 'ios' ? 'Menlo' : 'monospace' },
  logBox: { marginTop: 20, backgroundColor: '#1a1a2e', borderRadius: 8, padding: 12 },
  logHeader: { flexDirection: 'row', justifyContent: 'space-between', marginBottom: 6 },
  logTitle: { fontSize: 11, color: '#888', fontWeight: '600' },
  logClear: { fontSize: 11, color: '#4a9eff' },
  logLine: { fontSize: 10, color: '#a0e8a0', fontFamily: Platform.OS === 'ios' ? 'Menlo' : 'monospace', marginBottom: 2 },
});
