
// MARK: Render main
//
// Appended to services/macos/sunoto-overlay.swift (cut at "// MARK: App
// launch") by tools/ui/render-overlay-frames.sh, so it sees the file-private
// views. Each group replays what the daemon sends (show, status, recording
// levels, pasted, hide, state) on a synthetic clock, saves the frames its
// script names, and lays them out on a contact sheet.

// The pill's state machine; `pillDriver(of:)` reaches it from the view.
private typealias PillDriver = PillModel
private func pillDriver(of view: PillView) -> PillDriver { view.model }

private let renderOut = URL(fileURLWithPath: CommandLine.arguments[1])
private let renderGroups = Set(CommandLine.arguments.dropFirst(2))
private let renderStart: CFTimeInterval = 1000
private let renderDark = NSAppearance(named: .darkAqua)!
private let renderLight = NSAppearance(named: .aqua)!

/// Quiet for a moment, then phrases with breaths between them.
private func renderSpeech(_ t: Double) -> CGFloat {
    guard t > 1.2 else { return 0.08 }
    let envelope = max(0, sin(t * 2.3) * 0.6 + sin(t * 5.1 + 1) * 0.35 + 0.25)
    return CGFloat(min(1, envelope * (0.75 + 0.25 * sin(t * 37))))
}

/// What the overlay app does with each daemon message, minus the window.
private protocol RenderStage: AnyObject {
    var driver: PillDriver { get }
    var visible: Bool { get set }
    /// Whether the app has the window on screen (it orders it out once a
    /// close finishes).
    var onScreen: Bool { get set }
    var closeAt: CFTimeInterval? { get set }
    func opened(now: CFTimeInterval)
    func ticked(now: CFTimeInterval)
    /// The close finished and the app took the window away.
    func hidden()
    func snapshot(appearance: NSAppearance) -> NSBitmapImageRep
}

extension RenderStage {
    func show(_ now: CFTimeInterval) {
        driver.beginIfIdle(now: now)
        open(now)
    }

    func open(_ now: CFTimeInterval) {
        closeAt = nil
        if !visible {
            visible = true
            onScreen = true
            opened(now: now)
            driver.beginOpening(now: now)
        }
    }

    func status(_ text: String, error: Bool = false, _ now: CFTimeInterval) {
        driver.setStatus(text, error: error, now: now)
    }

    func state(_ name: String, _ detail: String, _ now: CFTimeInterval) {
        driver.setHealth(name: name, detail: detail)
        open(now)
    }

    func pasted(_ now: CFTimeInterval) {
        driver.markPasted(now: now)
        hide(now)
    }

    func hide(_ now: CFTimeInterval) {
        let hold = visible ? driver.closeDelay(now: now) : 0
        if hold > 0 {
            closeAt = now + hold
            return
        }
        visible = false
        driver.beginClosing(now: now)
    }

    func tick(_ now: CFTimeInterval) {
        if let closeAt, now >= closeAt {
            self.closeAt = nil
            hide(now)
        }
        _ = driver.tick(now: now)
        if driver.closeFinished {
            driver.reset()
            onScreen = false
            hidden()
        }
        ticked(now: now)
    }
}

private final class PillStage: RenderStage {
    let view = PillView(frame: NSRect(x: 0, y: 0, width: PillView.captureWidth, height: PillView.rowHeight))
    var driver: PillDriver { pillDriver(of: view) }
    var visible = false
    var onScreen = false
    var closeAt: CFTimeInterval?

    init(reduceMotion: Bool) {
        driver.reduceMotion = reduceMotion
    }

    func opened(now: CFTimeInterval) {}
    func ticked(now: CFTimeInterval) {}
    func hidden() {}

    func snapshot(appearance: NSAppearance) -> NSBitmapImageRep {
        view.appearance = appearance
        view.setFrameSize(view.windowSize)
        let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds)!
        if onScreen {
            view.cacheDisplay(in: view.bounds, to: rep)
        }
        return rep
    }
}

/// One scripted run: messages at times, audio while talking, frames to keep.
private struct RenderScript {
    /// Audio frames every 20 ms over this range; the key is let go at its end.
    var talk: Range<Double>?
    var voice: (Double) -> CGFloat = renderSpeech
    var events: [(Double, (CFTimeInterval) -> Void)] = []
    var frames: [(Double, String)] = []
}

