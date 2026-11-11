import SwiftUI

/// The engine benchmark on a device (#646): the same workloads on jitless
/// V8 (linked in) and in a WKWebView Worker (today's in-app kernel's
/// engine), with the results as JSON to share.
@main
struct V8BenchApp: App {
    var body: some Scene {
        WindowGroup { ContentView() }
    }
}

@MainActor
@Observable
final class BenchModel {
    var running = false
    var status = "Tap Run. Keep the app in front until it says Done."
    var v8Report: String?
    var workerReport: String?
    var showWorker = false

    var combined: String {
        "[\(v8Report ?? "null"),\n\(workerReport ?? "null")]"
    }

    func run(worker: WorkerBench) async {
        running = true
        defer { running = false }
        status = "Running V8 (jitless)…"
        v8Report = await V8Bench.run()
        status = "Running the Worker page…"
        showWorker = true
        workerReport = await worker.run()
        showWorker = false
        status = "Done. Share the JSON, or run again for another sample."
    }
}

struct ContentView: View {
    @State private var model = BenchModel()
    @State private var worker = WorkerBench()

    var body: some View {
        NavigationStack {
            List {
                Section {
                    Text(model.status)
                    Button("Run") { Task { await model.run(worker: worker) } }
                        .disabled(model.running)
                    if model.v8Report != nil || model.workerReport != nil {
                        ShareLink("Share results", item: model.combined)
                    }
                }
                if model.showWorker {
                    // On screen while it runs, so WebKit doesn't throttle it.
                    Section("Worker page") {
                        WorkerView(bench: worker).frame(height: 120)
                    }
                }
                if let r = model.v8Report {
                    Section("V8, jitless") { Text(r).font(.caption.monospaced()).textSelection(.enabled) }
                }
                if let r = model.workerReport {
                    Section("Worker (WKWebView)") { Text(r).font(.caption.monospaced()).textSelection(.enabled) }
                }
            }
            .navigationTitle("V8 Bench")
        }
    }
}
