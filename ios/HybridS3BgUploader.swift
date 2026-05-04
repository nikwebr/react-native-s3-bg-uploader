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

// ---------------------------------------------------------------------------
// C → Swift progress bridge
// Called from the Rust worker thread on every chunk completion.
// ---------------------------------------------------------------------------

private func rustProgressBridge(_ eventPtr: UnsafePointer<ProgressEvent>?) {
    guard let e = eventPtr?.pointee else { return }

    let fileKey    = e.file_key.map    { String(cString: $0) }
    let fileName   = e.file_name.map   { String(cString: $0) } ?? ""
    let fileHash   = e.file_hash.map   { String(cString: $0) } ?? ""
    let transferId = e.transfer_id.map { String(cString: $0) } ?? ""
    let fState     = e.state.map       { String(cString: $0) } ?? "NOT_STARTED"
    let tState     = e.transfer_state.map { String(cString: $0) } ?? "NOT_STARTED"
    let sState     = e.session_state.map  { String(cString: $0) } ?? "NOT_STARTED"

    let fileProgress = UploadProgress(
        fileKey:        fileKey,
        fileName:       fileName,
        fileHash:       fileHash,
        transferId:     transferId,
        totalBytes:     Double(e.total_bytes),
        uploadedBytes:  Double(e.uploaded_bytes),
        completedParts: Double(e.completed_parts),
        totalParts:     Double(e.total_parts),
        percentage:     e.percentage,
        state:          UploadState(fromString: fState) ?? .notStarted
    )

    let transferAggregate = AggregateProgress(
        percentage:         e.transfer_percentage,
        totalSize:          Double(e.transfer_total_size),
        uploadedSize:       Double(e.transfer_uploaded_size),
        totalTransfers:     nil,
        completedTransfers: nil,
        totalFiles:         Double(e.transfer_total_files),
        completedFiles:     Double(e.transfer_completed_files),
        state:              GlobalUploaderState(fromString: tState) ?? .notStarted
    )

    let sessionAggregate = AggregateProgress(
        percentage:         e.session_percentage,
        totalSize:          Double(e.session_total_size),
        uploadedSize:       Double(e.session_uploaded_size),
        totalTransfers:     e.session_total_transfers > 0 ? Double(e.session_total_transfers) : nil,
        completedTransfers: e.session_completed_transfers > 0 ? Double(e.session_completed_transfers) : nil,
        totalFiles:         Double(e.session_total_files),
        completedFiles:     Double(e.session_completed_files),
        state:              GlobalUploaderState(fromString: sState) ?? .notStarted
    )

    DispatchQueue.main.async {
        if #available(iOS 26.0, *), let task = HybridS3BgUploader.sharedBgTask {
            // Format title: use Rust template if set, else fall back to Swift default
            let title: String
            if let ptr = format_title_string() {
                let s = String(cString: ptr); free_string(ptr)
                title = s.isEmpty ? HybridS3BgUploader.sharedTitleTemplate : s
            } else {
                title = HybridS3BgUploader.sharedTitleTemplate
            }
            // Always format subtitle against the live sessionAggregate — the Rust
            // session only knows about completed chunks and would show 0% during
            // in-flight uploads.
            let subtitle = formatTemplate(HybridS3BgUploader.sharedSubtitleTemplate, agg: sessionAggregate)
            task.updateTitle(title, subtitle: subtitle)
            task.progress.totalUnitCount     = Int64(sessionAggregate.totalSize    / 1024)
            task.progress.completedUnitCount = Int64(sessionAggregate.uploadedSize / 1024)

            // Complete the task when all uploads are done or failed
            if sessionAggregate.state == .completed || sessionAggregate.state == .failed {
                task.setTaskCompleted(success: sessionAggregate.state == .completed)
                HybridS3BgUploader.sharedBgTask = nil
            }
        }
        HybridS3BgUploader.sharedProgressCallback?(fileProgress, sessionAggregate, transferAggregate)
    }
}

// ---------------------------------------------------------------------------
// HybridS3BgUploader
// ---------------------------------------------------------------------------

class HybridS3BgUploader: HybridS3BgUploaderSpec {

    fileprivate static var sharedProgressCallback: ((_ fileProgress: UploadProgress, _ sessionAggregate: AggregateProgress, _ transferAggregate: AggregateProgress) -> Void)?
    fileprivate static var sharedTitleTemplate: String = "Uploading"
    fileprivate static var sharedSubtitleTemplate: String = "{percentage}"

