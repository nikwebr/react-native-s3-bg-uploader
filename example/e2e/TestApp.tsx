import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Button, Platform, ScrollView, StyleSheet, Text, TextInput } from 'react-native';
import { S3BgUploader } from 'react-native-s3-bg-uploader';
import type { UploadProgress } from '../../src/specs/s3-bg-uploader.types';
import { CompletionTiming, COMPLETION_TIMEOUT_MS } from './completionTiming';
import { assertAggregateAcknowledged } from './networkProgress';

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
  const current = useRef<{ hash: string; mode: string } | undefined>(undefined);
  const completionTiming = useRef(new CompletionTiming());
  const completionTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const latestProgress = useRef<UploadProgress | undefined>(undefined);
  const recoveryError = useRef('');

  const onProgress = useCallback((file: UploadProgress) => {
    if (file.fileHash !== current.current?.hash || file.transferId !== current.current?.mode) return;
    setProgress({ ...file });
    if (current.current.mode.endsWith('-network')) {
      if (file.percentage >= 100 && file.completedParts < file.totalParts) {
        recoveryError.current = '100% reported before S3 acknowledged every part';
      }
      if (recoveryError.current) setError(recoveryError.current);
    }
    latestProgress.current = { ...file };
    if (file.state === 'FAILED' && current.current.mode !== 'encrypted-network') {
      setError(`Upload failed: ${file.fileName}`);
    }
    const timing = completionTiming.current;
    try {
      timing.observe(file, performance.now());
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
    S3BgUploader.setConfig(
      `${BACKEND}/start`,
      `${BACKEND}/urls`,
      `${BACKEND}/complete`,
      `${BACKEND}/cancel`,
      true,
    );
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

  const prepare = async (mode: 'plain' | 'encrypted' | 'cancel', network = false) => {
    clearTimeout(completionTimer.current);
    completionTimer.current = undefined;
    completionTiming.current = new CompletionTiming();
    current.current = undefined;
    latestProgress.current = undefined;
    recoveryError.current = '';
    setCompletionAfterMs(undefined);
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
      file = new File([await response.arrayBuffer()], 'fixture.bin');
    } else if (!fixturePath) {
      throw new Error('Enter the fixture path provided by the E2E runner');
    }
    const transfer = network ? `${mode}-network` : mode;
    const hash = mode === 'encrypted'
      ? await S3BgUploader.encryptAndUploadFile(file, transfer, KEY, NONCE, { mode: transfer })
      : await S3BgUploader.uploadFile(file, transfer, { mode: transfer });
    current.current = { hash, mode: transfer };
    setStatus(`${mode} queued`);
    await S3BgUploader.resume();
    // Native uploads can emit progress before uploadFile resolves with their key.
    for (const fileProgress of await S3BgUploader.getProgress(transfer, hash)) {
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

  // Native parts already sent when pause arrives are allowed to finish. Wait until
  // progress has been unchanged for a full window, so a slow runner cannot report
  // those permitted completions as progress that happened "while paused".
  // Web cannot suspend a request: its pause cancels the in-flight parts, so nothing
  // may land at all (checked below).
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
    const beforePause = await snapshot();
    S3BgUploader.pause();
    const deadline = Date.now() + 10000;
    while ((await S3BgUploader.getAggregateProgress()).state !== 'PAUSED') {
      if (Date.now() > deadline) throw new Error('Uploader did not enter PAUSED');
      await sleep(100);
    }
    const initial = await snapshot();
    if (Platform.OS !== 'web' && initial.uploadedBytes < beforePause.uploadedBytes) {
      throw new Error('Native pause discarded progress of a held request');
    }
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
    if (Platform.OS === 'web' && before.completedParts !== initial.completedParts) {
      // Browsers cannot suspend a request, so a pause must cancel the parts still
      // in transfer instead of letting them land once the server gate opens.
      throw new Error('Web pause left an in-flight part running');
    }
    if (after.completedParts !== before.completedParts ||
        after.uploadedBytes !== before.uploadedBytes) {
      throw new Error('Upload advanced while paused');
    }
    if ((await S3BgUploader.getAggregateProgress()).state !== 'PAUSED') {
      throw new Error('Uploader left PAUSED while paused');
    }
    await api('/paused', {
      mode: current.current?.mode,
      platform: Platform.OS,
      before,
      after,
    });
    setStatus('Pause verified');
  };

  const resume = async () => {
    const before = await snapshot();
    await api('/resumed', { mode: current.current?.mode });
    await S3BgUploader.resume();
    if (Platform.OS !== 'web' && current.current) {
      const after = (await S3BgUploader.getProgress(current.current.mode, current.current.hash))[0];
      if (after && after.uploadedBytes < before.uploadedBytes) {
        throw new Error('Native resume discarded progress of a viable held request');
      }
    }
    setStatus('Resume requested');
  };

  const networkRecovery = async (mode: 'plain' | 'encrypted') => {
    await prepare(mode, true);
    const waitForState = async (state: string, timeout: number) => {
      const deadline = performance.now() + timeout;
      while (latestProgress.current?.state !== state) {
        if (recoveryError.current) throw new Error(recoveryError.current);
        if (performance.now() > deadline) throw new Error(`Network recovery did not reach ${state}`);
        await assertAggregateAcknowledged(
          async () => S3BgUploader.getAggregateProgress(),
          async () => (await S3BgUploader.getProgress(current.current!.mode, current.current!.hash))[0],
        );
        await sleep(100);
      }
    };
    if (mode === 'encrypted') {
      await waitForState('FAILED', 90000);
      const before = await snapshot();
      const after = await snapshot();
      if (before.uploadedBytes !== after.uploadedBytes) {
        throw new Error('Failed progress did not settle at confirmed parts');
      }
      await api('/recover', { mode: current.current?.mode, before });
      await S3BgUploader.resume();
      await waitForState('COMPLETED', 20000);
    } else {
      await waitForState('COMPLETED', 90000);
    }
    if (recoveryError.current) throw new Error(recoveryError.current);
    await api('/completion-timing', {
      mode: current.current?.mode,
      completedAfterMs: completionTiming.current.assertComplete(),
    });
    await api('/verify', { mode: current.current?.mode });
    setStatus(`${mode} network recovery verified`);
  };

  const cancelUpload = async () => {
    await prepare('cancel');
    // Part one completes without the pause gate; parts two and beyond are in flight.
    // `resume()` can return before startUpload answered, and a pending file is not
    // reported by getProgress(fileHash): treat that transient absence as "not started"
    // instead of a disappeared upload.
    const deadline = Date.now() + 60000;
    let started: UploadProgress | undefined;
    while (Date.now() < deadline) {
      try {
        started = await snapshot();
        if (started.completedParts >= 1) break;
      } catch {
        // hashing or startUpload still in flight
      }
      await sleep(100);
    }
    if (!started || started.completedParts < 1) {
      throw new Error('Upload did not start before the cancel');
    }
    S3BgUploader.cancel();
    // cancel() is synchronous and the cancelUpload request is best effort, so wait for
    // the backend to confirm that S3 dropped the multipart upload and its parts.
    const abortDeadline = Date.now() + 30000;
    let lastError: unknown;
    for (;;) {
      try {
        await api('/cancelled', { mode: current.current?.mode });
        break;
      } catch (e: unknown) {
        lastError = e;
        if (Date.now() > abortDeadline) throw lastError;
        await sleep(200);
      }
    }
    const remaining = await S3BgUploader.getProgress();
    if (remaining.length !== 0) throw new Error('Cancelled files are still in the session');
    setStatus('Cancellation verified');
  };

  const verify = async () => {
    // The uploader evicts finished sessions; retain the real completion event.
    if (progress?.state !== 'COMPLETED' || progress.fileHash !== current.current?.hash) {
      throw new Error('Upload is not complete');
    }
    await api('/completion-timing', {
      mode: current.current?.mode,
      completedAfterMs: completionTiming.current.assertComplete(),
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
      <Button title="Cancel upload" disabled={busy} onPress={() => run(cancelUpload)} />
      <Button title="Test network retry" disabled={busy} onPress={() => run(() => networkRecovery('plain'))} />
      <Button title="Test network resume" disabled={busy} onPress={() => run(() => networkRecovery('encrypted'))} />
      <Text>{status}</Text>
      <Text>{progress?.state === 'COMPLETED'
        ? 'Upload completed'
        : `Completed parts: ${progress?.completedParts ?? 0}`}</Text>
      {completionAfterMs !== undefined && (
        <Text>Completion within 20 seconds verified</Text>
      )}
      {busy && <Text>Working</Text>}
      {!!error && <Text accessibilityRole="alert">E2E error: {error}</Text>}
    </ScrollView>
  );
}

const styles = StyleSheet.create({
  // Compact spacing: the extra cancel step keeps every button and the status text
  // reachable on the emulator without scrolling.
  screen: { padding: 24, paddingTop: 48, gap: 8 },
  title: { fontSize: 24 },
  input: { borderWidth: 1, padding: 8 },
});
