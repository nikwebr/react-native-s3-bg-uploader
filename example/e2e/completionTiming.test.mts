import assert from 'node:assert/strict';
import { test } from 'node:test';
import { CompletionTiming } from './completionTiming.ts';

const committed = (percentage: number, uploadedBytes: number, parts = 4) => ({
  percentage,
  state: 'RUNNING' as const,
  uploadedBytes,
  completedParts: parts,
  totalParts: 4,
});

test('starts at exact 100%, not rounded progress, and accepts the 20-second boundary', () => {
  const timing = new CompletionTiming();
  timing.observe(
    { percentage: 99.999, state: 'RUNNING', uploadedBytes: 999, completedParts: 3, totalParts: 4 },
    0
  );
  assert.equal(timing.reached100At, undefined);
  timing.observe(committed(100, 1000), 1000);
  timing.observe(
    { percentage: 100, state: 'COMPLETED', uploadedBytes: 1000, completedParts: 4, totalParts: 4 },
    21000
  );
  assert.equal(timing.assertComplete(), 20000);
});

test('rejects completion one millisecond beyond the deadline', () => {
  const timing = new CompletionTiming();
  timing.observe(committed(100, 1000), 0);
  assert.throws(
    () =>
      timing.observe(
        { percentage: 100, state: 'COMPLETED', uploadedBytes: 1000, completedParts: 4, totalParts: 4 },
        20001
      ),
    /did not complete within 20 seconds/
  );
  assert.throws(() => timing.assertComplete(), /did not complete within 20 seconds/);
});

test('progress regressions and repeated 100% events do not reset the deadline', () => {
  const timing = new CompletionTiming();
  timing.observe(committed(100, 1000), 0);
  timing.observe(committed(90, 900, 3), 10000);
  timing.observe(committed(100, 1000), 19000);
  assert.throws(() => timing.observe(committed(99, 990, 3), 20001),
    /did not complete within 20 seconds/);
  assert.equal(timing.reached100At, 0);
});

test('requires both 100% and completion, including when they arrive together', () => {
  const timing = new CompletionTiming();
  assert.throws(() => timing.assertComplete(), /never reported 100%/);
  timing.observe(committed(100, 1000), 0);
  assert.throws(() => timing.assertComplete(), /did not complete within 20 seconds/);
  timing.observe(
    { percentage: 100, state: 'COMPLETED', uploadedBytes: 1000, completedParts: 4, totalParts: 4 },
    0
  );
  assert.equal(timing.assertComplete(), 0);
});

test('counts progress that goes backwards while the upload runs', () => {
  const timing = new CompletionTiming();
  timing.observe(committed(40, 400, 2), 0);
  // A reconnect must continue from the committed part, never from zero.
  timing.observe(committed(20, 200, 1), 1000);
  timing.observe(committed(40, 400, 2), 2000);
  assert.equal(timing.progressRegressions, 1);
  assert.throws(() => timing.assertClean(), /went backwards 1 time/);
});

test('a pause and resume continue from the committed bytes without a drop', () => {
  const timing = new CompletionTiming();
  timing.observe(committed(40, 400, 2), 0);
  // Unacknowledged bytes were never counted, so pausing and resuming report the
  // same committed bytes instead of rewinding first.
  timing.observe(
    { percentage: 40, state: 'PAUSED', uploadedBytes: 400, completedParts: 2, totalParts: 4 },
    1000
  );
  timing.observe(committed(40, 400, 2), 2000);
  timing.assertClean();
  assert.equal(timing.progressRegressions, 0);
});

test('counts 100% reported while a part is still unacknowledged', () => {
  const timing = new CompletionTiming();
  // The old in-flight accounting counted the last part body before its ETag arrived.
  timing.observe(committed(100, 1000, 3), 0);
  assert.equal(timing.prematureHundreds, 1);
  assert.throws(() => timing.assertClean(), /while parts were unacknowledged/);
});

test('a clean run reports 100% only once every part is committed', () => {
  const timing = new CompletionTiming();
  timing.observe(committed(50, 500, 2), 0);
  // Three of four parts are stored; the last part is still on the wire and is not
  // counted, so the bar stays below 100 even though its body may be fully sent.
  timing.observe(committed(75, 750, 3), 100);
  timing.observe(committed(100, 1000, 4), 200);
  timing.observe(
    { percentage: 100, state: 'COMPLETED', uploadedBytes: 1000, completedParts: 4, totalParts: 4 },
    300
  );
  timing.assertClean();
  assert.equal(timing.assertComplete(), 100);
});
