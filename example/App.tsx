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
import type { UploadProgress, AggregateProgress } from '../src/specs/s3-bg-uploader.types';

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------
const START_UPLOAD_API = 'https://development1.ysendit.com/upload/MobileS3/startUpload';
const GET_UPLOAD_URLS_API = 'https://development1.ysendit.com/upload/MobileS3/getUploadUrls';
const COMPLETE_API = 'https://development1.ysendit.com/upload/MobileS3/complete';

let transferCounter = 0;
function nextTransferId(): string {
  return `transfer-${++transferCounter}`;
}

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
  /** True while uploadFile() is in-flight for this entry. */
  isLoading?: boolean;
  /** True if loaded from a persisted session but not yet re-provided via uploadFile(). */
  isMissing?: boolean;
}

type SessionState = 'idle' | 'running' | 'paused';

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------
export default function App(): React.JSX.Element {
  const [queue, setQueue] = useState<QueuedFile[]>([]);
  const [sessionProgress, setSessionProgress] = useState<AggregateProgress | null>(null);
  const [transferProgress, setTransferProgress] = useState<Record<string, AggregateProgress>>({});
  const [sessionState, setSessionState] = useState<SessionState>('idle');
  const [errorMsg, setErrorMsg] = useState('');
  const [transferIdInput, setTransferIdInput] = useState('transfer-1');

  const webInputRef = useRef<HTMLInputElement | null>(null);
  const pendingTransferIdRef = useRef<string>('');

  // ------------------------------------------------------------------
  // Progress callback
  // ------------------------------------------------------------------
  const startListening = useCallback(() => {
    S3BgUploader.setProgressCallback((fp, sessionAgg, transferAgg) => {
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
  }, []);

  // ------------------------------------------------------------------
  // Init: config + restore persisted session
  // ------------------------------------------------------------------
  useEffect(() => {
    S3BgUploader.setConfig(START_UPLOAD_API, GET_UPLOAD_URLS_API, COMPLETE_API);
    S3BgUploader.setTaskSubtitle(
      '{percentage} | {uploadedSize}/{totalSize} | {completedTransfers}/{totalTransfers} transfers | {completedFiles}/{totalFiles} files',
    );
    if (Platform.OS === 'android' && Platform.Version >= 33) {
      PermissionsAndroid.request(PermissionsAndroid.PERMISSIONS.POST_NOTIFICATIONS);
    }
  }, []);

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
  // Core: run uploadFile for an item already in queue (id known)
  // Concurrently with other uploads — each item updates independently.
  // ------------------------------------------------------------------
  const runUpload = useCallback(
    async (id: string, fileOrPath: File | string, transferId: string) => {
      try {
        const fileHash = await S3BgUploader.uploadFile(fileOrPath, transferId);
        setQueue((prev) => {
          // Remove any other entry with the same hash (same file re-provided after restart).
          // Must use f.id === id guard so we keep the current placeholder even if the
          // NOT_STARTED callback already cleared isMissing on the session-restored entry.
          const withoutDuplicate = prev.filter(
            (f) => f.id === id || f.fileHash !== fileHash,
          );
          return withoutDuplicate.map((q) =>
            q.id === id ? { ...q, fileHash, isLoading: false } : q,
          );
        });
      } catch (e) {
        // Remove the loading placeholder on error.
        setQueue((prev) => prev.filter((q) => q.id !== id));
        setErrorMsg((e as Error)?.message ?? String(e));
      }
    },
    [],
  );

  // ------------------------------------------------------------------
  // Picking files — add as loading immediately, then upload concurrently
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
        isLoading: true,
      }));
      setQueue((prev) => [...prev, ...newItems]);
      startListening();
      newItems.forEach((item) => runUpload(item.id, item.path!, item.transferId));
    } catch (e) {
      setErrorMsg((e as Error)?.message ?? String(e));
    }
  }, [transferIdInput, startListening, runUpload]);

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
        isLoading: true,
      }));
      setQueue((prev) => [...prev, ...newItems]);
      startListening();
      newItems.forEach((item) => runUpload(item.id, item.webFile!, item.transferId));
      event.target.value = '';
    },
    [startListening, runUpload],
  );

  const handleAddFiles = Platform.OS === 'web' ? handleAddFilesWeb : handleAddFilesNative;

  // ------------------------------------------------------------------
  // Start upload (only "ready" files — hashed but not yet started)
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

  // ------------------------------------------------------------------
  // Session controls
  // ------------------------------------------------------------------
  const handlePause = useCallback(() => {
    S3BgUploader.pause();
    setSessionState('paused');
  }, []);

  const handleResume = useCallback(async () => {
    setErrorMsg('');
    try {
      await S3BgUploader.resume();
      setSessionState('running');
    } catch (e: unknown) {
      setErrorMsg((e as Error)?.message ?? String(e));
    }
  }, []);

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
      <Text style={styles.title}>S3 Uploader</Text>

      {Platform.OS === 'web' && (
        <input
          ref={webInputRef}
          type="file"
          multiple
          style={{ display: 'none' }}
          onChange={handleWebFileChange as unknown as React.ChangeEventHandler<HTMLInputElement>}
        />
      )}

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
          <Text style={styles.addButtonText}>+ Dateien</Text>
        </TouchableOpacity>
      </View>

      {/* Missing-files banner */}
      {hasMissing && (
        <View style={styles.missingBanner}>
          <Text style={styles.missingBannerText}>
            ⚠ Dateien aus letzter Sitzung fehlen — erneut auswählen oder abbrechen.
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
                      {'  '}{tp.completedFiles}/{tp.totalFiles} Dateien
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
                <Text style={styles.cancelBtnText}>Abbrechen</Text>
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
                      {item.isMissing && <Text style={styles.missingTag}>Fehlt</Text>}
                      {item.isLoading && <ActivityIndicator size="small" color="#007AFF" />}
                    </View>

                    {item.isLoading ? (
                      <Text style={styles.fileStatusText}>Wird verarbeitet…</Text>
                    ) : item.isMissing ? (
                      <Text style={styles.missingHint}>Erneut auswählen oder abbrechen</Text>
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
                      <Text style={styles.fileStatusText}>Bereit</Text>
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
        <Text style={styles.emptyHint}>Wähle Dateien aus und weise sie einem Transfer zu.</Text>
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
            {sessionProgress.completedFiles}/{sessionProgress.totalFiles} Dateien
          </Text>
        </View>
      )}

      {/* Action buttons */}
      <View style={styles.actionRow}>
        {/* Hochladen — only when there are ready files and not yet running */}
        {hasReady && sessionState !== 'running' && (
          <TouchableOpacity
            style={[styles.actionBtn, styles.uploadBtn, isLoading && styles.buttonDisabled]}
            onPress={handleUploadAll}
            disabled={isLoading}>
            <Text style={styles.actionBtnText}>Hochladen</Text>
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
            <Text style={styles.actionBtnText}>Fortsetzen</Text>
          </TouchableOpacity>
        )}

        {(sessionState !== 'idle' || queue.length > 0) && (
          <TouchableOpacity style={[styles.actionBtn, styles.cancelAllBtn]} onPress={handleCancelAll}>
            <Text style={styles.actionBtnText}>Alles abbrechen</Text>
          </TouchableOpacity>
        )}
      </View>

      {errorMsg ? <Text style={styles.errorText}>{errorMsg}</Text> : null}
    </ScrollView>
  );
}

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------
const styles = StyleSheet.create({
  container: { flexGrow: 1, padding: 20, backgroundColor: '#f0f2f5' },
  title: { fontSize: 26, fontWeight: '700', marginBottom: 20, color: '#111', textAlign: 'center' },
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
});
