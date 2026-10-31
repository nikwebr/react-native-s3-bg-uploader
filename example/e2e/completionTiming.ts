import type { UploadProgress } from '../../src/specs/s3-bg-uploader.types';

export const COMPLETION_TIMEOUT_MS = 20000;

export class CompletionTiming {
  reached100At?: number;
  completedAfterMs?: number;

  observe(progress: Pick<UploadProgress, 'percentage' | 'state'>, now: number): void {
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

  assertComplete(): number {
    if (this.reached100At === undefined) throw new Error('Upload never reported 100%');
    if (this.completedAfterMs === undefined) {
      throw new Error('Upload did not complete within 20 seconds of 100%');
    }
    return this.completedAfterMs;
  }
}
