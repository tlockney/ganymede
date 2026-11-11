import SwiftUI
import UniformTypeIdentifiers
import WebKit

/// Runs bench/web/index.html in a WKWebView: the workloads in Workers,
/// and today's in-app kernel (cdp-kernel's browser kernel) cold-started to
/// its first result. The page posts its report to the "bench" handler.
/// Its memory is the WebContent process's, which the app can't read.
///
/// A failed load, or a page that hasn't posted within `timeout`, ends the
/// run with an error report, so the app never waits forever.
@MainActor
final class WorkerBench: NSObject, WKScriptMessageHandler, WKNavigationDelegate {
    let webView: WKWebView
    private var pending: CheckedContinuation<String, Never>?
    private var timer: Task<Void, Never>?
    static let timeout: Duration = .seconds(180)

    override init() {
        let config = WKWebViewConfiguration()
        config.setURLSchemeHandler(BundleSchemeHandler(), forURLScheme: "bench")
        webView = WKWebView(frame: .zero, configuration: config)
        super.init()
        config.userContentController.add(self, name: "bench")
        webView.navigationDelegate = self
    }

    func run() async -> String {
        await withCheckedContinuation { done in
            pending = done
            timer = Task { [weak self] in
                try? await Task.sleep(for: Self.timeout)
                guard !Task.isCancelled else { return }
                self?.finish(errorReport("the Worker page didn't report within \(Self.timeout)"))
            }
            webView.load(URLRequest(url: URL(string: "bench://app/web/index.html?repeats=5")!))
        }
    }

    private func finish(_ report: String) {
        timer?.cancel()
        timer = nil
        pending?.resume(returning: report)
        pending = nil
    }

    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        finish(message.body as? String ?? errorReport("the Worker page posted no report"))
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: any Error) {
        finish(errorReport("the Worker page failed: \(error.localizedDescription)"))
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: any Error) {
        finish(errorReport("the Worker page didn't load: \(error.localizedDescription)"))
    }
}

struct WorkerView: UIViewRepresentable {
    let bench: WorkerBench
    func makeUIView(context: Context) -> WKWebView { bench.webView }
    func updateUIView(_ view: WKWebView, context: Context) {}
}

/// Serves bench/ from the app bundle at bench://app/, so the page and its
/// Workers share an origin. The apps load kernel.html over HTTP from
/// their embedded server instead (ganymede-ui AppKernels.swift), so the
/// origin differs; see the README's caveats.
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
