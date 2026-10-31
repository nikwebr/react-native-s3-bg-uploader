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