    @available(iOS 26.0, *)
    fileprivate static weak var sharedBgTask: BGContinuedProcessingTask?
    @available(iOS 26.0, *)
    private static var bgTaskHandlerRegistered = false

    // Serial queue: max 1 concurrent hash
    private static let hashQueue = DispatchQueue(label: "com.s3bguploader.hash", qos: .userInitiated)
    // Concurrent queue with semaphore: max 2 concurrent start_api calls
    private static let initApiQueue = DispatchQueue(label: "com.s3bguploader.initApi", qos: .userInitiated, attributes: .concurrent)
    private static let initApiSemaphore = DispatchSemaphore(value: 2)
    // Files waiting for resume() to trigger start_api
    private static var pendingHashes: [(hash: String, transferId: String)] = []
    private static let pendingHashesLock = NSLock()

    // Storage path set once on first use
    private static var storagePath: String = {
        let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first!
        return caches.path
    }()

    private static let storagePathSetup: Void = {
        HybridS3BgUploader.storagePath.withCString { set_storage_path($0) }
    }()

    override init() {
        super.init()
        _ = HybridS3BgUploader.storagePathSetup
    }

    // -------------------------------------------------------------------------
    // Config
    // -------------------------------------------------------------------------

    func setConfig(startUploadApi: String, getUploadUrlsApi: String, completeApi: String) throws {
        startUploadApi.withCString { startPtr in
            getUploadUrlsApi.withCString { getUrlsPtr in
                completeApi.withCString { completePtr in
                    set_config(startPtr, getUrlsPtr, completePtr)
                }
            }
        }
    }

    // -------------------------------------------------------------------------
    // Progress callback
    // -------------------------------------------------------------------------

    func setProgressCallback(callback: Variant_NullType____fileProgress__UploadProgress____sessionAggregate__AggregateProgress____transferAggregate__AggregateProgress_____Void?) throws {
        if case .second(let fn) = callback {
            HybridS3BgUploader.sharedProgressCallback = fn
            set_progress_callback(rustProgressBridge)
        } else {
            HybridS3BgUploader.sharedProgressCallback = nil
            set_progress_callback(unsafeBitCast(0, to: ProgressCallback.self))
        }
    }

    // -------------------------------------------------------------------------
    // Notification templates
    // -------------------------------------------------------------------------

    func setTaskTitle(title: String) throws {
        HybridS3BgUploader.sharedTitleTemplate = title
        title.withCString { set_task_title($0) }
    }

    func setTaskSubtitle(subtitle: String) throws {
        HybridS3BgUploader.sharedSubtitleTemplate = subtitle
        subtitle.withCString { set_task_subtitle($0) }
    }

    // -------------------------------------------------------------------------
    // Upload
    // -------------------------------------------------------------------------

    func uploadFile(filePath: String, transferId: String, userParams: [String: String]?) throws -> Promise<String> {
        return Promise.parallel(HybridS3BgUploader.hashQueue) { [self] in
            let paramsJson: String
            if let params = userParams, !params.isEmpty {
                let data = try JSONSerialization.data(withJSONObject: params)
                paramsJson = String(data: data, encoding: .utf8) ?? "{}"
            } else {
                paramsJson = "{}"
            }

            let rawResult: String = filePath.withCString { pathPtr in
                transferId.withCString { tidPtr in
                    paramsJson.withCString { paramsPtr in
                        if let ptr = hash_and_pre_register(pathPtr, tidPtr, paramsPtr) {
                            let h = String(cString: ptr); free_string(ptr); return h
                        }
                        return ""
                    }
                }
            }

            guard !rawResult.isEmpty else {
                throw NSError(domain: "S3BgUploader", code: -1,
                              userInfo: [NSLocalizedDescriptionKey: "hash_and_pre_register failed"])
            }
            guard !rawResult.hasPrefix("ERROR:") else {
                let message = String(rawResult.dropFirst("ERROR:".count))
                throw NSError(domain: "S3BgUploader", code: -1,
                              userInfo: [NSLocalizedDescriptionKey: message])
            }
            let fileHash = rawResult

            // If already running, trigger start_api eagerly; otherwise defer until resume().
            let globalState: String
            if let ptr = get_global_state() {
                globalState = String(cString: ptr); free_string(ptr)
            } else {
                globalState = "NOT_STARTED"
            }
            if globalState == "RUNNING" || globalState == "RUNNING_IN_BG" {
                self.triggerInitApi(hash: fileHash, transferId: transferId)
            } else {
                HybridS3BgUploader.pendingHashesLock.lock()
                HybridS3BgUploader.pendingHashes.append((hash: fileHash, transferId: transferId))
                HybridS3BgUploader.pendingHashesLock.unlock()
            }

            return fileHash
        }
    }

