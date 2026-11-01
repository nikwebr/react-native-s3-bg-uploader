import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Button, Platform, ScrollView, StyleSheet, Text, TextInput } from 'react-native';
import { S3BgUploader } from 'react-native-s3-bg-uploader';
import type { UploadProgress } from '../../src/specs/s3-bg-uploader.types';
import { CompletionTiming, COMPLETION_TIMEOUT_MS } from './completionTiming';

// Public, deterministic test material only. Never used by the normal example.
const KEY = '0123456789abcdef'.repeat(4);
const NONCE = '0123456789abcdef'.repeat(3);
const BACKEND = 'http://127.0.0.1:8765';
const sleep = (ms: number) => new Promise<void>(resolve => setTimeout(resolve, ms));

async function api(path: string, body: unknown): Promise<void> {
  const response = await fetch(`${BACKEND}${path}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    throw new Error(`${path}: ${response.status} ${await response.text()}`);
  }
}

export default function TestApp({ initializeEncryptor }: {
  initializeEncryptor?: () => Promise<void>;
}): React.JSX.Element {
  const [fixturePath, setFixturePath] = useState('');
  const [status, setStatus] = useState('Ready');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<UploadProgress>();
  const [completionAfterMs, setCompletionAfterMs] = useState<number>();
  const [networkRetryVerified, setNetworkRetryVerified] = useState(false);
  const [networkFaultArmed, setNetworkFaultArmed] = useState(false);
  const current = useRef<{ hash: string; mode: string } | undefined>(undefined);
  const completionTiming = useRef(new CompletionTiming());
  const completionTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  const onProgress = useCallback((file: UploadProgress) => {
    if (file.fileHash !== current.current?.hash || file.transferId !== current.current?.mode) return;
    setProgress({ ...file });
    if (file.state === 'FAILED') setError(`Upload failed: ${file.fileName}`);
    const timing = completionTiming.current;
    try {
      timing.observe(file, performance.now());
      if (file.state === 'COMPLETED') {
        // The server drops one part PUT after receiving its body, so completion can
        // only be reached by retrying a part whose bytes were already counted.
        timing.assertClean();
        setNetworkRetryVerified(true);
      }
      if (timing.completedAfterMs !== undefined) {
        clearTimeout(completionTimer.current);
        setCompletionAfterMs(timing.assertComplete());
      } else if (timing.reached100At !== undefined && completionTimer.current === undefined) {
        completionTimer.current = setTimeout(() => {
          try {
            timing.assertComplete();
          } catch (e: unknown) {
            setError(e instanceof Error ? e.message : String(e));
          }
        }, Math.max(0, COMPLETION_TIMEOUT_MS - (performance.now() - timing.reached100At)) + 1);
      }
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    S3BgUploader.setConfig(`${BACKEND}/start`, `${BACKEND}/urls`, `${BACKEND}/complete`);
    S3BgUploader.setProgressCallback(onProgress);
    return () => {
      clearTimeout(completionTimer.current);
      S3BgUploader.setProgressCallback(null);
    };
  }, [onProgress]);

  const run = async (operation: () => Promise<void>) => {
    setBusy(true);
    setError('');
    try {
      await operation();
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const prepare = async (mode: 'plain' | 'encrypted') => {
    clearTimeout(completionTimer.current);
    completionTimer.current = undefined;
    completionTiming.current = new CompletionTiming();
    current.current = undefined;
    setCompletionAfterMs(undefined);
    setNetworkRetryVerified(false);
    setNetworkFaultArmed(false);
    await initializeEncryptor?.();
    S3BgUploader.cancel();
    // A query also fences the worker's asynchronous cancel() on web.
    await S3BgUploader.getProgress();
    setProgress(undefined);
    setStatus('Preparing fixture');
    let file: File | string = fixturePath;
    if (Platform.OS === 'web') {
      const response = await fetch(`${BACKEND}/fixture`);
      if (!response.ok) throw new Error(`Fixture download: HTTP ${response.status}`);
      file = new File([await response.blob()], 'fixture.bin');
    } else if (!fixturePath) {
      throw new Error('Enter the fixture path provided by the E2E runner');
    }
    const hash = mode === 'encrypted'
      ? await S3BgUploader.encryptAndUploadFile(file, mode, KEY, NONCE, { mode })
      : await S3BgUploader.uploadFile(file, mode, { mode });
    current.current = { hash, mode };
    setStatus(`${mode} queued`);
    await S3BgUploader.resume();
    // Native uploads can emit progress before uploadFile resolves with their key.
    for (const fileProgress of await S3BgUploader.getProgress(mode, hash)) {
      onProgress(fileProgress);
    }
    setStatus('Upload started');
  };

  const snapshot = async (): Promise<UploadProgress> => {
    if (!current.current) throw new Error('No active test upload');
    const files = await S3BgUploader.getProgress(current.current.mode, current.current.hash);
    const file = files[0];
    if (!file) throw new Error('Upload disappeared from the session');
    return file;
  };

  // Parts already sent when pause arrives are allowed to finish. Wait until progress
  // has been unchanged for a full window, so a slow runner cannot report those
  // permitted completions as progress that happened "while paused".
  // A no-op pause keeps dispatching, so the quiet window can then never be reached
  // before the deadline: the check still fails if pause does not actually stop work.
  const SETTLE_QUIET_MS = 5000;
  const settle = async (): Promise<UploadProgress> => {
    const deadline = Date.now() + 45000;
    let stable = await snapshot();
    let stableSince = Date.now();
    while (Date.now() - stableSince < SETTLE_QUIET_MS) {
      if (Date.now() > deadline) throw new Error('Upload did not settle while paused');
      await sleep(200);
      const next = await snapshot().catch(() => {
        // A finished upload is evicted from the session: the pause was a no-op.
        throw new Error('All parts uploaded while paused');
      });
      if (next.completedParts !== stable.completedParts ||
          next.uploadedBytes !== stable.uploadedBytes) {
        if (next.completedParts >= next.totalParts) {
          throw new Error('All parts uploaded while paused');
        }
        stable = next;
        stableSince = Date.now();
      }
    }
    return stable;
  };

  const pause = async () => {
    S3BgUploader.pause();
    const deadline = Date.now() + 10000;
    while ((await S3BgUploader.getAggregateProgress()).state !== 'PAUSED') {
      if (Date.now() > deadline) throw new Error('Uploader did not enter PAUSED');
      await sleep(100);
    }
    const initial = await snapshot();
    if (initial.completedParts < 1 || initial.completedParts >= initial.totalParts) {
      throw new Error('Pause must occur after a completed part and before completion');
    }
    // Open the server gate while still paused: a no-op pause cannot pass this check.
    await api('/release', { mode: current.current?.mode });
    // Already-sent parts may finish after a pause. Wait for progress to stop moving
    // instead of sleeping a fixed time: a fixed window made the check depend on how
    // fast the runner completed those in-flight parts.
    // A straggler that was already in flight can still land right after a quiet
    // window. Re-settle instead of failing: the contract allows those completions,
    // and only an upload that keeps moving (pause did not stop dispatch) can never
    // reach a second quiet window.
    let before = await settle();
    let after = await snapshot();
    for (let attempt = 0; attempt < 3 && before.completedParts < before.totalParts &&
         (after.completedParts !== before.completedParts ||
          after.uploadedBytes !== before.uploadedBytes); attempt++) {
      before = await settle();
      after = await snapshot();
    }
    if (before.completedParts >= before.totalParts) {
      throw new Error('All parts uploaded while paused');
    }
    if (after.completedParts !== before.completedParts ||
        after.uploadedBytes !== before.uploadedBytes) {
      throw new Error('Upload advanced while paused');
    }
    if ((await S3BgUploader.getAggregateProgress()).state !== 'PAUSED') {
      throw new Error('Uploader left PAUSED while paused');
    }
    await api('/paused', { mode: current.current?.mode, before, after });
    // The paused snapshot counts committed parts only, so the resumed run starts there.
    completionTiming.current.resetProgressBaseline(after.uploadedBytes);
    setStatus('Pause verified');
  };

  const resume = async () => {
    const mode = current.current?.mode;
    const { totalParts, completedParts } = await snapshot();
    if (!(totalParts > 1) || completedParts >= totalParts) {
      throw new Error('Cannot arm a network error without a pending part');
    }
    // Drop the last pending part's PUT once, after its body was delivered: the
    // uploader counted those bytes but never got an ETag, so it has to retry.
    await api('/fault', { mode, part: totalParts });
    setNetworkFaultArmed(true);
    await api('/resumed', { mode });
    await S3BgUploader.resume();
    setStatus('Resume requested');
  };

  const verify = async () => {
    // The uploader evicts finished sessions; retain the real completion event.
    if (progress?.state !== 'COMPLETED' || progress.fileHash !== current.current?.hash) {
      throw new Error('Upload is not complete');
    }
    const timing = completionTiming.current;
    timing.assertClean();
    await api('/completion-timing', {
      mode: current.current?.mode,
      completedAfterMs: timing.assertComplete(),
      progressRegressions: timing.progressRegressions,
      prematureHundreds: timing.prematureHundreds,
    });
    await api('/verify', { mode: current.current?.mode });
    setStatus(current.current?.mode === 'encrypted'
      ? 'Encrypted download verified; correct key decrypts; wrong key rejected'
      : 'Unencrypted download verified; bytes match fixture');
  };

  return (
    <ScrollView
      contentContainerStyle={styles.screen}
      keyboardShouldPersistTaps="handled"
      keyboardDismissMode="on-drag"
    >
      <Text style={styles.title}>Uploader E2E</Text>
      {Platform.OS !== 'web' && (
        <TextInput
          accessibilityLabel="Fixture path"
          placeholder="Fixture path"
          value={fixturePath}
          onChangeText={setFixturePath}
          autoCapitalize="none"
          autoCorrect={false}
          style={styles.input}
        />
      )}
      <Button title="Upload unencrypted fixture" disabled={busy} onPress={() => run(() => prepare('plain'))} />
      <Button title="Upload encrypted fixture" disabled={busy} onPress={() => run(() => prepare('encrypted'))} />
      <Button title="Pause upload" disabled={busy} onPress={() => run(pause)} />
      <Button title="Resume upload" disabled={busy} onPress={() => run(resume)} />
      <Button title="Verify download" disabled={busy} onPress={() => run(verify)} />
      <Text>{status}</Text>
      <Text>{progress?.state === 'COMPLETED'
        ? 'Upload completed'
        : `Completed parts: ${progress?.completedParts ?? 0}`}</Text>
      {completionAfterMs !== undefined && (
        <Text>Completion within 20 seconds verified</Text>
      )}
      {networkFaultArmed && networkRetryVerified && (
        <Text>Network error retried without losing progress</Text>
      )}
      {busy && <Text>Working</Text>}
      {!!error && <Text accessibilityRole="alert">E2E error: {error}</Text>}
    </ScrollView>
  );
}

const styles = StyleSheet.create({
  screen: { padding: 24, paddingTop: 64, gap: 16 },
  title: { fontSize: 24 },
  input: { borderWidth: 1, padding: 8 },
});
