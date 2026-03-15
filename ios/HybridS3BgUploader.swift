//
//  HybridS3BgUploader.swift
//  Pods
//
//  Created by Niklas Weber on 14.3.2026.
//

import Foundation
import RustCore

class HybridS3BgUploader: HybridS3BgUploaderSpec {
    func sum(num1: Double, num2: Double) throws -> Double {
        return Double(add(Int32(num1), Int32(num2)))
    }
}
