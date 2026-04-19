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
import { pick, isCancel, types } from '@react-native-documents/picker';
import type { UploadProgress, AggregateProgress } from '../src/specs/s3-bg-uploader.types';

// ---------------------------------------------------------------------------
// Config — point these at your own backend
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
  // native path or web File object
  path?: string;
  webFile?: File;
  transferId: string;
  // filled once upload starts
  fileKey?: string;
  progress?: UploadProgress;
}

type SessionState = 'idle' | 'running' | 'paused';

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------
function App(): React.JSX.Element {
  const [queue, setQueue] = useState<QueuedFile[]>([]);
  const [sessionProgress, setSessionProgress] = useState<AggregateProgress | null>(null);
  // transferId -> AggregateProgress
  const [transferProgress, setTransferProgress] = useState<Record<string, AggregateProgress>>({});
  const [sessionState, setSessionState] = useState<SessionState>('idle');
  const [errorMsg, setErrorMsg] = useState('');
  const [transferIdInput, setTransferIdInput] = useState('transfer-1');

  // Web only: hidden file input
  const webInputRef = useRef<HTMLInputElement | null>(null);
  // pending transferId while web picker is open
  const pendingTransferIdRef = useRef<string>('');

  useEffect(() => {
    S3BgUploader.setConfig(START_UPLOAD_API, GET_UPLOAD_URLS_API, COMPLETE_API);
    S3BgUploader.setTaskSubtitle(
      '{percentage} | {uploadedSize}/{totalSize} | {completedTransfers}/{totalTransfers} transfers | {completedFiles}/{totalFiles} files',
    );

    if (Platform.OS === 'android' && Platform.Version >= 33) {
      PermissionsAndroid.request(PermissionsAndroid.PERMISSIONS.POST_NOTIFICATIONS);
    }
  }, []);

  // ------------------------------------------------------------------
  // Progress callback — kept alive for the whole upload session
  // ------------------------------------------------------------------
  const startListening = useCallback(() => {
    S3BgUploader.setProgressCallback((fp, sessionAgg, transferAgg) => {
      console.log("file", fp)
      console.log("transfer", transferAgg)
      console.log("session", sessionAgg)
      setSessionProgress({ ...sessionAgg });
      setTransferProgress((prev) => ({
        ...prev,
        [fp.transferId]: { ...transferAgg },
      }));

      setQueue((prev) =>
        prev.map((item) => {
          if (item.fileKey && item.fileKey === fp.fileKey) {
            return { ...item, progress: { ...fp } };
          }
          return item;
        }),
      );

      // Stop listening once everything is done
      if (sessionAgg.state === 'COMPLETED') {
        setSessionState('idle');
        S3BgUploader.setProgressCallback(null);
      } else if (sessionAgg.state === 'FAILED' || sessionAgg.state === 'PAUSED') {
        // Keep session active so the user can resume
        setSessionState((prev) => (prev === 'running' ? 'paused' : prev));
      } else {
        setSessionState('running');
      }
    });
  }, []);

  // ------------------------------------------------------------------
  // Picking files
  // ------------------------------------------------------------------
  const handleAddFilesNative = useCallback(async () => {
    try {
      const results = await pick({
        mode: 'open',
        type: [types.allFiles],
        allowMultiSelection: true,
      });
      const newFiles: QueuedFile[] = results.map((r) => ({
        id: `${Date.now()}-${Math.random()}`,
        name: r.name ?? r.uri,
        path: uriToPath(r.uri),
        transferId: transferIdInput.trim() || nextTransferId(),
      }));
      setQueue((prev) => [...prev, ...newFiles]);
    } catch (e) {
      if (!isCancel(e)) {
        setErrorMsg((e as Error)?.message ?? String(e));
      }
    }
  }, [transferIdInput]);

  const handleAddFilesWeb = useCallback(() => {
    pendingTransferIdRef.current = transferIdInput.trim() || nextTransferId();
    webInputRef.current?.click();
  }, [transferIdInput]);

  const handleWebFileChange = useCallback(
    (event: React.ChangeEvent<HTMLInputElement>) => {
      const files = event.target.files;
      if (!files || files.length === 0) return;
      const newFiles: QueuedFile[] = Array.from(files).map((f) => ({
        id: `${Date.now()}-${Math.random()}`,
        name: f.name,
        webFile: f,
        transferId: pendingTransferIdRef.current,
      }));
      setQueue((prev) => [...prev, ...newFiles]);
      // Reset so the same file can be picked again later
      event.target.value = '';
    },
    [],
  );

  const handleAddFiles =
    Platform.OS === 'web' ? handleAddFilesWeb : handleAddFilesNative;

  // ------------------------------------------------------------------
  // Upload all queued files
  // ------------------------------------------------------------------
  const handleUploadAll = useCallback(async () => {
    const pending = queue.filter((f) => !f.fileKey);
    if (pending.length === 0) return;

    setErrorMsg('');
    setSessionState('running');
    startListening();

    try {
      if (Platform.OS === 'web') {
        // Start all web uploads concurrently — wasm_start_file resolves quickly (~100ms)
        await Promise.all(
          pending.map(async (item) => {
            const fileKey = await (S3BgUploader as any).uploadFile(item.webFile!, item.transferId);
            setQueue((prev) =>
              prev.map((q) => (q.id === item.id ? { ...q, fileKey } : q)),
            );
          }),
        );
      } else {
        for (const item of pending) {
          const fileKey = (S3BgUploader as any).uploadFile(item.path!, item.transferId);
          setQueue((prev) =>
            prev.map((q) => (q.id === item.id ? { ...q, fileKey } : q)),
          );
        }
      }
    } catch (e: unknown) {
      setErrorMsg((e as Error)?.message ?? String(e));
      setSessionState('idle');
      S3BgUploader.setProgressCallback(null);
    }
  }, [queue, startListening]);

  // ------------------------------------------------------------------
  // Session controls
  // ------------------------------------------------------------------
  const handlePause = useCallback(() => {
    S3BgUploader.pause();
    setSessionState('paused');
  }, []);

  const handleResume = useCallback(() => {
    S3BgUploader.resume();
    setSessionState('running');
  }, []);

  const handleCancelAll = useCallback(() => {
    S3BgUploader.cancel();
    setQueue([]);
    setSessionProgress(null);
    setTransferProgress({});
    setSessionState('idle');
    S3BgUploader.setProgressCallback(null);
  }, []);

  // ------------------------------------------------------------------
  // Per-file / per-transfer controls
  // ------------------------------------------------------------------
  const handleCancelFile = useCallback((item: QueuedFile) => {
    if (item.fileKey) {
      S3BgUploader.cancelFile(item.fileKey);
    }
    setQueue((prev) => prev.filter((q) => q.id !== item.id));
  }, []);

  const handleCancelTransfer = useCallback((transferId: string) => {
    S3BgUploader.cancelTransfer(transferId);
    setQueue((prev) => prev.filter((q) => q.transferId !== transferId));
  }, []);

  const handleRemoveQueued = useCallback((id: string) => {
    setQueue((prev) => prev.filter((q) => q.id !== id));
  }, []);

  // ------------------------------------------------------------------
  // Helpers
  // ------------------------------------------------------------------
  const sessionPercent = sessionProgress
    ? Math.round(sessionProgress.percentage)
    : 0;

  const transferIds = Array.from(new Set(queue.map((f) => f.transferId)));

  // ------------------------------------------------------------------
  // Render
  // ------------------------------------------------------------------
  return (
    <ScrollView contentContainerStyle={styles.container}>
      <Text style={styles.title}>S3 Uploader</Text>

      {/* Hidden web file input */}
      {Platform.OS === 'web' && (
        <input
          ref={webInputRef}
          type="file"
          multiple
          style={{ display: 'none' }}
          onChange={
            handleWebFileChange as unknown as React.ChangeEventHandler<HTMLInputElement>
          }
        />
      )}

      {/* Transfer ID input + Add Files */}
      <View style={styles.row}>
        <TextInput
          style={styles.transferInput}
          value={transferIdInput}
          onChangeText={setTransferIdInput}
          placeholder="Transfer-ID"
          placeholderTextColor="#aaa"
          editable={sessionState === 'idle'}
        />
        <TouchableOpacity
          style={[styles.addButton, sessionState === 'running' && styles.buttonDisabled]}
          onPress={handleAddFiles}
          disabled={sessionState === 'running'}>
          <Text style={styles.addButtonText}>+ Dateien</Text>
        </TouchableOpacity>
      </View>

      {/* Queue list grouped by transfer */}
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
                  <Text style={styles.transferStats}>
                    {tPct}% · {formatBytes(tp.uploadedSize)}/{formatBytes(tp.totalSize)}
                    {'  '}{tp.completedFiles}/{tp.totalFiles} Dateien
                  </Text>
                )}
                {tp && (
                  <View style={styles.transferTrack}>
                    <View
                      style={[
                        styles.transferFill,
                        // eslint-disable-next-line react-native/no-inline-styles
                        { width: `${tPct}%` as unknown as number },
                        tp.state === 'COMPLETED' && styles.miniFillDone,
                        tp.state === 'FAILED' && styles.miniFillFail,
                      ]}
                    />
                  </View>
                )}
              </View>
              <TouchableOpacity
                style={styles.cancelTransferBtn}
                onPress={() => handleCancelTransfer(tid)}>
                <Text style={styles.cancelBtnText}>Abbrechen</Text>
              </TouchableOpacity>
            </View>

            {filesInTransfer.map((item) => {
              const pct = item.progress
                ? Math.round(item.progress.percentage)
                : 0;
              const state = item.progress?.state;
              const isActive = !!item.fileKey;
              return (
                <View key={item.id} style={styles.fileRow}>
                  <View style={styles.fileInfo}>
                    <Text style={styles.fileName} numberOfLines={1}>
                      {item.name}
                    </Text>
                    {isActive && item.progress ? (
                      <View style={styles.fileProgressRow}>
                        <View style={styles.miniTrack}>
                          <View
                            style={[
                              styles.miniFill,
                              // eslint-disable-next-line react-native/no-inline-styles
                              { width: `${pct}%` as unknown as number },
                              state === 'COMPLETED' && styles.miniFillDone,
                              state === 'FAILED' && styles.miniFillFail,
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
                      <Text style={styles.fileQueued}>Warteschlange</Text>
                    )}
                  </View>
                  <TouchableOpacity
                    style={styles.cancelFileBtn}
                    onPress={() =>
                      isActive
                        ? handleCancelFile(item)
                        : handleRemoveQueued(item.id)
                    }>
                    <Text style={styles.cancelFileBtnText}>✕</Text>
                  </TouchableOpacity>
                </View>
              );
            })}
          </View>
        );
      })}

      {queue.length === 0 && (
        <Text style={styles.emptyHint}>
          Wähle Dateien aus und weise sie einem Transfer zu.
        </Text>
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
            {formatBytes(sessionProgress.uploadedSize)} /{' '}
            {formatBytes(sessionProgress.totalSize)}
            {'  ·  '}
            {sessionProgress.completedFiles}/{sessionProgress.totalFiles} Dateien
          </Text>
        </View>
      )}

      {/* Action buttons */}
      <View style={styles.actionRow}>
        {/* Upload */}
        <TouchableOpacity
          style={[
            styles.actionBtn,
            styles.uploadBtn,
            (queue.length === 0 || sessionState === 'running') &&
              styles.buttonDisabled,
          ]}
          onPress={handleUploadAll}
          disabled={queue.length === 0 || sessionState === 'running'}>
          {sessionState === 'running' ? (
            <ActivityIndicator color="#fff" />
          ) : (
            <Text style={styles.actionBtnText}>Hochladen</Text>
          )}
        </TouchableOpacity>

        {/* Pause / Resume */}
        {sessionState === 'running' && (
          <TouchableOpacity
            style={[styles.actionBtn, styles.pauseBtn]}
            onPress={handlePause}>
            <Text style={styles.actionBtnText}>Pause</Text>
          </TouchableOpacity>
        )}
        {sessionState === 'paused' && (
          <TouchableOpacity
            style={[styles.actionBtn, styles.resumeBtn]}
            onPress={handleResume}>
            <Text style={styles.actionBtnText}>Fortsetzen</Text>
          </TouchableOpacity>
        )}

        {/* Cancel all */}
        {sessionState !== 'idle' && (
          <TouchableOpacity
            style={[styles.actionBtn, styles.cancelAllBtn]}
            onPress={handleCancelAll}>
            <Text style={styles.actionBtnText}>Alles abbrechen</Text>
          </TouchableOpacity>
        )}
      </View>

      {errorMsg ? (
        <Text style={styles.errorText}>{errorMsg}</Text>
      ) : null}
    </ScrollView>
  );
}

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------
const styles = StyleSheet.create({
  container: {
    flexGrow: 1,
    padding: 20,
    backgroundColor: '#f0f2f5',
  },
  title: {
    fontSize: 26,
    fontWeight: '700',
    marginBottom: 20,
    color: '#111',
    textAlign: 'center',
  },
  // Transfer ID + add button row
  row: {
    flexDirection: 'row',
    alignItems: 'center',
    marginBottom: 16,
    gap: 8,
  },
  transferInput: {
    flex: 1,
    height: 42,
    borderWidth: 1.5,
    borderColor: '#007AFF',
    borderRadius: 8,
    paddingHorizontal: 10,
    fontSize: 14,
    color: '#111',
    backgroundColor: '#fff',
  },
  addButton: {
    backgroundColor: '#007AFF',
    borderRadius: 8,
    paddingVertical: 10,
    paddingHorizontal: 16,
  },
  addButtonText: {
    color: '#fff',
    fontSize: 15,
    fontWeight: '600',
  },
  // Transfer block
  transferBlock: {
    backgroundColor: '#fff',
    borderRadius: 10,
    marginBottom: 12,
    overflow: 'hidden',
    borderWidth: 1,
    borderColor: '#e0e0e0',
  },
  transferHeader: {
    flexDirection: 'row',
    justifyContent: 'space-between',
    alignItems: 'center',
    backgroundColor: '#e8f0fe',
    paddingHorizontal: 12,
    paddingVertical: 8,
  },
  transferHeaderLeft: {
    flex: 1,
    marginRight: 8,
  },
  transferStats: {
    fontSize: 11,
    color: '#4a6fa5',
    marginTop: 2,
    marginBottom: 4,
  },
  transferTrack: {
    height: 5,
    backgroundColor: '#c5d5f0',
    borderRadius: 3,
    overflow: 'hidden',
    marginBottom: 2,
  },
  transferFill: {
    height: '100%',
    backgroundColor: '#007AFF',
    borderRadius: 3,
  },
  transferLabel: {
    fontWeight: '700',
    fontSize: 13,
    color: '#1a3c7a',
  },
  cancelTransferBtn: {
    backgroundColor: '#FF3B30',
    borderRadius: 6,
    paddingVertical: 4,
    paddingHorizontal: 10,
  },
  cancelBtnText: {
    color: '#fff',
    fontSize: 12,
    fontWeight: '600',
  },
  // File row
  fileRow: {
    flexDirection: 'row',
    alignItems: 'center',
    paddingHorizontal: 12,
    paddingVertical: 10,
    borderTopWidth: 1,
    borderTopColor: '#f0f0f0',
  },
  fileInfo: {
    flex: 1,
    marginRight: 8,
  },
  fileName: {
    fontSize: 14,
    color: '#222',
    marginBottom: 4,
  },
  fileQueued: {
    fontSize: 12,
    color: '#999',
  },
  fileProgressRow: {
    flexDirection: 'row',
    alignItems: 'center',
    gap: 6,
  },
  miniTrack: {
    flex: 1,
    height: 6,
    backgroundColor: '#e0e0e0',
    borderRadius: 3,
    overflow: 'hidden',
  },
  miniFill: {
    height: '100%',
    backgroundColor: '#007AFF',
    borderRadius: 3,
  },
  miniFillDone: {
    backgroundColor: '#34C759',
  },
  miniFillFail: {
    backgroundColor: '#FF3B30',
  },
  filePct: {
    fontSize: 12,
    color: '#555',
    minWidth: 34,
    textAlign: 'right',
  },
  fileBytes: {
    fontSize: 11,
    color: '#777',
  },
  fileStateLabel: {
    fontSize: 11,
    color: '#888',
    minWidth: 70,
  },
  cancelFileBtn: {
    width: 30,
    height: 30,
    borderRadius: 15,
    backgroundColor: '#FF3B30',
    justifyContent: 'center',
    alignItems: 'center',
  },
  cancelFileBtnText: {
    color: '#fff',
    fontSize: 14,
    fontWeight: '700',
  },
  // Empty hint
  emptyHint: {
    textAlign: 'center',
    color: '#999',
    marginVertical: 24,
    fontSize: 14,
  },
  // Session progress
  sessionProgress: {
    marginVertical: 16,
    alignItems: 'center',
  },
  progressBarTrack: {
    width: '100%',
    height: 10,
    backgroundColor: '#ddd',
    borderRadius: 5,
    overflow: 'hidden',
    marginBottom: 8,
  },
  progressBarFill: {
    height: '100%',
    backgroundColor: '#007AFF',
    borderRadius: 5,
  },
  progressPercent: {
    fontSize: 22,
    fontWeight: '700',
    color: '#007AFF',
    marginBottom: 4,
  },
  progressDetails: {
    fontSize: 13,
    color: '#666',
  },
  // Action buttons
  actionRow: {
    flexDirection: 'row',
    flexWrap: 'wrap',
    gap: 10,
    justifyContent: 'center',
    marginTop: 8,
  },
  actionBtn: {
    borderRadius: 10,
    paddingVertical: 12,
    paddingHorizontal: 24,
    minWidth: 120,
    alignItems: 'center',
  },
  actionBtnText: {
    color: '#fff',
    fontSize: 16,
    fontWeight: '600',
  },
  uploadBtn: {
    backgroundColor: '#007AFF',
  },
  pauseBtn: {
    backgroundColor: '#FF9500',
  },
  resumeBtn: {
    backgroundColor: '#34C759',
  },
  cancelAllBtn: {
    backgroundColor: '#FF3B30',
  },
  buttonDisabled: {
    opacity: 0.45,
  },
  errorText: {
    marginTop: 14,
    color: '#FF3B30',
    fontSize: 14,
    textAlign: 'center',
  },
});

export default App;
