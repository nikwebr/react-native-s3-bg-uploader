import type { AggregateProgress, UploadProgress } from '../../src/specs/s3-bg-uploader.types';

export async function assertAggregateAcknowledged(
  getAggregate: () => Promise<Pick<AggregateProgress, 'percentage'>>,
  getFile: () => Promise<Pick<UploadProgress, 'completedParts' | 'totalParts'> | undefined>,
): Promise<void> {
  const aggregate = await getAggregate();
  if (aggregate.percentage < 100) return;
  // Read after the aggregate: callbacks can still be queued with older counts.
  // An evicted file has completed; its completion callback is checked separately.
  const file = await getFile();
  if (file && file.completedParts < file.totalParts) {
    throw new Error('Aggregate reached 100% before acknowledgement');
  }
}