private func play(_ script: RenderScript, on stage: RenderStage, appearance: NSAppearance) -> [(String, NSBitmapImageRep)] {
    var events = script.events.sorted { $0.0 < $1.0 }
    var frames = script.frames.sorted { $0.0 < $1.0 }
    let end = frames.last?.0 ?? 0
    var shots: [(String, NSBitmapImageRep)] = []
    var lastSample = -1.0
    var step = 0
    while true {
        let t = Double(step) * 0.005
        if t > end + 1e-9 { break }
        let now = renderStart + t
        while let event = events.first, event.0 <= t + 1e-9 {
            events.removeFirst()
            event.1(now)
        }
        if let talk = script.talk, talk.contains(t), lastSample < 0 || t - lastSample >= 0.02 - 1e-9 {
            lastSample = t
            stage.driver.pushLevel(script.voice(t - talk.lowerBound), elapsed: t - talk.lowerBound, now: now)
        }
        stage.tick(now)
        while let frame = frames.first, frame.0 <= t + 1e-9 {
            frames.removeFirst()
            shots.append((frame.1, stage.snapshot(appearance: appearance)))
        }
        step += 1
    }
    return shots
}

// MARK: Scripts

/// A dictation that is pasted before anyone needs words.
private func quickDictation(_ stage: RenderStage) -> RenderScript {
    var script = RenderScript(talk: 0..<2.6)
    script.events = [
        (0, { stage.show($0); stage.status("Tap again to paste, Esc to cancel", $0) }),
        (2.65, { stage.show($0); stage.status("transcribing...", $0) }),
        (3.0, { stage.pasted($0) }),
    ]
    script.frames = [
        (0.05, "open-050ms"), (0.12, "open-120ms"), (0.2, "open-200ms"), (0.3, "open-300ms"),
        (1.0, "hint"), (1.4, "hint-fading"), (2.4, "talking"),
        (2.75, "released"), (2.95, "settled"),
        (3.1, "pasted-100ms"), (3.3, "pasted-300ms"), (3.5, "pasted-500ms"), (3.8, "pasted-800ms"),
        (4.3, "closing-100ms"), (4.45, "closing-250ms"),
    ]
    return script
}

/// A long wait: "Transcribing…" shows after 1.5 s, then the paste.
private func slowDictation(_ stage: RenderStage) -> RenderScript {
    var script = RenderScript(talk: 0..<3.0)
    script.events = [
        (0, { stage.show($0); stage.status("Tap again to paste, Esc to cancel", $0) }),
        (3.05, { stage.show($0); stage.status("transcribing...", $0) }),
        (5.2, { stage.show($0); stage.status("polishing...", $0) }),
        (5.6, { stage.pasted($0) }),
    ]
    script.frames = [
        (4.4, "waiting-1.35s"), (4.7, "transcribing"), (5.3, "polishing"),
        (5.7, "pasted-100ms"), (6.1, "pasted-500ms"), (6.9, "closing-100ms"),
    ]
    return script
}

/// Cancelled with Esc while recording: no paste, a plain close.
private func cancelledDictation(_ stage: RenderStage) -> RenderScript {
    var script = RenderScript(talk: 0..<2.0)
    script.events = [
        (0, { stage.show($0); stage.status("Tap again to paste, Esc to cancel", $0) }),
        (2.0, { stage.hide($0) }),
    ]
    script.frames = [(2.1, "closing-100ms"), (2.2, "closing-200ms")]
    return script
}

private func systemDictation(_ stage: RenderStage) -> RenderScript {
    var script = RenderScript(talk: 0..<2.5)
    script.events = [
        (0, { stage.show($0); stage.status("System — tap again to finish", $0) }),
        (2.55, { stage.show($0); stage.status("System — matching...", $0) }),
        (3.4, { stage.show($0); stage.status("Opened Safari", $0) }),
    ]
    script.frames = [(0.5, "system-hint"), (2.3, "system-talking"), (2.9, "system-matching"), (3.5, "system-opened")]
    return script
}

private func health(_ stage: RenderStage) -> RenderScript {
    var script = RenderScript()
    script.events = [
        (0, { stage.state("loading_asr", "loading speech model...", $0) }),
        (1.0, { stage.state("hotkey_blocked", "hotkey blocked: check permissions", $0) }),
        (1.5, { stage.status("No matching target found", error: true, $0) }),
        (2.0, { stage.status("Opened Safari", $0) }),
        (2.5, { stage.state("warming_polish", "warming polish...", $0) }),
    ]
    script.frames = [
        (0.1, "loading-100ms"), (0.6, "loading"), (1.2, "attention"), (1.7, "error"), (2.2, "notice"),
        (2.7, "warming"),
    ]
    return script
}

