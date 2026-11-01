import type { UploadProgress } from '../../src/specs/s3-bg-uploader.types';

export const COMPLETION_TIMEOUT_MS = 20000;

type ObservedProgress = Pick<
  UploadProgress,
  'percentage' | 'state' | 'completedParts' | 'totalParts'
> &
  Partial<Pick<UploadProgress, 'uploadedBytes'>>;

export class CompletionTiming {
  reached100At?: number;
  completedAfterMs?: number;
  /** Times a running upload reported fewer bytes than one of its earlier events. */
  progressRegressions = 0;
  /** Times an upload reported 100% while parts were still unacknowledged. */
  prematureHundreds = 0;
  private highestUploadedBytes = 0;

  observe(progress: ObservedProgress, now: number): void {
    const uploadedBytes = progress.uploadedBytes ?? 0;
    if (progress.state === 'RUNNING') {
      // Only acknowledged bytes are progress, so a reconnect resumes from the last
      // committed part. Reporting fewer bytes than before therefore means the bar
      // rewound — exactly the bug this run guards against. The check spans the pause
      // and resume so a drop on reconnection cannot hide behind a new baseline.
      if (uploadedBytes < this.highestUploadedBytes) this.progressRegressions += 1;
    }
    if (progress.state !== 'COMPLETED' && progress.completedParts < progress.totalParts) {
      // 100% must mean every part is stored. A part whose body reached the server but
      // whose response was lost has no ETag yet and can still fail and be retried, so
      // counting its bytes would claim completion too early.
      if (progress.percentage >= 100) this.prematureHundreds += 1;
    }
    this.highestUploadedBytes = Math.max(this.highestUploadedBytes, uploadedBytes);

    if (this.reached100At === undefined && progress.percentage >= 100) {
      this.reached100At = now;
    }
    if (this.reached100At === undefined || this.completedAfterMs !== undefined) return;
    const elapsed = now - this.reached100At;
    if (elapsed > COMPLETION_TIMEOUT_MS) {
      throw new Error(`Upload did not complete within 20 seconds of 100% (${elapsed.toFixed(0)} ms)`);
    }
    if (progress.state === 'COMPLETED') this.completedAfterMs = elapsed;
  }

  /** Throws unless progress stayed honest and monotonic for the whole run. */
  assertClean(): void {
    if (this.progressRegressions > 0) {
      throw new Error(
        `Upload progress went backwards ${this.progressRegressions} time(s) before completion`
      );
    }
    if (this.prematureHundreds > 0) {
      throw new Error(
        `Upload reported 100% ${this.prematureHundreds} time(s) while parts were unacknowledged`
      );
    }
  }

  assertComplete(): number {
    if (this.reached100At === undefined) throw new Error('Upload never reported 100%');
    if (this.completedAfterMs === undefined) {
      throw new Error('Upload did not complete within 20 seconds of 100%');
    }
    return this.completedAfterMs;
  }
}
