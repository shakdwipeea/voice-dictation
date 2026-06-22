import AppKit
import Foundation

private final class PillView: NSView {
    var level: CGFloat = 0.0 {
        didSet { needsDisplay = true }
    }
    var status: String = "" {
        didSet { needsDisplay = true }
    }

    override var isFlipped: Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        let bounds = self.bounds
        let pillWidth: CGFloat = 214
        let pillHeight: CGFloat = 34
        let pillRect = NSRect(
            x: max(0, (bounds.width - pillWidth) / 2.0),
            y: 0,
            width: min(pillWidth, bounds.width),
            height: pillHeight
        )

        NSColor(calibratedWhite: 0.05, alpha: 0.86).setFill()
        NSBezierPath(roundedRect: pillRect, xRadius: pillRect.height / 2.0, yRadius: pillRect.height / 2.0).fill()

        let dotRect = NSRect(x: pillRect.minX + 14, y: pillRect.minY + (pillRect.height - 10) / 2.0, width: 10, height: 10)
        NSColor(calibratedRed: 0.94, green: 0.26, blue: 0.26, alpha: 1.0).setFill()
        NSBezierPath(ovalIn: dotRect).fill()

        let meterX = pillRect.minX + 36
        let meterWidth: CGFloat = 150
        let meterRect = NSRect(x: meterX, y: pillRect.minY + (pillRect.height - 5) / 2.0, width: meterWidth, height: 5)
        NSColor(calibratedWhite: 1.0, alpha: 0.12).setFill()
        NSBezierPath(roundedRect: meterRect, xRadius: 2.5, yRadius: 2.5).fill()

        let fillWidth = max(2, min(meterWidth, meterWidth * level))
        NSColor(calibratedWhite: 0.88, alpha: 1.0).setFill()
        NSBezierPath(
            roundedRect: NSRect(x: meterX, y: meterRect.minY, width: fillWidth, height: meterRect.height),
            xRadius: 2.5,
            yRadius: 2.5
        ).fill()

        if !status.isEmpty {
            let paragraph = NSMutableParagraphStyle()
            paragraph.alignment = .center
            paragraph.lineBreakMode = .byTruncatingTail
            let attrs: [NSAttributedString.Key: Any] = [
                .foregroundColor: NSColor(calibratedWhite: 0.92, alpha: 1.0),
                .font: NSFont.systemFont(ofSize: 12.5, weight: .medium),
                .paragraphStyle: paragraph,
            ]
            let textSize = (status as NSString).size(withAttributes: attrs)
            let captionWidth = min(bounds.width, max(112, ceil(textSize.width) + 32))
            let statusRect = NSRect(
                x: (bounds.width - captionWidth) / 2.0,
                y: pillRect.maxY + 7,
                width: captionWidth,
                height: 27
            )
            let path = NSBezierPath(roundedRect: statusRect, xRadius: 13.5, yRadius: 13.5)
            NSColor(calibratedWhite: 0.04, alpha: 0.72).setFill()
            path.fill()
            NSColor(calibratedWhite: 1.0, alpha: 0.10).setStroke()
            path.lineWidth = 1.0
            path.stroke()

            let textRect = statusRect.insetBy(dx: 14, dy: 5.5)
            (status as NSString).draw(in: textRect, withAttributes: attrs)
        }
    }
}

private final class OverlayApp: NSObject, NSApplicationDelegate {
    private let panel: NSPanel
    private let pill = PillView(frame: NSRect(x: 0, y: 0, width: 214, height: 34))
    private var visible = false

    override init() {
        panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 214, height: 34),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        super.init()
        panel.contentView = pill
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.ignoresMouseEvents = true
        panel.isReleasedWhenClosed = false
        panel.level = .statusBar
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
        panel.hidesOnDeactivate = false
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        hide()
        announceReady()
        DispatchQueue.global(qos: .utility).async { self.pumpStdin() }
    }

    private func announceReady() {
        let line = "{\"type\":\"ready\",\"backend\":\"overlay\"}\n"
        FileHandle.standardOutput.write(Data(line.utf8))
    }

    private func pumpStdin() {
        while let line = readLine() {
            guard let data = line.data(using: .utf8),
                  let object = try? JSONSerialization.jsonObject(with: data),
                  let message = object as? [String: Any] else {
                continue
            }
            DispatchQueue.main.async { self.handle(message) }
        }
        DispatchQueue.main.async { NSApp.terminate(nil) }
    }

    private func handle(_ message: [String: Any]) {
        switch message["type"] as? String {
        case "show":
            show()
        case "hide":
            hide()
        case "recording":
            let peak = number(message["peak"])
            let rms = number(message["rms"])
            pill.level = min(1.0, max(0.0, CGFloat(peak * 8.0), CGFloat(rms * 35.0)))
        case "status":
            pill.status = (message["text"] as? String) ?? ""
            resizeForStatus()
        case "segment", "clear":
            break
        case "shutdown":
            NSApp.terminate(nil)
        default:
            break
        }
    }

    private func number(_ value: Any?) -> Double {
        if let value = value as? Double { return value }
        if let value = value as? Int { return Double(value) }
        if let value = value as? String { return Double(value) ?? 0.0 }
        return 0.0
    }

    private func resizeForStatus() {
        let width: CGFloat = pill.status.isEmpty ? 214 : 420
        let height: CGFloat = pill.status.isEmpty ? 34 : 68
        var frame = panel.frame
        frame.size = NSSize(width: width, height: height)
        panel.setFrame(frame, display: true)
        pill.frame = NSRect(x: 0, y: 0, width: width, height: height)
        place()
    }

    private func show() {
        visible = true
        place()
        panel.setIsVisible(true)
        panel.orderFrontRegardless()
    }

    private func hide() {
        visible = false
        panel.orderOut(nil)
    }

    private func place() {
        guard visible, let screen = NSScreen.main else { return }
        let frame = screen.visibleFrame
        let x = frame.midX - panel.frame.width / 2.0
        let y = frame.maxY - panel.frame.height - 16
        panel.setFrameOrigin(NSPoint(x: x, y: y))
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
private let delegate = OverlayApp()
app.delegate = delegate
app.run()
