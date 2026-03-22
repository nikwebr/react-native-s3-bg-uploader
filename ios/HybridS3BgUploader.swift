//
//  HybridS3BgUploader.swift
//  Pods
//
//  Created by Niklas Weber on 14.3.2026.
//

import Foundation
import NitroModules
import RustCore

private func rustProgressBridge(
    totalBytes: UInt64,
    uploadedBytes: UInt64,
    completedParts: UInt32,
    totalParts: UInt32,
    percentage: Double
) {
    guard let cb = HybridS3BgUploader.sharedProgressCallback else { return }
    let progress = UploadProgress(
        totalBytes: Double(totalBytes),
        uploadedBytes: Double(uploadedBytes),
        completedParts: Double(completedParts),
        totalParts: Double(totalParts),
        percentage: percentage
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

    func uploadFile(filePath: String) throws -> Promise<Void> {
        return Promise.async {
            if HybridS3BgUploader.sharedProgressCallback != nil {
                set_progress_callback(rustProgressBridge)
            }

            let success = filePath.withCString { cPath in
                upload_file(cPath) == 0
            }

            set_progress_callback(nil)

            if !success {
                throw RuntimeError.error(withMessage: "Upload failed")
            }
        }
    }
}