// MARK: Contact sheets

private func savePNG(_ rep: NSBitmapImageRep, _ url: URL) {
    try! FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    try! rep.representation(using: .png, properties: [:])!.write(to: url)
}

/// Frames in a grid, each labelled with its name, on a plain backdrop.
private func contactSheet(_ shots: [(String, NSBitmapImageRep)], columns: Int, backdrop: NSColor) -> NSBitmapImageRep {
    let label: CGFloat = 34, pad: CGFloat = 16
    let cellW = CGFloat(shots.map(\.1.pixelsWide).max() ?? 1) + pad * 2
    let cellH = CGFloat(shots.map(\.1.pixelsHigh).max() ?? 1) + label + pad
    let cols = min(columns, shots.count), rows = (shots.count + cols - 1) / cols
    let width = Int(cellW) * cols, height = Int(cellH) * rows
    let sheet = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height, bitsPerSample: 8, samplesPerPixel: 4,
        hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
    )!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: sheet)
    backdrop.setFill()
    NSRect(x: 0, y: 0, width: width, height: height).fill()
    let attributes: [NSAttributedString.Key: Any] = [
        .font: NSFont.monospacedSystemFont(ofSize: 20, weight: .medium),
        .foregroundColor: NSColor(white: (backdrop.usingColorSpace(.sRGB)?.brightnessComponent ?? 0) > 0.5 ? 0.1 : 0.92, alpha: 1),
    ]
    for (index, shot) in shots.enumerated() {
        let col = CGFloat(index % cols), row = CGFloat(index / cols)
        let top = CGFloat(height) - row * cellH
        (shot.0 as NSString).draw(at: NSPoint(x: col * cellW + pad, y: top - label + 4), withAttributes: attributes)
        let size = NSSize(width: shot.1.pixelsWide, height: shot.1.pixelsHigh)
        shot.1.draw(in: NSRect(x: col * cellW + pad, y: top - label - size.height, width: size.width, height: size.height))
    }
    NSGraphicsContext.restoreGraphicsState()
    return sheet
}

private func renderGroup(_ name: String, columns: Int, backdrop: NSColor, _ shots: () -> [(String, NSBitmapImageRep)]) {
    guard renderGroups.isEmpty || renderGroups.contains(name) else { return }
    let frames = shots()
    for (frame, rep) in frames {
        savePNG(rep, renderOut.appendingPathComponent(name).appendingPathComponent(frame + ".png"))
    }
    savePNG(contactSheet(frames, columns: columns, backdrop: backdrop), renderOut.appendingPathComponent(name + ".png"))
    print("\(name): \(frames.count) frames")
}

// MARK: Groups

private let pillBackdrop = NSColor(srgbRed: 0.55, green: 0.58, blue: 0.62, alpha: 1)

private func pillRun(_ script: (RenderStage) -> RenderScript, reduceMotion: Bool = false, appearance: NSAppearance = renderDark) -> [(String, NSBitmapImageRep)] {
    let stage = PillStage(reduceMotion: reduceMotion)
    return play(script(stage), on: stage, appearance: appearance)
}

renderGroup("pill-dark", columns: 4, backdrop: pillBackdrop) {
    pillRun(quickDictation) + pillRun(slowDictation).map { ("slow-" + $0.0, $0.1) }
        + pillRun(cancelledDictation).map { ("esc-" + $0.0, $0.1) }
}
renderGroup("pill-light", columns: 4, backdrop: pillBackdrop) {
    pillRun(quickDictation, appearance: renderLight) + pillRun(slowDictation, appearance: renderLight).map { ("slow-" + $0.0, $0.1) }
}
renderGroup("pill-states", columns: 4, backdrop: pillBackdrop) {
    pillRun(health) + pillRun(systemDictation)
        + pillRun(health, appearance: renderLight).map { ("light-" + $0.0, $0.1) }
}
renderGroup("pill-still", columns: 4, backdrop: pillBackdrop) {
    pillRun(quickDictation, reduceMotion: true)
}

// MARK: The island

/// The island over a drawn strip of desktop: wallpaper, the menu bar with
/// menus on the left and status items on the right, and the notch with its
/// camera, so frames show the island where it will be.
private final class IslandStage: RenderStage {
    let view: IslandView
    var driver: PillDriver { view.model }
    var visible = false
    var onScreen = false
    var closeAt: CFTimeInterval?
    private let notch: NSSize
    private let darkWallpaper: Bool

