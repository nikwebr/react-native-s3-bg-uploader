import React, { useState, useCallback, useRef } from 'react';
import {
  Text,
  View,
  StyleSheet,
  TouchableOpacity,
  Platform,
  ActivityIndicator,
  ScrollView,
} from 'react-native';
import { S3BgUploader } from 'react-native-s3-bg-uploader';
import { pickSingle, isCancel, types } from 'react-native-document-picker';

interface UploadProgress {
  totalBytes: number;
  uploadedBytes: number;
  completedParts: number;
  totalParts: number;
  percentage: number;
}

function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B';
  const k = 1024;
  const sizes = ['B', 'KB', 'MB', 'GB'];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return `${(bytes / Math.pow(k, i)).toFixed(1)} ${sizes[i]}`;
}

// Strip file:// prefix for native Rust C path
function uriToPath(uri: string): string {
  return uri.startsWith('file://') ? decodeURIComponent(uri.slice(7)) : uri;
}

function App(): React.JSX.Element {
  const [fileName, setFileName] = useState<string | null>(null);
  const [nativePath, setNativePath] = useState<string | null>(null);
  const [webFile, setWebFile] = useState<File | null>(null);
  const [progress, setProgress] = useState<UploadProgress | null>(null);
  const [uploading, setUploading] = useState(false);
  const [uploadState, setUploadState] = useState<'idle' | 'success' | 'error'>('idle');
  const [errorMsg, setErrorMsg] = useState('');
  const webInputRef = useRef<HTMLInputElement | null>(null);

  // iOS: use react-native-document-picker
  const handlePickNative = useCallback(async () => {
    try {
      const result = await pickSingle({ type: [types.allFiles], copyTo: 'cachesDirectory' });
      // Use fileCopyUri (local cache copy) when available, else fallback to uri
      const uri = result.fileCopyUri ?? result.uri;
      setNativePath(uriToPath(uri));
      setFileName(result.name ?? uri);
      setUploadState('idle');
      setProgress(null);
    } catch (e) {
      if (!isCancel(e)) {
        setErrorMsg((e as Error)?.message ?? String(e));
        setUploadState('error');
      }
    }
  }, []);

  // Web: trigger hidden file input
  const handlePickWeb = useCallback(() => {
    webInputRef.current?.click();
  }, []);

  const handleWebFileChange = useCallback(
    (event: React.ChangeEvent<HTMLInputElement>) => {
      const file = event.target.files?.[0];
      if (file) {
        setWebFile(file);
        setFileName(file.name);
        setUploadState('idle');
        setProgress(null);
      }
    },
    [],
  );

  const handlePick = Platform.OS === 'web' ? handlePickWeb : handlePickNative;

  const handleUpload = useCallback(async () => {
    if (Platform.OS === 'web' && !webFile) {
      setErrorMsg('Bitte zuerst eine Datei auswählen.');
      setUploadState('error');
      return;
    }
    if (Platform.OS !== 'web' && !nativePath) {
      setErrorMsg('Bitte zuerst eine Datei auswählen.');
      setUploadState('error');
      return;
    }

    setUploading(true);
    setUploadState('idle');
    setProgress(null);
    setErrorMsg('');

    S3BgUploader.setProgressCallback((p: UploadProgress) => {
      setProgress({ ...p });
    });

    try {
      if (Platform.OS === 'web') {
        await S3BgUploader.uploadFile(webFile!);
      } else {
        await S3BgUploader.uploadFile(nativePath!);
      }
      setUploadState('success');
    } catch (e: unknown) {
      setErrorMsg((e as Error)?.message ?? String(e));
      setUploadState('error');
    } finally {
      setUploading(false);
      S3BgUploader.setProgressCallback(null);
    }
  }, [webFile, nativePath]);

  const progressPercent = progress ? Math.round(progress.percentage) : 0;
  const hasFile = Platform.OS === 'web' ? !!webFile : !!nativePath;

  return (
    <ScrollView contentContainerStyle={styles.container}>
      <Text style={styles.title}>S3 Uploader</Text>

      {/* Hidden web file input */}
      {Platform.OS === 'web' && (
        <input
          ref={webInputRef}
          type="file"
          style={{ display: 'none' }}
          onChange={handleWebFileChange as unknown as React.ChangeEventHandler<HTMLInputElement>}
        />
      )}

      {/* Pick File Button */}
      <TouchableOpacity
        style={[styles.pickButton, uploading && styles.buttonDisabled]}
        onPress={handlePick}
        disabled={uploading}>
        <Text style={styles.pickButtonText}>
          {hasFile ? '📄 Andere Datei wählen' : '📂 Datei auswählen'}
        </Text>
      </TouchableOpacity>

      {fileName ? (
        <Text style={styles.fileLabel} numberOfLines={2}>
          {fileName}
        </Text>
      ) : null}

      {/* Upload Button */}
      <TouchableOpacity
        style={[
          styles.uploadButton,
          (!hasFile || uploading) && styles.buttonDisabled,
          uploadState === 'success' && styles.uploadButtonSuccess,
          uploadState === 'error' && styles.uploadButtonError,
        ]}
        onPress={handleUpload}
        disabled={!hasFile || uploading}>
        {uploading ? (
          <ActivityIndicator color="#fff" />
        ) : (
          <Text style={styles.uploadButtonText}>
            {uploadState === 'success'
              ? '✓ Hochgeladen'
              : uploadState === 'error'
              ? '✗ Nochmal versuchen'
              : 'Hochladen'}
          </Text>
        )}
      </TouchableOpacity>

      {/* Progress Bar */}
      {(uploading || progress) ? (
        <View style={styles.progressContainer}>
          <View style={styles.progressBarTrack}>
            <View
              style={[
                styles.progressBarFill,
                // eslint-disable-next-line react-native/no-inline-styles
                { width: `${progressPercent}%` as unknown as number },
              ]}
            />
          </View>
          <Text style={styles.progressPercent}>{progressPercent}%</Text>
          {progress ? (
            <Text style={styles.progressDetails}>
              {formatBytes(progress.uploadedBytes)} / {formatBytes(progress.totalBytes)}
              {'  ·  '}
              {progress.completedParts}/{progress.totalParts} Parts
            </Text>
          ) : null}
        </View>
      ) : null}

      {/* Error */}
      {uploadState === 'error' && errorMsg ? (
        <Text style={styles.errorText}>{errorMsg}</Text>
      ) : null}
    </ScrollView>
  );
}

