import assert from 'node:assert/strict';
import { test } from 'node:test';
import { CompletionTiming } from './completionTiming.ts';

const TOTAL = 1000;

/** A callback with everything the real one carries. */
const see = (
  percentage: number,
  uploadedBytes: number,
  completedParts = 3,
  state: 'RUNNING' | 'PAUSED' | 'COMPLETED' = 'RUNNING'
) => ({ percentage, state, uploadedBytes, totalBytes: TOTAL, completedParts, totalParts: 4 });

/** A run that reports network progress, then completes. */
const cleanRun = (timing: CompletionTiming) => {
  timing.observe(see(50, 500, 2), 0);
  // In-flight bytes of the next part: the bar moves without a part being acknowledged.
  timing.observe(see(60, 600, 2), 500);
  timing.observe(see(70, 700, 2), 900);
  timing.observe(see(80, 800, 3), 1000);
  timing.observe(see(99.9, 1000, 3), 1500);
  timing.observe(see(100, 1000, 4, 'COMPLETED'), 2000);
};

test('a maxed-out bar before the resume does not start the completion budget', () => {
  const timing = new CompletionTiming();
  // The e2e gate holds parts whose bodies were already delivered: the bar is maxed out
  // long before the pause, and that moment must not count against the resumed run.
  timing.observe(see(50, 500, 2), 0);
  timing.observe(see(60, 600, 2), 100);
  timing.observe(see(99.9, 1000, 2), 200);
  assert.equal(timing.reached100At, 200);

  const resumed = new CompletionTiming();
  resumed.observe(see(99.9, 1000, 2), 200);
  resumed.markResumed(30000);
  resumed.observe(see(50, 500, 2), 30100);
  resumed.observe(see(60, 600, 2), 31000);
  resumed.observe(see(99.9, 1000, 3), 40000);
  resumed.observe(see(100, 1000, 4, 'COMPLETED'), 41000);
  assert.equal(resumed.reached100At, 40000);
  assert.equal(resumed.assertComplete(), 1000);
  resumed.assertClean();
});

test('accepts the 20-second boundary measured from the first full-bytes callback', () => {
  const timing = new CompletionTiming();
  cleanRun(timing);
  assert.equal(timing.reached100At, 1500);
  assert.equal(timing.assertComplete(), 500);
});

test('rejects completion one millisecond beyond the deadline', () => {
  const late = new CompletionTiming();
  late.observe(see(50, 500, 2), 0);
  late.observe(see(99.9, 1000, 3), 0);
  assert.throws(
    () => late.observe(see(100, 1000, 4, 'COMPLETED'), 20001),
    /did not complete within 20 seconds/
  );
});

test('a failed attempt that drops its bytes does not reset the deadline', () => {
  const timing = new CompletionTiming();
  timing.observe(see(50, 500, 2), 0);
  timing.observe(see(99.9, 1000, 3), 1000);
  // Part 4 failed after its body arrived; those bytes were never stored.
  timing.observe(see(50, 500, 2), 20000);
  timing.observe(see(99.9, 1000, 3), 21000);
  assert.equal(timing.reached100At, 1000);
  assert.throws(() => timing.assertComplete(), /did not complete within 20 seconds/);
});

test('requires both 100% and completion, including when they arrive together', () => {
  const timing = new CompletionTiming();
  assert.throws(() => timing.assertComplete(), /never reported 100%/);
  timing.observe(see(99.9, 1000, 3), 0);
  assert.throws(() => timing.assertComplete(), /did not complete within 20 seconds/);
  timing.observe(see(100, 1000, 4, 'COMPLETED'), 0);
  assert.equal(timing.assertComplete(), 0);
  timing.observe(see(100, 1000, 4, 'COMPLETED'), 30000);
  assert.equal(timing.assertComplete(), 0);
});

test('counts progress that moves while a part is on the wire', () => {
  const timing = new CompletionTiming();
  cleanRun(timing);
  // Three events grew the byte count without a new acknowledged part.
  assert.equal(timing.networkProgressReports, 3);
  timing.assertClean();
});

test('rejects a run that only jumps from part to part', () => {
  const timing = new CompletionTiming();
  // No in-flight reports at all: this is the accounting that hid the network progress.
  timing.observe(see(75, 500, 3), 0);
  timing.observe(see(100, 1000, 4, 'COMPLETED'), 100);
  assert.throws(() => timing.assertClean(), /never reported progress while a part was being uploaded/);
});

test('rejects 100% reported before completion', () => {
  const timing = new CompletionTiming();
  timing.observe(see(50, 500, 2), 0);
  timing.observe(see(60, 600, 2), 100);
  timing.observe(see(100, 1000, 3), 200);
  timing.observe(see(100, 1000, 4, 'COMPLETED'), 300);
  assert.equal(timing.prematureHundreds, 1);
  assert.throws(() => timing.assertClean(), /reported 100% 1 time\(s\) before completion/);
});

test('rejects an upload that forgets acknowledged parts', () => {
  const timing = new CompletionTiming();
  timing.observe(see(50, 500, 3), 0);
  timing.observe(see(60, 600, 2), 100);
  assert.equal(timing.committedRegressions, 1);
  assert.throws(() => timing.assertClean(), /fewer acknowledged parts 1 time\(s\)/);
});

test('rejects a resumed run that drops below the paused snapshot', () => {
  const timing = new CompletionTiming();
  timing.observe(see(50, 500, 2), 0);
  timing.observe(see(60, 600, 2), 100);
  timing.observe(see(50, 500, 2, 'PAUSED'), 200);
  timing.recordPausedSnapshot(500);
  // The resumed run must continue from the acknowledged 500 bytes, never from zero.
  timing.observe(see(25, 250, 2), 300);
  assert.equal(timing.resumeRegressions, 1);
  assert.throws(() => timing.assertClean(), /fewer bytes than the paused snapshot 1 time\(s\)/);
});

test('a pause and resume that continue from the acknowledged bytes stay clean', () => {
  const timing = new CompletionTiming();
  timing.observe(see(50, 500, 2), 0);
  timing.observe(see(70, 700, 2), 100);
  // Pausing reports the acknowledged parts only: nothing was stored for the in-flight part.
  timing.observe(see(50, 500, 2, 'PAUSED'), 200);
  timing.recordPausedSnapshot(500);
  timing.observe(see(50, 500, 2), 300);
  timing.observe(see(60, 600, 2), 400);
  timing.observe(see(99.9, 1000, 3), 500);
  timing.observe(see(100, 1000, 4, 'COMPLETED'), 600);
  timing.assertClean();
  assert.equal(timing.resumeRegressions, 0);
  assert.equal(timing.assertComplete(), 100);
});
