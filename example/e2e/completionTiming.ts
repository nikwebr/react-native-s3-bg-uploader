import type { UploadProgress } from '../../src/specs/s3-bg-uploader.types';

export const COMPLETION_TIMEOUT_MS = 20000;

type ObservedProgress = Pick<UploadProgress, 'percentage' | 'state'> &
  Partial<Pick<UploadProgress, 'uploadedBytes'>>;

export class CompletionTiming {
  reached100At?: number;
  completedAfterMs?: number;
  /** Times a running upload reported fewer bytes than one of its earlier events. */
  progressRegressions = 0;
  /** Times an upload that was not `COMPLETED` reported 100%. */
  prematureHundreds = 0;
  private highestUploadedBytes = 0;

  observe(progress: ObservedProgress, now: number): void {
    const uploadedBytes = progress.uploadedBytes ?? 0;
    if (progress.state === 'RUNNING') {
      // Progress may only move forward: a retried part restarts on the wire, but
      // reporting that as lost bytes (and climbing back again) is what users saw as
      // the bar dropping after a connection error. A pause legitimately reports the
      // committed parts only, so it is not part of the monotonic check.
      if (uploadedBytes < this.highestUploadedBytes) this.progressRegressions += 1;
    }
    if (progress.state !== 'COMPLETED') {
      // 100% must mean "committed". In-flight bytes are only handed to the transport;
      // the part can still fail and be retried after they were all counted.
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

  /**
   * Re-baseline the monotonic check. A pause legitimately clears in-flight bytes (the
   * paused snapshot is counted from committed parts only), so the resumed run starts
   * from that value instead of being reported as a regression.
   */
  resetProgressBaseline(uploadedBytes: number): void {
    this.highestUploadedBytes = uploadedBytes;
  }

  /** Throws unless progress stayed honest and monotonic for the whole run. */
  assertClean(): void {
    if (this.progressRegressions > 0) {
      throw new Error(
        `Upload progress went backwards ${this.progressRegressions} time(s) before completion`
      );
    }
    if (this.prematureHundreds > 0) {
      throw new Error(`Upload reported 100% ${this.prematureHundreds} time(s) before completion`);
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
