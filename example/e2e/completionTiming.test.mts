import assert from 'node:assert/strict';
import { test } from 'node:test';
import { CompletionTiming } from './completionTiming.ts';

test('starts at exact 100%, not rounded progress, and accepts the 20-second boundary', () => {
  const timing = new CompletionTiming();
  timing.observe({ percentage: 99.999, state: 'RUNNING' }, 0);
  assert.equal(timing.reached100At, undefined);
  timing.observe({ percentage: 100, state: 'RUNNING' }, 1000);
  timing.observe({ percentage: 100, state: 'COMPLETED' }, 21000);
  assert.equal(timing.assertComplete(), 20000);
});

test('rejects completion one millisecond beyond the deadline', () => {
  const timing = new CompletionTiming();
  timing.observe({ percentage: 100, state: 'RUNNING' }, 0);
  assert.throws(() => timing.observe({ percentage: 100, state: 'COMPLETED' }, 20001),
    /did not complete within 20 seconds/);
  assert.throws(() => timing.assertComplete(), /did not complete within 20 seconds/);
});

test('progress regressions and repeated 100% events do not reset the deadline', () => {
  const timing = new CompletionTiming();
  timing.observe({ percentage: 100, state: 'RUNNING' }, 0);
  timing.observe({ percentage: 90, state: 'RUNNING' }, 10000);
  timing.observe({ percentage: 100, state: 'RUNNING' }, 19000);
  assert.throws(() => timing.observe({ percentage: 99, state: 'RUNNING' }, 20001),
    /did not complete within 20 seconds/);
  assert.equal(timing.reached100At, 0);
});

test('requires both 100% and completion, including when they arrive together', () => {
  const timing = new CompletionTiming();
  assert.throws(() => timing.assertComplete(), /never reported 100%/);
  timing.observe({ percentage: 100, state: 'RUNNING' }, 0);
  assert.throws(() => timing.assertComplete(), /did not complete within 20 seconds/);
  timing.observe({ percentage: 100, state: 'COMPLETED' }, 0);
  assert.equal(timing.assertComplete(), 0);
  timing.observe({ percentage: 100, state: 'COMPLETED' }, 30000);
  assert.equal(timing.assertComplete(), 0);
});

test('counts progress that goes backwards while the upload runs', () => {
  const timing = new CompletionTiming();
  timing.observe({ percentage: 40, state: 'RUNNING', uploadedBytes: 400 }, 0);
  // A retried part restarts on the wire; the reported bytes must not follow it down.
  timing.observe({ percentage: 20, state: 'RUNNING', uploadedBytes: 200 }, 1000);
  timing.observe({ percentage: 40, state: 'RUNNING', uploadedBytes: 400 }, 2000);
  assert.equal(timing.progressRegressions, 1);
  assert.throws(() => timing.assertClean(), /went backwards 1 time/);
});

test('a pause may re-baseline the monotonic check', () => {
  const timing = new CompletionTiming();
  timing.observe({ percentage: 40, state: 'RUNNING', uploadedBytes: 400 }, 0);
  // Pausing drops in-flight bytes; that is the documented paused snapshot.
  timing.observe({ percentage: 30, state: 'PAUSED', uploadedBytes: 300 }, 1000);
  timing.resetProgressBaseline(300);
  timing.observe({ percentage: 30, state: 'RUNNING', uploadedBytes: 300 }, 2000);
  timing.assertClean();
  assert.equal(timing.progressRegressions, 0);
});

test('counts 100% reported before completion', () => {
  const timing = new CompletionTiming();
  timing.observe({ percentage: 100, state: 'RUNNING', uploadedBytes: 1000 }, 0);
  timing.observe({ percentage: 99.9, state: 'RUNNING', uploadedBytes: 1000 }, 100);
  timing.observe({ percentage: 100, state: 'COMPLETED', uploadedBytes: 1000 }, 200);
  assert.equal(timing.prematureHundreds, 1);
  assert.throws(() => timing.assertClean(), /reported 100% 1 time\(s\) before completion/);
});

test('a clean run reports 100% only when it completes', () => {
  const timing = new CompletionTiming();
  timing.observe({ percentage: 50, state: 'RUNNING', uploadedBytes: 500 }, 0);
  // All in-flight bytes are counted here, but the last part is not acknowledged.
  timing.observe({ percentage: 99.9, state: 'RUNNING', uploadedBytes: 1000 }, 100);
  timing.observe({ percentage: 100, state: 'COMPLETED', uploadedBytes: 1000 }, 200);
  timing.assertClean();
  assert.equal(timing.assertComplete(), 0);
});
