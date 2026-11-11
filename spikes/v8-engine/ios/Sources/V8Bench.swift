import Darwin
import Foundation

/// Runs the workloads on the linked V8 (jitless: the device build has no
/// JIT) and adds the app's memory footprint to the report.
enum V8Bench {
    /// bench/ in the app bundle, as stage-ios.sh copied it.
    static var benchDir: URL {
        Bundle.main.resourceURL!.appendingPathComponent("bench")
    }

    static func config() throws -> String {
        let manifest = try JSONSerialization.jsonObject(
            with: Data(contentsOf: benchDir.appendingPathComponent("workloads/manifest.json"))
        ) as! [String: Any]
        let workloads = try (manifest["workloads"] as! [[String: Any]]).map { w in
            [
                "name": w["name"] as! String,
                "sources": try (w["sources"] as! [String]).map {
                    try String(contentsOf: benchDir.appendingPathComponent($0), encoding: .utf8)
                },
            ] as [String: Any]
        }
        let data = try JSONSerialization.data(withJSONObject: [
            "jitless": true, "repeats": 5, "workloads": workloads,
        ])
        return String(decoding: data, as: UTF8.self)
    }

    /// Runs on a thread of its own with an 8 MB stack: V8 sizes its stack
    /// limit from the thread it runs on.
    static func run() async -> String {
        await withCheckedContinuation { done in
            let thread = Thread {
                let before = Footprint.now()
                let report: String
                do {
                    let config = try config()
                    let raw = v8_spike_run(config)!
                    report = String(cString: raw)
                    v8_spike_free(raw)
                } catch {
                    done.resume(returning: "{\"error\": \"\(error)\"}")
                    return
                }
                let after = Footprint.now()
                done.resume(returning: withFootprint(report, before: before, after: after))
            }
            thread.stackSize = 8 << 20
            thread.start()
        }
    }

    /// The report with footprint_before/after/peak_bytes added.
    static func withFootprint(_ report: String, before: Footprint, after: Footprint) -> String {
        guard var json = try? JSONSerialization.jsonObject(with: Data(report.utf8)) as? [String: Any] else {
            return report
        }
        json["footprint_before_bytes"] = before.current
        json["footprint_after_bytes"] = after.current
        json["footprint_peak_bytes"] = after.peak
        json["device"] = deviceModel()
        json["os"] = ProcessInfo.processInfo.operatingSystemVersionString
        guard let data = try? JSONSerialization.data(withJSONObject: json, options: [.prettyPrinted, .sortedKeys]) else {
            return report
        }
        return String(decoding: data, as: UTF8.self)
    }

    static func deviceModel() -> String {
        var info = utsname()
        uname(&info)
        return withUnsafeBytes(of: &info.machine) { String(decoding: $0.prefix { $0 != 0 }, as: UTF8.self) }
    }
}

/// The app's memory footprint, as iOS counts it against its limit.
struct Footprint {
    var current: UInt64
    var peak: UInt64

    static func now() -> Footprint {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<natural_t>.size)
        let rc = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
            }
        }
        guard rc == KERN_SUCCESS else { return Footprint(current: 0, peak: 0) }
        return Footprint(current: info.phys_footprint, peak: info.ledger_phys_footprint_peak)
    }
}
