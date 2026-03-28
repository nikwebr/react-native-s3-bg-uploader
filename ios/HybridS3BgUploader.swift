//
//  HybridS3BgUploader.swift
//  Pods
//
//  Created by Niklas Weber on 14.3.2026.
//

import Foundation
import NitroModules
import RustCore
import BackgroundTasks

private let taskId = "\(Bundle.main.bundleIdentifier!).background"

private func rustProgressBridge(
    totalBytes: UInt64,
    uploadedBytes: UInt64,
    completedParts: UInt32,
    totalParts: UInt32,
    percentage: Double,
    state: UnsafePointer<CChar>?
) {
    guard let cb = HybridS3BgUploader.sharedProgressCallback else { return }
    let stateStr = state.map { String(cString: $0) } ?? ""
    let progress = UploadProgress(
        totalBytes: Double(totalBytes),
        uploadedBytes: Double(uploadedBytes),
        completedParts: Double(completedParts),
        totalParts: Double(totalParts),
        percentage: percentage,
        state: UploadState(fromString: stateStr) ?? .failed
    )
    DispatchQueue.main.async { cb(progress) }
}

class HybridS3BgUploader: HybridS3BgUploaderSpec {

    fileprivate static var sharedProgressCallback: ((_ progress: UploadProgress) -> Void)?

    func setProgressCallback(callback: Variant_NullType____progress__UploadProgress_____Void?) throws -> Void {
        if case .second(let fn) = callback {
            HybridS3BgUploader.sharedProgressCallback = fn
        } else {
            HybridS3BgUploader.sharedProgressCallback = nil
        }
    }

    func sum(num1: Double, num2: Double) throws -> Double {
        return Double(add(Int32(num1), Int32(num2)))
    }

    func uploadFile(filePath: String) throws -> Void {
        registerBgTask(filePath)
        runBgTask()
    }

    private func registerBgTask(_ filePath: String) {
        BGTaskScheduler.shared.register(forTaskWithIdentifier: taskId, using: nil) { task in
            guard let task = task as? BGContinuedProcessingTask else { return }

            task.expirationHandler = {}

            task.progress.totalUnitCount = 100

            DispatchQueue.global(qos: .userInitiated).async {
                if HybridS3BgUploader.sharedProgressCallback != nil {
                    set_progress_callback(rustProgressBridge)
                }
                let success = filePath.withCString { cPath in
                    upload_file(cPath) == 0
                }
                set_progress_callback(nil)

                DispatchQueue.main.async {
                    task.setTaskCompleted(success: success)
                }
            }
        }
    }

    /// Runs the background continued processing task.
    private func runBgTask() {
        let request = BGContinuedProcessingTaskRequest(
            identifier: taskId,
            title: "Upload",
            subtitle: "Running..."
        )
        request.strategy = .queue
        do {
            try BGTaskScheduler.shared.submit(request)
        } catch {
            print(error)
        }
    }
}