    init(reduceMotion: Bool = false, notch: NSSize = NSSize(width: 160, height: 28), darkWallpaper: Bool = true) {
        let model = PillModel()
        model.reduceMotion = reduceMotion
        model.surface = .island
        view = IslandView(model: model)
        view.notch = notch
        view.setFrameSize(IslandView.windowSize(notch: notch))
        self.notch = notch
        self.darkWallpaper = darkWallpaper
    }

    func opened(now: CFTimeInterval) {}

    func ticked(now: CFTimeInterval) {
        view.update(now: now)
    }

    func hidden() {
        view.reset()
    }

    func snapshot(appearance: NSAppearance) -> NSBitmapImageRep {
        let window = view.bounds.size
        let side: CGFloat = 110
        let size = NSSize(width: window.width + side * 2, height: window.height + 16)
        let rep = NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: Int(size.width * 2), pixelsHigh: Int(size.height * 2),
            bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
            bytesPerRow: 0, bitsPerPixel: 0
        )!
        rep.size = size
        let context = NSGraphicsContext(bitmapImageRep: rep)!
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = context
        let top = size.height, mid = size.width / 2
        let (wallTop, wallBottom) = darkWallpaper
            ? (NSColor(srgbRed: 0.10, green: 0.12, blue: 0.24, alpha: 1), NSColor(srgbRed: 0.30, green: 0.18, blue: 0.36, alpha: 1))
            : (NSColor(srgbRed: 0.62, green: 0.78, blue: 0.93, alpha: 1), NSColor(srgbRed: 0.93, green: 0.90, blue: 0.84, alpha: 1))
        NSGradient(starting: wallBottom, ending: wallTop)!.draw(in: NSRect(origin: .zero, size: size), angle: 90)
        let menuHeight = notch.height + 1
        (darkWallpaper ? NSColor(white: 0, alpha: 0.32) : NSColor(white: 1, alpha: 0.5)).setFill()
        NSRect(x: 0, y: top - menuHeight, width: size.width, height: menuHeight).fill()
        let ink = darkWallpaper ? NSColor.white : NSColor.black
        let menuFont: [NSAttributedString.Key: Any] = [.font: NSFont.systemFont(ofSize: 13, weight: .regular), .foregroundColor: ink]
        let baseline = top - menuHeight / 2 - 8
        ("Edit    View    Window    Help" as NSString).draw(at: NSPoint(x: 24, y: baseline), withAttributes: menuFont)
        let status = "◐    ⌕    Fri 16 Oct  7:49 PM" as NSString
        status.draw(at: NSPoint(x: size.width - 24 - status.size(withAttributes: menuFont).width, y: baseline), withAttributes: menuFont)
        // The hardware notch and its camera.
        NSColor.black.setFill()
        let notchRect = NSRect(x: mid - notch.width / 2, y: top - notch.height, width: notch.width, height: notch.height)
        NSBezierPath(roundedRect: notchRect.insetBy(dx: 0, dy: -10).offsetBy(dx: 0, dy: 10), xRadius: 10, yRadius: 10).fill()
        NSColor(white: 0.12, alpha: 1).setFill()
        NSBezierPath(ovalIn: NSRect(x: mid - 3, y: top - notch.height / 2 - 3, width: 6, height: 6)).fill()

        // The island's window, flush with the top and centred on the notch.
        let cg = context.cgContext
        cg.saveGState()
        cg.translateBy(x: (size.width - window.width) / 2, y: top)
        cg.scaleBy(x: 1, y: -1)
        NSGraphicsContext.current = NSGraphicsContext(cgContext: cg, flipped: true)
        if onScreen {
            view.effectiveAppearance.performAsCurrentDrawingAppearance {
                view.draw(view.bounds)
            }
        }
        cg.restoreGState()
        NSGraphicsContext.restoreGraphicsState()
        return rep
    }
}

private let islandBackdrop = NSColor(white: 0.2, alpha: 1)

private func islandRun(_ script: (RenderStage) -> RenderScript, reduceMotion: Bool = false,
                       notch: NSSize = NSSize(width: 160, height: 28), darkWallpaper: Bool = true) -> [(String, NSBitmapImageRep)] {
    let stage = IslandStage(reduceMotion: reduceMotion, notch: notch, darkWallpaper: darkWallpaper)
    return play(script(stage), on: stage, appearance: renderDark)
}