    private func triggerInitApi(hash: String, transferId: String) {
        HybridS3BgUploader.initApiQueue.async {
            HybridS3BgUploader.initApiSemaphore.wait()
            defer { HybridS3BgUploader.initApiSemaphore.signal() }
            hash.withCString { hashPtr in
                transferId.withCString { tidPtr in
                    if let ptr = initialize_file(hashPtr, tidPtr) { free_string(ptr) }
                }
            }
        }
    }

    @available(iOS 26.0, *)
    private func scheduleBgTaskIfNeeded() {
        guard HybridS3BgUploader.sharedBgTask == nil else { return }

        // Register handler once — BGTaskScheduler allows only one registration per identifier
        if !HybridS3BgUploader.bgTaskHandlerRegistered {
            HybridS3BgUploader.bgTaskHandlerRegistered = true
            BGTaskScheduler.shared.register(forTaskWithIdentifier: taskId, using: nil) { task in
                guard let task = task as? BGContinuedProcessingTask else { return }
                HybridS3BgUploader.sharedBgTask = task
                task.expirationHandler = {
                    pause_all()
                    DispatchQueue.main.async {
                        HybridS3BgUploader.sharedBgTask = nil
                        task.setTaskCompleted(success: false)
                    }
                }
            }
        }

        let request = BGContinuedProcessingTaskRequest(
            identifier: taskId,
            title: HybridS3BgUploader.sharedTitleTemplate,
            subtitle: HybridS3BgUploader.sharedSubtitleTemplate
        )
        request.strategy = .queue
        do {
            try BGTaskScheduler.shared.submit(request)
        } catch {
            print("[S3BgUploader] BGTask submit error: \(error)")
        }
    }

    // -------------------------------------------------------------------------
    // Cancel / pause / resume
    // -------------------------------------------------------------------------

    func cancelFile(fileHash: String) throws {
        fileHash.withCString { cancel_file($0) }
    }

    func cancelTransfer(transferId: String) throws {
        transferId.withCString { cancel_transfer($0) }
    }

