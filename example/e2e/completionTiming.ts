import type { UploadProgress } from '../../src/specs/s3-bg-uploader.types';

export const COMPLETION_TIMEOUT_MS = 20000;

type ObservedProgress = Pick<UploadProgress, 'percentage' | 'state' | 'completedParts' | 'totalParts'> &
  Partial<Pick<UploadProgress, 'uploadedBytes' | 'totalBytes'>>;

/**
 * Records every progress callback of one upload and checks the contract that produced
 * the bug reports:
 *
 * - the bar moves **while a part is on the wire** (`networkProgressReports`), so the
 *   user sees network progress and not just a jump at part boundaries;
 * - `100%` is only reported together with `COMPLETED` (`prematureHundreds`), because
 *   in-flight bytes belong to a part that can still fail after its body was delivered;
 * - acknowledged parts never shrink (`committedRegressions`) and a resumed run never
 *   reports fewer bytes than the paused snapshot (`resumeRegressions`).
 */
export class CompletionTiming {
  /**
   * First callback of the resumed run that reported every byte of the file, i.e. the
   * moment the bar is maxed out for the user who just reconnected. In-flight bytes are
   * included, so this is the last part being on the wire — exactly the point after which
   * the reported bug stalled for another 30 seconds.
   */
  reached100At?: number;
  completedAfterMs?: number;
  /** Progress callbacks where bytes moved without a part being acknowledged. */
  networkProgressReports = 0;
  /** Times an upload that was not `COMPLETED` reported 100%. */
  prematureHundreds = 0;
  /** Times the number of acknowledged parts went backwards. */
  committedRegressions = 0;
  /** Times a resumed run reported fewer bytes than the paused snapshot. */
  resumeRegressions = 0;
  private lastUploadedBytes = 0;
  private lastCompletedParts = 0;
  private pausedBytes?: number;
  private resumedAt?: number;

  observe(progress: ObservedProgress, now: number): void {
    const uploadedBytes = progress.uploadedBytes ?? 0;
    if (progress.state === 'RUNNING') {
      // Bytes that grow while no part was acknowledged are the body of the part that is
      // currently being sent: the network progress in question. A failed attempt drops
      // its own bytes again (they were never stored), which is not a committed change.
      if (
        uploadedBytes > this.lastUploadedBytes &&
        progress.completedParts === this.lastCompletedParts
      ) {
        this.networkProgressReports += 1;
      }
      if (progress.completedParts < this.lastCompletedParts) this.committedRegressions += 1;
      // In-flight bytes sit on top of the committed parts, so a resumed run can never
      // report less than the paused snapshot. A drop here is the "progress was lost on
      // reconnect" symptom from the bug report.
      if (this.pausedBytes !== undefined && uploadedBytes < this.pausedBytes) {
        this.resumeRegressions += 1;
      }
    }
    if (progress.state !== 'COMPLETED' && progress.percentage >= 100) {
      // In-flight bytes are only handed to the transport; the part can still fail and be
      // retried after all of them were counted, so 100% must mean "everything stored".
      this.prematureHundreds += 1;
    }
    this.lastUploadedBytes = uploadedBytes;
    this.lastCompletedParts = progress.completedParts;

    if (this.reached100At === undefined) {
      const totalBytes = progress.totalBytes;
      const everyByteReported = totalBytes !== undefined && totalBytes > 0 &&
        uploadedBytes >= totalBytes;
      // Only a run that started at the resume counts: before the pause the gate holds
      // parts whose bytes were already handed over, so "all bytes reported" there says
      // nothing about the tail the bug report is about.
      const afterResume = this.resumedAt === undefined || now >= this.resumedAt;
      if (afterResume && (everyByteReported || progress.percentage >= 100)) {
        this.reached100At = now;
      }
    }
    if (this.reached100At === undefined || this.completedAfterMs !== undefined) return;
    const elapsed = now - this.reached100At;
    if (elapsed > COMPLETION_TIMEOUT_MS) {
      throw new Error(`Upload did not complete within 20 seconds of 100% (${elapsed.toFixed(0)} ms)`);
    }
    if (progress.state === 'COMPLETED') this.completedAfterMs = elapsed;
  }

  /**
   * Remember the paused snapshot. The paused report counts committed parts only, so the
   * resumed run has to start at or above this value instead of rewinding first.
   */
  recordPausedSnapshot(uploadedBytes: number): void {
    this.pausedBytes = uploadedBytes;
  }

  /** Start of the resumed run whose completion budget is measured. */
  markResumed(now: number): void {
    this.resumedAt = now;
    // A bar that was maxed out before the resume belongs to the previous run (the e2e
    // gate holds parts whose bodies were already delivered), so restart the budget for
    // the run the user is actually watching.
    this.reached100At = undefined;
    this.completedAfterMs = undefined;
  }

  /** Throws unless the run reported real network progress and stayed honest. */
  assertClean(): void {
    if (this.prematureHundreds > 0) {
      throw new Error(`Upload reported 100% ${this.prematureHundreds} time(s) before completion`);
    }
    if (this.committedRegressions > 0) {
      throw new Error(
        `Upload reported fewer acknowledged parts ${this.committedRegressions} time(s)`
      );
    }
    if (this.resumeRegressions > 0) {
      throw new Error(
        `Resumed upload reported fewer bytes than the paused snapshot ${this.resumeRegressions} time(s)`
      );
    }
    if (this.networkProgressReports === 0) {
      throw new Error('Upload never reported progress while a part was being uploaded');
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