renderGroup("island-dark", columns: 2, backdrop: islandBackdrop) {
    islandRun(quickDictation) + islandRun(slowDictation).map { ("slow-" + $0.0, $0.1) }
        + islandRun(cancelledDictation).map { ("esc-" + $0.0, $0.1) }
}
renderGroup("island-light", columns: 2, backdrop: islandBackdrop) {
    islandRun(quickDictation, darkWallpaper: false)
}
renderGroup("island-states", columns: 2, backdrop: islandBackdrop) {
    islandRun(health) + islandRun(systemDictation)
}
renderGroup("island-still", columns: 2, backdrop: islandBackdrop) {
    islandRun(quickDictation, reduceMotion: true)
}
/// Other MacBooks and scaled display modes have other notches.
renderGroup("island-notches", columns: 2, backdrop: islandBackdrop) {
    [NSSize(width: 160, height: 28), NSSize(width: 185, height: 32), NSSize(width: 210, height: 38)].flatMap { notch in
        islandRun(quickDictation, notch: notch).filter { ["hint", "talking", "pasted-500ms"].contains($0.0) }
            .map { ("\(Int(notch.width))x\(Int(notch.height))-" + $0.0, $0.1) }
    }
}

// MARK: Notch geometry

/// Not frames: checks that the notch is read right on screens with and
/// without one, wherever the screen sits, and that the island's window lands
/// flush with the top and centred on it. Exits non-zero on a failure.
if renderGroups.isEmpty || renderGroups.contains("geometry") {
    var failures = 0
    func check(_ name: String, _ ok: Bool, _ detail: String = "") {
        print("geometry: \(ok ? "ok  " : "FAIL") \(name) \(detail)")
        failures += ok ? 0 : 1
    }
    // This MacBook (measured 2026-10-16): 1496 × 967 pt, notch x 668–828, 28 pt.
    let mac = NotchGeometry(frame: NSRect(x: 0, y: 0, width: 1496, height: 967), safeTop: 28,
                            left: NSRect(x: 0, y: 939, width: 668, height: 28),
                            right: NSRect(x: 828, y: 939, width: 668, height: 28))
    check("built-in screen has a notch", mac?.rect == NSRect(x: 668, y: 939, width: 160, height: 28), "\(String(describing: mac?.rect))")
    if let mac {
        let frame = IslandView.windowFrame(over: mac)
        check("island window flush with the top", frame.maxY == 967, "\(frame)")
        check("island window centred on the notch", frame.midX == 748, "\(frame)")
        check("island window holds the widest island", frame.width >= PillView.maxWidth + 12, "\(frame)")
    }
    check("screen without a notch", NotchGeometry(frame: NSRect(x: 0, y: 0, width: 1440, height: 900), safeTop: 0, left: nil, right: nil) == nil)
    check("external display beside it", NotchGeometry(frame: NSRect(x: 1496, y: 0, width: 1920, height: 1080), safeTop: 0, left: nil, right: nil) == nil)
    check("safe area but no side areas", NotchGeometry(frame: NSRect(x: 0, y: 0, width: 1496, height: 967), safeTop: 28, left: nil, right: nil) == nil)
    // A notched laptop to the left of and above the main screen; its side
    // areas given in global coordinates, then in its own.
    let away = NSRect(x: -1512, y: 300, width: 1512, height: 982)
    let global = NotchGeometry(frame: away, safeTop: 32,
                               left: NSRect(x: -1512, y: 1250, width: 664, height: 32),
                               right: NSRect(x: -664, y: 1250, width: 664, height: 32))
    let local = NotchGeometry(frame: away, safeTop: 32,
                              left: NSRect(x: 0, y: 950, width: 664, height: 32),
                              right: NSRect(x: 848, y: 950, width: 664, height: 32))
    let expected = NSRect(x: -848, y: 1250, width: 184, height: 32)
    check("notch on a screen away from the origin", global?.rect == expected, "\(String(describing: global?.rect))")
    check("side areas in screen coordinates give the same notch", local?.rect == expected, "\(String(describing: local?.rect))")
    if let global {
        let frame = IslandView.windowFrame(over: global)
        check("its island window flush with its top", frame.maxY == 1282 && frame.midX == -756, "\(frame)")
    }
    check("side areas that overlap", NotchGeometry(frame: NSRect(x: 0, y: 0, width: 1000, height: 600), safeTop: 30,
                                                   left: NSRect(x: 0, y: 570, width: 600, height: 30),
                                                   right: NSRect(x: 400, y: 570, width: 600, height: 30)) == nil)
    if failures > 0 {
        exit(1)
    }
}
