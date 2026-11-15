import assert from 'node:assert/strict';
import { test } from 'node:test';
import { assertAggregateAcknowledged } from './networkProgress.ts';

test('accepts acknowledged parts while their progress callback is still queued', async () => {
  const callback = { completedParts: 6, totalParts: 7 };
  let current = callback;
  await assertAggregateAcknowledged(async () => {
    current = { completedParts: 7, totalParts: 7 };
    return { percentage: 100 };
  }, async () => current);
  assert.equal(callback.completedParts, 6);
});

test('rejects an aggregate at 100% while the current file still has unconfirmed parts', async () => {
  await assert.rejects(assertAggregateAcknowledged(
    async () => ({ percentage: 100 }),
    async () => ({ completedParts: 6, totalParts: 7 }),
  ), /Aggregate reached 100% before acknowledgement/);
});

test('accepts completion eviction and only queries file counts at 100%', async () => {
  await assertAggregateAcknowledged(async () => ({ percentage: 100 }), async () => undefined);
  await assertAggregateAcknowledged(async () => ({ percentage: 99 }), async () => {
    throw new Error('No file query needed below 100%');
  });
});