    func cancel() throws {
        HybridS3BgUploader.pendingHashesLock.lock()
        HybridS3BgUploader.pendingHashes.removeAll()
        HybridS3BgUploader.pendingHashesLock.unlock()
        cancel_all()
        if #available(iOS 26.0, *) {
            DispatchQueue.main.async {
                HybridS3BgUploader.sharedBgTask?.setTaskCompleted(success: true)
                HybridS3BgUploader.sharedBgTask = nil
            }
        }
    }

    func pause() throws {
        pause_all()
        if #available(iOS 26.0, *) {
            DispatchQueue.main.async {
                HybridS3BgUploader.sharedBgTask?.setTaskCompleted(success: false)
                HybridS3BgUploader.sharedBgTask = nil
            }
        }
    }

    func resume() throws -> Promise<Void> {
        if let errPtr = resume_all() {
            let errMsg = String(cString: errPtr); free_string(errPtr)
            throw NSError(domain: "S3BgUploader", code: -1,
                          userInfo: [NSLocalizedDescriptionKey: errMsg])
        }
        // Process deferred NOT_STARTED files
        HybridS3BgUploader.pendingHashesLock.lock()
        let pending = HybridS3BgUploader.pendingHashes
        HybridS3BgUploader.pendingHashes.removeAll()
        HybridS3BgUploader.pendingHashesLock.unlock()
        for p in pending {
            triggerInitApi(hash: p.hash, transferId: p.transferId)
        }
        if #available(iOS 26.0, *) {
            DispatchQueue.main.async {
                self.scheduleBgTaskIfNeeded()
            }
        }
        return Promise.resolved()
    }

    // -------------------------------------------------------------------------
    // Progress queries
    // -------------------------------------------------------------------------

    func getProgress(transferId: String?, fileKey: String?) throws -> [UploadProgress] {
        let ptr: UnsafeMutablePointer<CChar>? = withOptionalCString(transferId) { tidPtr in
            withOptionalCString(fileKey) { fkPtr in
                get_progress_json(tidPtr, fkPtr)
            }
        }
        guard let ptr = ptr else { return [] }
        let json = String(cString: ptr)
        free_string(ptr)
        return decodeJSON([UploadProgressJSON].self, from: json)?.map(\.asUploadProgress) ?? []
    }

    func getAggregateProgress(transferId: String?) throws -> AggregateProgress {
        let ptr: UnsafeMutablePointer<CChar>? = withOptionalCString(transferId) { tidPtr in
            get_aggregate_progress_json(tidPtr)
        }
        guard let ptr = ptr else { return .empty }
        let json = String(cString: ptr)
        free_string(ptr)
        return decodeJSON(AggregateProgressJSON.self, from: json)?.asAggregateProgress ?? .empty
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Calls the closure with a C string pointer, or NULL if the Swift string is nil.
private func withOptionalCString<R>(_ s: String?, body: (UnsafePointer<CChar>?) -> R) -> R {
    guard let s = s else { return body(nil) }
    return s.withCString { body($0) }
}

private func formatTemplate(_ template: String, agg: AggregateProgress) -> String {
    var s = template
    s = s.replacingOccurrences(of: "{percentage}", with: String(format: "%.0f%%", agg.percentage))
    s = s.replacingOccurrences(of: "{totalSize}", with: humanBytes(agg.totalSize))
    s = s.replacingOccurrences(of: "{uploadedSize}", with: humanBytes(agg.uploadedSize))
    s = s.replacingOccurrences(of: "{totalTransfers}",     with: "\(Int(agg.totalTransfers ?? 0))")
    s = s.replacingOccurrences(of: "{completedTransfers}", with: "\(Int(agg.completedTransfers ?? 0))")
    s = s.replacingOccurrences(of: "{totalFiles}",     with: "\(Int(agg.totalFiles))")
    s = s.replacingOccurrences(of: "{completedFiles}", with: "\(Int(agg.completedFiles))")
    return s
}

private func humanBytes(_ bytes: Double) -> String {
    let kb = bytes / 1024
    if kb < 1024 { return String(format: "%.0f KB", kb) }
    let mb = kb / 1024
    if mb < 1024 { return String(format: "%.1f MB", mb) }
    return String(format: "%.2f GB", mb / 1024)
}

private func decodeJSON<T: Decodable>(_ type: T.Type, from string: String) -> T? {
    guard let data = string.data(using: .utf8) else { return nil }
    return try? JSONDecoder().decode(type, from: data)
}

// ---------------------------------------------------------------------------
// JSON bridge types (match Rust serde output)
// ---------------------------------------------------------------------------

private struct UploadProgressJSON: Decodable {
    let fileKey: String?
    let fileName: String
    let fileHash: String
    let transferId: String
    let totalBytes: UInt64
    let uploadedBytes: UInt64
    let completedParts: UInt32
    let totalParts: UInt32
    let percentage: Double
    let state: String

    var asUploadProgress: UploadProgress {
        UploadProgress(
            fileKey:        fileKey,
            fileName:       fileName,
            fileHash:       fileHash,
            transferId:     transferId,
            totalBytes:     Double(totalBytes),
            uploadedBytes:  Double(uploadedBytes),
            completedParts: Double(completedParts),
            totalParts:     Double(totalParts),
            percentage:     percentage,
            state:          UploadState(fromString: state) ?? .notStarted
        )
    }
}

private struct AggregateProgressJSON: Decodable {
    let percentage: Double
    let totalSize: UInt64
    let uploadedSize: UInt64
    let totalTransfers: UInt32?
    let completedTransfers: UInt32?
    let totalFiles: UInt32
    let completedFiles: UInt32
    let state: String

    var asAggregateProgress: AggregateProgress {
        AggregateProgress(
            percentage:         percentage,
            totalSize:          Double(totalSize),
            uploadedSize:       Double(uploadedSize),
            totalTransfers:     totalTransfers.map(Double.init),
            completedTransfers: completedTransfers.map(Double.init),
            totalFiles:         Double(totalFiles),
            completedFiles:     Double(completedFiles),
            state:              GlobalUploaderState(fromString: state) ?? .notStarted
        )
    }
}

private extension AggregateProgress {
    static var empty: AggregateProgress {
        AggregateProgress(
            percentage: 0,
            totalSize: 0,
            uploadedSize: 0,
            totalTransfers: nil,
            completedTransfers: nil,
            totalFiles: 0,
            completedFiles: 0,
            state: .notStarted
        )
    }
}
