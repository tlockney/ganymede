import SwiftUI
import UniformTypeIdentifiers
import WebKit

/// Runs bench/web/index.html in a WKWebView: the workloads in Workers,
/// and today's in-app kernel (cdp-kernel's browser kernel) cold-started to
/// its first result. The page posts its report to the "bench" handler.
/// Its memory is the WebContent process's, which the app can't read.
@MainActor
final class WorkerBench: NSObject, WKScriptMessageHandler {
    let webView: WKWebView
    private var pending: CheckedContinuation<String, Never>?

    override init() {
        let config = WKWebViewConfiguration()
        config.setURLSchemeHandler(BundleSchemeHandler(), forURLScheme: "bench")
        webView = WKWebView(frame: .zero, configuration: config)
        super.init()
        config.userContentController.add(self, name: "bench")
    }

    func run() async -> String {
        await withCheckedContinuation { done in
            pending = done
            webView.load(URLRequest(url: URL(string: "bench://app/web/index.html?repeats=5")!))
        }
    }

    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        let report = message.body as? String ?? "null"
        pending?.resume(returning: report)
        pending = nil
    }
}

struct WorkerView: UIViewRepresentable {
    let bench: WorkerBench
    func makeUIView(context: Context) -> WKWebView { bench.webView }
    func updateUIView(_ view: WKWebView, context: Context) {}
}

/// Serves bench/ from the app bundle at bench://app/, so the page and its
/// Workers share an origin, as the apps serve kernel.html.
final class BundleSchemeHandler: NSObject, WKURLSchemeHandler {
    func webView(_ webView: WKWebView, start task: any WKURLSchemeTask) {
        let path = task.request.url?.path ?? "/"
        let file = V8Bench.benchDir.appendingPathComponent(String(path.dropFirst()))
        guard file.standardized.path.hasPrefix(V8Bench.benchDir.standardized.path),
              let data = try? Data(contentsOf: file)
        else {
            task.didFailWithError(URLError(.fileDoesNotExist))
            return
        }
        let type = UTType(filenameExtension: file.pathExtension)?.preferredMIMEType ?? "application/octet-stream"
        let mime = file.pathExtension == "js" ? "text/javascript" : type
        task.didReceive(URLResponse(url: task.request.url!, mimeType: mime, expectedContentLength: data.count, textEncodingName: "utf-8"))
        task.didReceive(data)
        task.didFinish()
    }

    func webView(_ webView: WKWebView, stop task: any WKURLSchemeTask) {}
}