const styles = StyleSheet.create({
  container: {
    flexGrow: 1,
    justifyContent: 'center',
    alignItems: 'center',
    padding: 24,
    backgroundColor: '#f0f2f5',
  },
  title: {
    fontSize: 28,
    fontWeight: '700',
    marginBottom: 32,
    color: '#111',
  },
  pickButton: {
    borderWidth: 1.5,
    borderColor: '#007AFF',
    borderRadius: 10,
    paddingVertical: 12,
    paddingHorizontal: 28,
    marginBottom: 12,
  },
  pickButtonText: {
    color: '#007AFF',
    fontSize: 16,
    fontWeight: '500',
  },
  fileLabel: {
    fontSize: 13,
    color: '#555',
    marginBottom: 20,
    maxWidth: 360,
    textAlign: 'center',
  },
  uploadButton: {
    backgroundColor: '#007AFF',
    paddingVertical: 14,
    paddingHorizontal: 40,
    borderRadius: 10,
    minWidth: 200,
    alignItems: 'center',
    marginBottom: 28,
  },
  uploadButtonSuccess: {
    backgroundColor: '#34C759',
  },
  uploadButtonError: {
    backgroundColor: '#FF3B30',
  },
  uploadButtonText: {
    color: '#fff',
    fontSize: 17,
    fontWeight: '600',
  },
  buttonDisabled: {
    opacity: 0.5,
  },
  progressContainer: {
    width: '100%',
    maxWidth: 400,
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
  errorText: {
    marginTop: 12,
    color: '#FF3B30',
    fontSize: 14,
    textAlign: 'center',
    maxWidth: 360,
  },
});

export default App;
