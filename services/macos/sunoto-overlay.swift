import AppKit
import Foundation

// The look of the overlay. Your voice is red while you talk; when you let go
// the sound settles into an ink-blue line, the line text sits on, and a glint
// travels along it while Sunoto works. Amber marks anything that needs the
// user. System mode uses the person's macOS accent so it can never be
// confused with dictation capture.
private struct PillPalette {
    let body: NSColor
    let edge: NSColor
    let primary: NSColor
    let secondary: NSColor
    let faint: NSColor
    let voice: NSColor
    let ink: NSColor
    let amber: NSColor
    let onAmber: NSColor

    static func resolve(for appearance: NSAppearance) -> PillPalette {
        if appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua {
            return PillPalette(
                body: NSColor(srgbRed: 0.105, green: 0.115, blue: 0.145, alpha: 0.95),
                edge: NSColor(white: 1.0, alpha: 0.11),
                primary: NSColor(srgbRed: 0.95, green: 0.96, blue: 0.98, alpha: 1),
                secondary: NSColor(white: 1.0, alpha: 0.58),
                faint: NSColor(white: 1.0, alpha: 0.16),
                voice: NSColor(srgbRed: 1.0, green: 0.38, blue: 0.32, alpha: 1),
                ink: NSColor(srgbRed: 0.56, green: 0.69, blue: 1.0, alpha: 1),
                amber: NSColor(srgbRed: 1.0, green: 0.72, blue: 0.28, alpha: 1),
                onAmber: NSColor(srgbRed: 0.16, green: 0.10, blue: 0.0, alpha: 0.9)
            )
        }
        return PillPalette(
            body: NSColor(srgbRed: 0.985, green: 0.987, blue: 0.992, alpha: 0.96),
            edge: NSColor(srgbRed: 0.05, green: 0.08, blue: 0.15, alpha: 0.13),
            primary: NSColor(srgbRed: 0.09, green: 0.11, blue: 0.15, alpha: 1),
            secondary: NSColor(srgbRed: 0.09, green: 0.11, blue: 0.15, alpha: 0.56),
            faint: NSColor(srgbRed: 0.09, green: 0.11, blue: 0.15, alpha: 0.13),
            voice: NSColor(srgbRed: 0.88, green: 0.22, blue: 0.17, alpha: 1),
            ink: NSColor(srgbRed: 0.18, green: 0.37, blue: 0.84, alpha: 1),
            amber: NSColor(srgbRed: 0.85, green: 0.52, blue: 0.0, alpha: 1),
            onAmber: NSColor.white
        )
    }

    /// The island is the notch's own black in light and dark mode, with the
    /// dark palette's colours on it.
    static let island: PillPalette = {
        let dark = resolve(for: NSAppearance(named: .darkAqua)!)
        return PillPalette(
            body: .black, edge: .clear, primary: dark.primary, secondary: dark.secondary, faint: dark.faint,
            voice: dark.voice, ink: dark.ink, amber: dark.amber, onAmber: dark.onAmber
        )
    }()
}

/// Easing curves for the pill's open and close motion.
private enum Ease {
    static func clamp(_ t: CGFloat) -> CGFloat { max(0, min(1, t)) }
    static func outCubic(_ t: CGFloat) -> CGFloat { 1 - pow(1 - clamp(t), 3) }
    static func inCubic(_ t: CGFloat) -> CGFloat { pow(clamp(t), 3) }
    static func inOutCubic(_ t: CGFloat) -> CGFloat {
        let t = clamp(t)
        return t < 0.5 ? 4 * t * t * t : 1 - pow(-2 * t + 2, 3) / 2
    }
    /// Overshoots by a few percent, then settles: a gentle spring.
    static func outBack(_ t: CGFloat) -> CGFloat {
        let t = clamp(t), c1: CGFloat = 1.2, c3 = c1 + 1
        return 1 + c3 * pow(t - 1, 3) + c1 * pow(t - 1, 2)
    }
}

/// How the pill enters and leaves. The record dot appears first and the
/// pill unrolls out of it to the right; closing rolls it back into the dot,
/// which then shrinks away. After a paste the pill instead folds in around
/// its middle, so the check mark lands in the centre. With Reduce Motion
/// all of it is a plain fade.
private struct PillMotion {
    /// Scale and opacity of the round cap that holds the dot.
    var seedScale: CGFloat = 1
    var seedAlpha: CGFloat = 1
    /// 0 is just the round cap, 1 the full pill (briefly above 1 on open).
    var unroll: CGFloat = 1
    var contentAlpha: CGFloat = 1
    /// Vertical offset; negative is up.
    var lift: CGFloat = 0
    /// Where the round cap sits as the pill rolls up: 0 at the left end,
    /// 0.5 in the middle.
    var anchor: CGFloat = 0
}

private enum PillPhase {
    /// Nothing to show.
    case idle
    /// Holding the shortcut: live sound ribbon and timer.
    case recording
    /// Let go: the ribbon has settled into a line while ASR and polish run.
    case working
    /// Daemon health that resolves by itself (loading, warming, mic starting).
    case busy
    /// Daemon health the user has to fix (hotkey blocked, mic unavailable).
    case attention
    /// A session failed or its text went somewhere other than the cursor.
    case error
    /// A short confirmation, such as a System mode action that ran.
    case notice
}

/// Where the pill is drawn this session: floating under the menu bar, or as
/// an island grown out of the notch.
private enum PillSurface {
    case floating
    case island
}

/// What the pill shows and when: the phase, the sound, the caption, and the
/// timing of the open and close motion. The floating pill and the notch
/// island both draw from it; how each moves and lays itself out is theirs.
private final class PillModel {
    static let historyLength = 160
    static let sampleInterval: CFTimeInterval = 0.02
    /// Audio frames arrive every 20 ms while the key is held; a longer gap
    /// means the user let go.
    private static let releaseGap: CFTimeInterval = 0.14
    private static let settleDuration: CFTimeInterval = 0.36
    static let openDuration: CFTimeInterval = 0.42
    private static let closeDuration: CFTimeInterval = 0.3
    /// The recording hint fades once you start talking, or after this long,
    /// and the pill narrows to the ribbon and timer.
    private static let hintDuration: CFTimeInterval = 3
    static let hintFade: CFTimeInterval = 0.2
    /// Talking means this many 20 ms frames in a row above `voiceLevel`, so
    /// a click or a cough does not count.
    private static let voiceLevel: CGFloat = 0.35
    private static let voiceFrames = 4
    /// After the second tap the pill shows no words unless the wait is
    /// longer than this.
    private static let slowWorkDelay: CFTimeInterval = 1.5
    /// After a paste the floating pill folds in around its middle, carrying
    /// the dot there. Near the end of the fold the dot grows into a disc and
    /// a check is drawn on it; the pill closes once the check has been seen.
    static let foldDuration: CFTimeInterval = 0.42
    static let checkDelay: CFTimeInterval = 0.3
    static let checkGrow: CFTimeInterval = 0.34
    static let checkStroke: CFTimeInterval = 0.3
    static let checkStrokeDelay: CFTimeInterval = 0.42
    private static let confirmDuration: CFTimeInterval = 1.2
    /// The folded check leaves a little slower than the pill rolls up.
    private static let foldedCloseDuration: CFTimeInterval = 0.38
    /// The island pulls back into the notch, then fades the last sliver.
    private static let islandCloseDuration: CFTimeInterval = 0.5
    /// With Reduce Motion the check simply appears in the dot.
    private static let stillCheckHold: CFTimeInterval = 0.45

    private(set) var phase: PillPhase = .idle
    private(set) var caption = ""
    private(set) var systemMode = false
    private(set) var levels = [CGFloat](repeating: 0, count: PillModel.historyLength)
    private(set) var elapsed: Double = 0
    private(set) var sawSample = false
    private(set) var lastSampleAt: CFTimeInterval = 0
    private(set) var settle: CGFloat = 0
    private var settleStartedAt: CFTimeInterval = 0
    private(set) var clock: CFTimeInterval = 0
    /// When the recording hint starts to fade.
    private(set) var hintFadeAt: CFTimeInterval = .infinity
    private var voicedFrames = 0
    private var workingSince: CFTimeInterval = 0
    /// "Transcribing…" or "Polishing…", held back until the wait is long.
    private var pendingCaption = ""
    /// When the text was pasted: the pill shows a check mark.
    private(set) var pastedAt: CFTimeInterval?
    var reduceMotion = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
    /// The app picks this as the pill opens from hidden, and keeps it until
    /// the pill is gone.
    var surface: PillSurface = .floating
    enum Transition {
        case opening(since: CFTimeInterval)
        case closing(since: CFTimeInterval)
    }
    private(set) var transition: Transition?
    /// Set when a close finishes; the app then hides the window.
    private(set) var closeFinished = false

    /// True while the pill changes on its own and needs the display clock.
    var isAnimating: Bool {
        if transition != nil {
            return true
        }
        switch phase {
        case .recording: return true
        case .working:
            return settle < 1 || !reduceMotion || pastedAt != nil
                || (caption.isEmpty && !pendingCaption.isEmpty)
        case .busy: return !reduceMotion
        default: return false
        }
    }

    // MARK: Input from the daemon

    /// A new capture is about to start (or the pill was just shown). A pill
    /// still confirming the last paste starts over too.
    func beginIfIdle(now: CFTimeInterval) {
        guard phase == .idle || pastedAt != nil else { return }
        startCapture(now: now)
    }

    func pushLevel(_ level: CGFloat, elapsed newElapsed: Double, now: CFTimeInterval) {
        let newSession = newElapsed + 0.05 < elapsed
        if newSession || !(phase == .recording || phase == .working) {
            startCapture(now: now)
        } else if phase == .working {
            // Audio resumed after a hiccup: the key is still held.
            phase = .recording
            settle = 0
        }
        levels.removeFirst()
        levels.append(max(0, min(1, level)))
        voicedFrames = level > Self.voiceLevel ? voicedFrames + 1 : 0
        if phase == .recording, voicedFrames >= Self.voiceFrames {
            // You are talking: the hint has done its job.
            hintFadeAt = min(hintFadeAt, now)
        }
        elapsed = newElapsed
        sawSample = true
        lastSampleAt = now
        clock = now
    }

    func setStatus(_ raw: String, error: Bool, now: CFTimeInterval) {
        var text = raw
        for prefix in ["System — ", "System: "] where text.hasPrefix(prefix) {
            systemMode = true
            text = String(text.dropFirst(prefix.count))
        }
        text = Self.tidy(text)
        clock = now
        if error {
            phase = .error
            caption = Self.sentenceCase(text)
        } else if raw.isEmpty {
            // Sent as a new recording starts: clear the caption.
            caption = ""
            systemMode = false
        } else if text.hasSuffix("…") {
            // Progress after the second tap. Transcribing and polishing are
            // usually over in under a second, so their words wait; a System
            // step ("matching…") shows at once.
            enterWorking(now: now)
            if ["polishing…", "transcribing…"].contains(text.lowercased()) {
                pendingCaption = Self.sentenceCase(text)
                if !caption.isEmpty {
                    caption = pendingCaption
                }
            } else {
                caption = Self.sentenceCase(text)
            }
        } else if phase == .recording {
            // The hint shown as a recording starts.
            caption = Self.sentenceCase(text)
            hintFadeAt = now + Self.hintDuration
        } else {
            phase = .notice
            caption = Self.sentenceCase(text)
        }
    }

    /// The text landed at the cursor: show the check mark.
    func markPasted(now: CFTimeInterval) {
        guard phase == .recording || phase == .working else { return }
        enterWorking(now: now)
        pendingCaption = ""
        if !caption.isEmpty {
            // A slow wait was showing "Transcribing…".
            caption = "Pasted"
        }
        pastedAt = now
        clock = now
    }

    /// How long a close should wait so the check mark is seen.
    func closeDelay(now: CFTimeInterval) -> CFTimeInterval {
        guard let pastedAt, phase == .working else { return 0 }
        let hold = reduceMotion ? Self.stillCheckHold : Self.confirmDuration
        return max(0, pastedAt + hold - now)
    }

    /// The floating pill is folded in (or folding) around a check mark.
    var folded: Bool { surface == .floating && pastedAt != nil && phase == .working && !reduceMotion }

    var closeDuration: CFTimeInterval {
        if surface == .island {
            return Self.islandCloseDuration
        }
        return folded ? Self.foldedCloseDuration : Self.closeDuration
    }

    func setHealth(name: String, detail: String) {
        let selfResolving = ["loading_asr", "warming_polish", "mic_starting"]
        phase = selfResolving.contains(name) ? .busy : .attention
        caption = Self.sentenceCase(Self.tidy(detail.isEmpty ? name : detail))
        systemMode = false
    }

    // MARK: Open and close

    /// How far the pill is shown, 0 to 1, assuming linear time. Used to turn
    /// an interrupted close into an open (and back) without a jump.
    private func presence(at now: CFTimeInterval) -> CGFloat {
        switch transition {
        case .opening(let since):
            return Ease.clamp(CGFloat((now - since) / Self.openDuration))
        case .closing(let since):
            return 1 - Ease.clamp(CGFloat((now - since) / closeDuration))
        case nil:
            return closeFinished ? 0 : 1
        }
    }

    func beginOpening(now: CFTimeInterval) {
        let shown = transition == nil && !closeFinished ? 0 : presence(at: now)
        transition = .opening(since: now - CFTimeInterval(shown) * Self.openDuration)
        closeFinished = false
        clock = now
    }

    func beginClosing(now: CFTimeInterval) {
        let shown = presence(at: now)
        transition = .closing(since: now - CFTimeInterval(1 - shown) * closeDuration)
        clock = now
    }

    /// Stop any open or close motion and show the pill as it is.
    func settleMotion() {
        transition = nil
        closeFinished = false
    }

    /// Advance animations to `now`. Returns whether to keep the clock running.
    func tick(now: CFTimeInterval) -> Bool {
        clock = now
        switch transition {
        case .opening(let since) where now - since >= Self.openDuration:
            transition = nil
        case .closing(let since) where now - since >= closeDuration:
            transition = nil
            closeFinished = true
        default:
            break
        }
        if phase == .recording, sawSample, now - lastSampleAt > Self.releaseGap {
            enterWorking(now: now)
        }
        if phase == .recording, !caption.isEmpty, now >= hintFadeAt + Self.hintFade {
            caption = ""
        }
        if phase == .working, settle < 1 {
            let t = CGFloat(min(1, (now - settleStartedAt) / Self.settleDuration))
            settle = 1 - pow(1 - t, 3)
        }
        if phase == .working, caption.isEmpty, !pendingCaption.isEmpty, pastedAt == nil,
           now - workingSince >= Self.slowWorkDelay {
            caption = pendingCaption
        }
        return isAnimating
    }

    func reset() {
        transition = nil
        closeFinished = false
        phase = .idle
        caption = ""
        pendingCaption = ""
        pastedAt = nil
        systemMode = false
        levels = [CGFloat](repeating: 0, count: Self.historyLength)
        elapsed = 0
        sawSample = false
        settle = 0
    }

    /// A still frame of a recording for the onboarding panel.
    func showDemo() {
        reset()
        phase = .recording
        // Two spoken phrases with a breath between them.
        for index in 0..<Self.historyLength {
            let t = Double(index) * Self.sampleInterval
            let envelope = max(0, sin(t * 2.3) * 0.6 + sin(t * 5.1 + 1) * 0.35 + 0.25)
            levels[index] = CGFloat(min(1, envelope * (0.75 + 0.25 * sin(t * 37))))
        }
        elapsed = 3.2
    }

    private func startCapture(now: CFTimeInterval) {
        if folded {
            // Still confirming the last paste: open afresh rather than jump
            // from the folded check back to a full pill.
            transition = .opening(since: now)
            closeFinished = false
        }
        phase = .recording
        levels = [CGFloat](repeating: 0, count: Self.historyLength)
        elapsed = 0
        sawSample = false
        lastSampleAt = now
        settle = 0
        pendingCaption = ""
        pastedAt = nil
        hintFadeAt = caption.isEmpty ? .infinity : now + Self.hintDuration
        voicedFrames = 0
    }

    private func enterWorking(now: CFTimeInterval) {
        guard phase != .working else { return }
        if phase != .recording {
            settle = 1
        } else {
            settleStartedAt = now
            // The recording hint has done its job.
            caption = ""
        }
        phase = .working
        workingSince = now
        pendingCaption = ""
    }

    private static func tidy(_ text: String) -> String {
        var result = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if result.hasSuffix("...") {
            result = String(result.dropLast(3)) + "…"
        }
        return result
    }

    private static func sentenceCase(_ text: String) -> String {
        guard let first = text.first, first.isLowercase else { return text }
        return first.uppercased() + text.dropFirst()
    }
}

/// The pieces both surfaces draw: the record dot and check, the sound
/// ribbon, the timer, the health marks, and the caption. Each takes its
/// position, so the floating pill and the island can lay them out apart.
private enum PillArt {
    static let checkRadius: CGFloat = 10
    /// With Reduce Motion the check simply appears in the dot.
    private static let stillCheckRadius: CGFloat = 7.5
    private static let sampleSpacing: CGFloat = 3
    private static let glintPeriod: CFTimeInterval = 1.25
    private static var labelFont: NSFont { .systemFont(ofSize: 13, weight: .medium) }
    /// Captions under the ribbon: the hint and the rare progress word, in
    /// the serif italic of the setup panel's heading.
    private static let captionFont: NSFont = {
        let base = NSFont.systemFont(ofSize: 13, weight: .regular)
        let serif = base.fontDescriptor.withDesign(.serif)?.withSymbolicTraits(.italic)
        return serif.flatMap { NSFont(descriptor: $0, size: 13) } ?? base
    }()

    /// Your voice, and the line it settles into. System mode uses the
    /// macOS accent for both.
    static func captureColors(_ model: PillModel, _ palette: PillPalette) -> (live: NSColor, settled: NSColor) {
        model.systemMode
            ? (NSColor.controlAccentColor, NSColor.controlAccentColor)
            : (palette.voice, palette.ink)
    }

    static func captionString(_ model: PillModel, _ palette: PillPalette?, emphasized: Bool = false, fade: CGFloat = 1,
                              serif: Bool = false, alignment: NSTextAlignment = .natural,
                              size: CGFloat = 13) -> NSAttributedString {
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineBreakMode = .byTruncatingTail
        paragraph.alignment = alignment
        let base = palette.map { emphasized ? $0.primary : $0.secondary } ?? NSColor.labelColor
        let color = base.withAlphaComponent(base.alphaComponent * fade)
        let result = NSMutableAttributedString()
        if model.systemMode {
            result.append(NSAttributedString(string: "System  ", attributes: [
                .font: NSFont.systemFont(ofSize: size - 1, weight: .semibold),
                .foregroundColor: NSColor.controlAccentColor,
                .paragraphStyle: paragraph,
            ]))
        }
        result.append(NSAttributedString(string: model.caption, attributes: [
            .font: size == 13 ? (serif ? Self.captionFont : labelFont)
                : serif ? NSFont(descriptor: Self.captionFont.fontDescriptor, size: size) ?? Self.captionFont
                : NSFont.systemFont(ofSize: size, weight: .medium),
            .foregroundColor: color,
            .paragraphStyle: paragraph,
        ]))
        return result
    }

    /// The record dot at (`markX`, `center`). It breathes with your voice
    /// while recording, then crossfades from red to ink as the sound settles
    /// (a hue blend would pass through purple, which reads as System mode).
    /// After a paste it grows into a disc of `checkRadius` with a check drawn
    /// on it, first riding to `foldTo` when one is given; with `growsCheck`
    /// off it stays the dot.
    static func captureMark(_ model: PillModel, _ palette: PillPalette, x markX: CGFloat, y center: CGFloat,
                            foldTo: CGFloat? = nil, checkRadius: CGFloat = PillArt.checkRadius, growsCheck: Bool = true) {
        let (live, settled) = captureColors(model, palette)
        let latest = model.levels.last ?? 0
        var radius = 4.5 + (model.phase == .recording ? latest * 1.8 : 0)
        if model.phase == .recording, latest > 0.05 {
            live.withAlphaComponent(0.2 * min(1, latest * 1.5)).setFill()
            let halo = radius + 4
            NSBezierPath(ovalIn: NSRect(x: markX - halo, y: center - halo, width: halo * 2, height: halo * 2)).fill()
        }
        var x = markX
        var stroke: CGFloat = 0
        if let pastedAt = model.pastedAt, growsCheck {
            let since = model.clock - pastedAt
            if model.reduceMotion {
                radius = min(Self.stillCheckRadius, checkRadius)
                stroke = 1
            } else {
                if let foldTo {
                    // Riding the fold to the middle, then growing into a
                    // disc as the check is drawn on it.
                    x = markX + (foldTo - markX) * Ease.inOutCubic(CGFloat(since / PillModel.foldDuration))
                }
                let grow = Ease.outBack(CGFloat((since - PillModel.checkDelay) / PillModel.checkGrow))
                radius += (checkRadius - radius) * grow
                stroke = Ease.outCubic(CGFloat((since - PillModel.checkStrokeDelay) / PillModel.checkStroke))
            }
        }
        let dot = NSRect(x: x - radius, y: center - radius, width: radius * 2, height: radius * 2)
        live.withAlphaComponent(1 - model.settle).setFill()
        NSBezierPath(ovalIn: dot).fill()
        settled.withAlphaComponent(model.settle).setFill()
        NSBezierPath(ovalIn: dot).fill()
        if stroke > 0 {
            drawCheck(palette, x: x, y: center, scale: radius / Self.checkRadius, progress: stroke)
        }
    }

    /// An ink disc of `radius` with the check cut out of it, drawn on as
    /// `stroke` goes from 0 to 1.
    static func checkDisc(_ palette: PillPalette, x: CGFloat, y: CGFloat, radius: CGFloat, stroke: CGFloat) {
        guard radius > 0.1 else { return }
        palette.ink.setFill()
        NSBezierPath(ovalIn: NSRect(x: x - radius, y: y - radius, width: radius * 2, height: radius * 2)).fill()
        if stroke > 0 {
            drawCheck(palette, x: x, y: y, scale: radius / Self.checkRadius, progress: stroke)
        }
    }

    /// A check cut out of the disc, drawn on from its short arm to its long one.
    private static func drawCheck(_ palette: PillPalette, x: CGFloat, y center: CGFloat, scale: CGFloat, progress: CGFloat) {
        let points = [(-4.4, 0.3), (-1.2, 3.5), (4.5, -3.2)].map {
            NSPoint(x: x + CGFloat($0.0) * scale, y: center + CGFloat($0.1) * scale)
        }
        let first = hypot(points[1].x - points[0].x, points[1].y - points[0].y)
        let second = hypot(points[2].x - points[1].x, points[2].y - points[1].y)
        var remaining = (first + second) * progress
        let check = NSBezierPath()
        check.move(to: points[0])
        for (from, to, length) in [(points[0], points[1], first), (points[1], points[2], second)] where remaining > 0 {
            let part = min(1, remaining / length)
            check.line(to: NSPoint(x: from.x + (to.x - from.x) * part, y: from.y + (to.y - from.y) * part))
            remaining -= length
        }
        check.lineWidth = 2.2 * scale
        check.lineCapStyle = .round
        check.lineJoinStyle = .round
        palette.body.setStroke()
        check.stroke()
    }

    /// The live sound between `left` and `right`, newest at the right, and
    /// the line it settles into once you let go.
    static func ribbon(_ model: PillModel, from left: CGFloat, to right: CGFloat, y center: CGFloat,
                       tint: NSColor, settled: NSColor, maxAmplitude: CGFloat = 10, taper: CGFloat = 30,
                       glintWidth: CGFloat = 64) {
        guard right > left else { return }
        let spacing = Self.sampleSpacing
        let settle = model.settle
        let levels = model.levels
        let drift = model.phase == .recording && model.sawSample
            ? CGFloat(min(1, (model.clock - model.lastSampleAt) / PillModel.sampleInterval)) : 0
        var points: [NSPoint] = []
        var step = 0
        while step < levels.count {
            let x = right - (CGFloat(step) + drift) * spacing
            if x < left { break }
            let fade = min(1, (x - left) / taper) * min(1, (right - x) / 8 + 0.35)
            let amplitude = levels[levels.count - 1 - step] * maxAmplitude * (1 - settle) * max(0, fade)
            points.append(NSPoint(x: x, y: amplitude))
            step += 1
        }
        points.reverse()

        // The baseline: faint while you talk, the full ink line once settled.
        let baseAlpha = 0.22 + 0.33 * settle
        settled.withAlphaComponent(baseAlpha).setFill()
        let lineHeight: CGFloat = 1 + settle
        NSRect(x: left, y: center - lineHeight / 2, width: right - left, height: lineHeight).fill()

        if settle < 1, points.count > 2 {
            let ribbon = NSBezierPath()
            ribbon.move(to: NSPoint(x: points[0].x, y: center))
            for index in 1..<points.count {
                let previous = NSPoint(x: points[index - 1].x, y: center - points[index - 1].y)
                let current = NSPoint(x: points[index].x, y: center - points[index].y)
                let mid = NSPoint(x: (previous.x + current.x) / 2, y: (previous.y + current.y) / 2)
                ribbon.curve(to: mid, controlPoint1: previous, controlPoint2: previous)
            }
            ribbon.line(to: NSPoint(x: points[points.count - 1].x, y: center))
            for index in stride(from: points.count - 1, to: 0, by: -1) {
                let previous = NSPoint(x: points[index].x, y: center + points[index].y)
                let current = NSPoint(x: points[index - 1].x, y: center + points[index - 1].y)
                let mid = NSPoint(x: (previous.x + current.x) / 2, y: (previous.y + current.y) / 2)
                ribbon.curve(to: mid, controlPoint1: previous, controlPoint2: previous)
            }
            ribbon.close()
            tint.withAlphaComponent(0.92 * (1 - settle)).setFill()
            ribbon.fill()
        }

        // While Sunoto works, a glint runs along the line.
        if model.phase == .working, settle >= 1, model.pastedAt == nil, !model.reduceMotion {
            glint(clock: model.clock, from: left, to: right, y: center, color: settled, width: glintWidth)
        }
    }

    /// A glint running along a line from `left` to `right`: Sunoto is busy.
    static func glint(clock: CFTimeInterval, from left: CGFloat, to right: CGFloat, y center: CGFloat,
                      color: NSColor, width glintWidth: CGFloat) {
        let progress = CGFloat(clock.truncatingRemainder(dividingBy: Self.glintPeriod) / Self.glintPeriod)
        let eased = progress < 0.5 ? 2 * progress * progress : 1 - pow(-2 * progress + 2, 2) / 2
        let x = left - glintWidth + (right - left + glintWidth) * eased
        NSGraphicsContext.saveGraphicsState()
        NSRect(x: left, y: center - 1.5, width: right - left, height: 3).clip()
        NSGradient(colors: [color.withAlphaComponent(0), color, color.withAlphaComponent(0)])?
            .draw(in: NSRect(x: x, y: center - 1.5, width: glintWidth, height: 3), angle: 0)
        NSGraphicsContext.restoreGraphicsState()
    }

    /// The elapsed time, its right edge at `edge` (or its left edge, when
    /// left-aligned), dimmed once you let go.
    static func timer(_ model: PillModel, _ palette: PillPalette, edge: CGFloat, y center: CGFloat,
                      alignment: NSTextAlignment = .right) {
        let seconds = Int(model.elapsed.rounded(.down))
        let text = String(format: "%d:%02d", seconds / 60, seconds % 60)
        let paragraph = NSMutableParagraphStyle()
        paragraph.alignment = alignment
        let attributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.monospacedDigitSystemFont(ofSize: 11.5, weight: .medium),
            .foregroundColor: model.phase == .recording ? palette.secondary : palette.secondary.withAlphaComponent(0.4),
            .paragraphStyle: paragraph,
        ]
        let height: CGFloat = 15
        let width: CGFloat = 40
        (text as NSString).draw(
            in: NSRect(x: alignment == .right ? edge - width : edge, y: center - height / 2, width: width, height: height),
            withAttributes: attributes
        )
    }

    static func spinner(_ model: PillModel, _ palette: PillPalette, x markX: CGFloat, y center: CGFloat) {
        let radius: CGFloat = 6.5
        let ring = NSBezierPath(ovalIn: NSRect(x: markX - radius, y: center - radius, width: radius * 2, height: radius * 2))
        ring.lineWidth = 2
        palette.faint.setStroke()
        ring.stroke()
        guard !model.reduceMotion else { return }
        let start = CGFloat(model.clock.truncatingRemainder(dividingBy: 1.1) / 1.1) * 360
        let arc = NSBezierPath()
        arc.appendArc(withCenter: NSPoint(x: markX, y: center), radius: radius, startAngle: start, endAngle: start + 100)
        arc.lineWidth = 2
        arc.lineCapStyle = .round
        palette.secondary.setStroke()
        arc.stroke()
    }

    static func warning(_ palette: PillPalette, x markX: CGFloat, y center: CGFloat) {
        let radius: CGFloat = 7.5
        palette.amber.setFill()
        NSBezierPath(ovalIn: NSRect(x: markX - radius, y: center - radius, width: radius * 2, height: radius * 2)).fill()
        palette.onAmber.setFill()
        NSBezierPath(roundedRect: NSRect(x: markX - 0.9, y: center - 4.2, width: 1.8, height: 5), xRadius: 0.9, yRadius: 0.9).fill()
        NSBezierPath(ovalIn: NSRect(x: markX - 1.05, y: center + 2.1, width: 2.1, height: 2.1)).fill()
    }

    static func notice(_ model: PillModel, _ palette: PillPalette, x markX: CGFloat, y center: CGFloat) {
        let radius: CGFloat = 4.5
        let tint = model.systemMode ? NSColor.controlAccentColor : palette.ink
        tint.setFill()
        NSBezierPath(ovalIn: NSRect(x: markX - radius, y: center - radius, width: radius * 2, height: radius * 2)).fill()
    }

    /// The mark at the start of the row: the record dot while capturing,
    /// otherwise the mark for the health or message being shown.
    static func mark(_ model: PillModel, _ palette: PillPalette, x: CGFloat, y: CGFloat,
                     foldTo: CGFloat? = nil, checkRadius: CGFloat = PillArt.checkRadius) {
        switch model.phase {
        case .idle, .recording, .working:
            captureMark(model, palette, x: x, y: y, foldTo: foldTo, checkRadius: checkRadius)
        case .busy:
            spinner(model, palette, x: x, y: y)
        case .attention, .error:
            warning(palette, x: x, y: y)
        case .notice:
            notice(model, palette, x: x, y: y)
        }
    }
}

/// The floating pill shown under the menu bar. It never takes clicks.
private final class PillView: NSView {
    static let rowHeight: CGFloat = 40
    static let captionHeight: CGFloat = 24
    static let captureWidth: CGFloat = 232
    static let maxWidth: CGFloat = 440
    private static let ribbonLeft: CGFloat = 38
    /// Room after a message, matching the room before its icon.
    private static let labelInset: CGFloat = 14
    private static let timerWidth: CGFloat = 54

    let model: PillModel
    /// Transparent room around the pill for its shadow and the spring's
    /// overshoot. The onboarding panel's still copy uses none.
    var margin: CGFloat = 16
    /// Width of the pill body, without the margin.
    private var bodyWidth: CGFloat { bounds.width - margin * 2 }

    init(frame frameRect: NSRect, model: PillModel = PillModel()) {
        self.model = model
        super.init(frame: frameRect)
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    override var isFlipped: Bool { true }
    override var isOpaque: Bool { false }

    // MARK: Size

    /// The pill grows to fit its caption. While dictating, widths snap to
    /// 16 pt steps so a changing caption does not make the window twitch; a
    /// message fits its text so the space after it matches the space before
    /// its icon.
    var preferredSize: NSSize {
        let width: CGFloat
        let height: CGFloat
        switch model.phase {
        case .idle:
            width = ceil(Self.captureWidth / 16) * 16
            height = Self.rowHeight
        case .recording, .working:
            let hasCaption = !model.caption.isEmpty || model.systemMode
            let textWidth = hasCaption ? ceil(PillArt.captionString(model, nil, serif: true).size().width) : 0
            width = ceil(max(Self.captureWidth, Self.ribbonLeft + textWidth + 20) / 16) * 16
            height = Self.rowHeight + (hasCaption ? Self.captionHeight : 0)
        case .busy, .attention, .error, .notice:
            let textWidth = ceil(PillArt.captionString(model, nil).size().width)
            width = max(Self.rowHeight * 2, Self.ribbonLeft + textWidth + Self.labelInset)
            height = Self.rowHeight
        }
        return NSSize(width: min(Self.maxWidth, width), height: height)
    }

    /// The window around the pill: its body plus the margin on every side.
    var windowSize: NSSize {
        let body = preferredSize
        return NSSize(width: body.width + margin * 2, height: body.height + margin * 2)
    }

    // MARK: Motion

    /// How the pill enters and leaves this frame (see `PillMotion`).
    private var motion: PillMotion {
        var motion = PillMotion()
        let clock = model.clock
        if model.folded, let pastedAt = model.pastedAt {
            let t = CGFloat((clock - pastedAt) / PillModel.foldDuration)
            motion.anchor = 0.5
            motion.unroll = 1 - Ease.inOutCubic(t)
            motion.contentAlpha = 1 - Ease.clamp(t / 0.45)
        }
        switch model.transition {
        case nil:
            return motion
        case .opening(let since):
            let t = CGFloat((clock - since) / PillModel.openDuration)
            if model.reduceMotion {
                motion.seedAlpha = Ease.clamp(t)
                return motion
            }
            motion.seedScale = 0.5 + 0.5 * Ease.outCubic(t / 0.35)
            motion.seedAlpha = Ease.outCubic(t / 0.3)
            motion.lift = -8 * (1 - Ease.outCubic(t / 0.6))
            motion.unroll = Ease.outBack((t - 0.18) / 0.82)
            motion.contentAlpha = Ease.outCubic((t - 0.25) / 0.5)
        case .closing(let since):
            let t = CGFloat((clock - since) / model.closeDuration)
            if model.reduceMotion {
                motion.seedAlpha = 1 - Ease.clamp(t)
                return motion
            }
            if model.folded {
                // Only the check is left: it shrinks away where it is.
                let shrink = Ease.inOutCubic(t)
                motion.seedScale = 1 - 0.6 * shrink
                motion.seedAlpha = 1 - shrink
                motion.lift = -4 * shrink
                return motion
            }
            motion.contentAlpha = 1 - Ease.clamp(t / 0.35)
            motion.unroll = 1 - Ease.inCubic(t / 0.6)
            let shrink = Ease.inCubic((t - 0.5) / 0.5)
            motion.seedScale = 1 - 0.5 * shrink
            motion.seedAlpha = 1 - shrink
            motion.lift = -4 * shrink
        }
        return motion
    }

    // MARK: Drawing

    override func draw(_ dirtyRect: NSRect) {
        guard let context = NSGraphicsContext.current?.cgContext else { return }
        let palette = PillPalette.resolve(for: effectiveAppearance)
        let motion = self.motion
        let shape = bodyPath(motion)

        // The pill with its own shadow, so the shadow follows the motion.
        context.saveGState()
        context.setAlpha(motion.seedAlpha)
        context.beginTransparencyLayer(auxiliaryInfo: nil)
        if margin > 0 {
            let shadow = NSShadow()
            shadow.shadowBlurRadius = 12
            shadow.shadowOffset = NSSize(width: 0, height: -4)
            shadow.shadowColor = NSColor.black.withAlphaComponent(
                effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? 0.45 : 0.18
            )
            shadow.set()
        }
        palette.body.setFill()
        shape.fill()
        NSShadow().set()
        palette.edge.setStroke()
        shape.lineWidth = 1
        shape.stroke()
        context.endTransparencyLayer()
        context.restoreGState()

        // The mark (dot, spinner, or warning) belongs to the round cap: it
        // arrives first and leaves last. Everything else is laid out at full
        // size and uncovered as the pill unrolls, so nothing reflows.
        context.saveGState()
        shape.addClip()
        context.setAlpha(motion.seedAlpha)
        context.beginTransparencyLayer(auxiliaryInfo: nil)
        let cap = Self.rowHeight / 2
        let scale = motion.seedScale
        context.translateBy(x: margin + capShift(motion) + cap, y: margin + motion.lift + cap)
        context.scaleBy(x: scale, y: scale)
        context.translateBy(x: -cap, y: -cap)
        PillArt.mark(model, palette, x: markX, y: center, foldTo: center)
        context.endTransparencyLayer()
        context.restoreGState()

        guard motion.contentAlpha > 0.01 else { return }
        context.saveGState()
        shape.addClip()
        context.setAlpha(motion.contentAlpha * motion.seedAlpha)
        context.beginTransparencyLayer(auxiliaryInfo: nil)
        context.translateBy(x: margin, y: margin + motion.lift)
        drawContents(palette)
        context.endTransparencyLayer()
        context.restoreGState()
    }

    /// The pill body's width for this frame of the open or close motion.
    private func unrolledWidth(_ motion: PillMotion) -> CGFloat {
        let cap = min(Self.rowHeight, bounds.height - margin * 2)
        return min(bodyWidth + margin - 2, cap + (bodyWidth - cap) * motion.unroll)
    }

    /// How far the round cap has moved in from the left end.
    private func capShift(_ motion: PillMotion) -> CGFloat {
        max(0, bodyWidth - unrolledWidth(motion)) * motion.anchor
    }

    /// The pill's outline for this frame of the open or close motion.
    private func bodyPath(_ motion: PillMotion) -> NSBezierPath {
        let fullHeight = bounds.height - margin * 2
        let cap = min(Self.rowHeight, fullHeight)
        let width = unrolledWidth(motion)
        let height = cap + (fullHeight - cap) * Ease.clamp(motion.unroll)
        // Scale about the centre of the round cap that holds the dot.
        let scale = motion.seedScale
        let left = margin + capShift(motion)
        let center = NSPoint(x: left + cap / 2, y: margin + cap / 2 + motion.lift)
        let rect = NSRect(
            x: center.x + (left - center.x) * scale,
            y: center.y + (margin + motion.lift - center.y) * scale,
            width: width * scale,
            height: height * scale
        ).insetBy(dx: 0.5, dy: 0.5)
        let radius = min(rect.height / 2, 20 * scale)
        return NSBezierPath(roundedRect: rect, xRadius: radius, yRadius: radius)
    }

    private func drawContents(_ palette: PillPalette) {
        switch model.phase {
        case .idle:
            break
        case .recording, .working:
            drawCapture(palette)
        case .busy, .attention, .error, .notice:
            drawLabel(palette)
        }
    }

    private var center: CGFloat { Self.rowHeight / 2 }
    private let markX: CGFloat = 21

    private func drawCapture(_ palette: PillPalette) {
        let (live, settled) = PillArt.captureColors(model, palette)
        let left = Self.ribbonLeft
        let right = bodyWidth - Self.timerWidth
        PillArt.ribbon(model, from: left, to: right, y: center, tint: live, settled: settled)
        PillArt.timer(model, palette, edge: bodyWidth - 16, y: center)

        if !model.caption.isEmpty || model.systemMode {
            let rect = NSRect(x: left, y: Self.rowHeight - 6, width: bodyWidth - left - 18, height: 18)
            // The recording hint fades out before the pill narrows; a
            // System label stays.
            let fade = model.phase == .recording && !model.caption.isEmpty
                ? 1 - Ease.clamp(CGFloat((model.clock - model.hintFadeAt) / PillModel.hintFade)) : 1
            PillArt.captionString(model, palette, fade: fade, serif: true)
                .draw(with: rect, options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
        }
    }

    private func drawLabel(_ palette: PillPalette) {
        let rect = NSRect(x: Self.ribbonLeft, y: center - 8.5, width: bodyWidth - Self.ribbonLeft - Self.labelInset, height: 18)
        PillArt.captionString(model, palette, emphasized: true)
            .draw(with: rect, options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
    }
}

/// The camera notch at the top of a MacBook's built-in screen, in global
/// screen coordinates. Read from the screen every time: notches differ
/// between models, and scaled display modes change their size in points.
private struct NotchGeometry: Equatable {
    let rect: NSRect

    /// Nil for a screen without a notch (an external display, or a Mac
    /// without one).
    init?(screen: NSScreen) {
        self.init(frame: screen.frame, safeTop: screen.safeAreaInsets.top,
                  left: screen.auxiliaryTopLeftArea, right: screen.auxiliaryTopRightArea)
    }

    /// `left` and `right` are the menu bar on either side of the notch. They
    /// are measured inwards from the screen's edges, so it does not matter
    /// which coordinates they are given in.
    init?(frame: NSRect, safeTop height: CGFloat, left: NSRect?, right: NSRect?) {
        guard height > 0, let left, let right else { return nil }
        let minX = frame.minX + left.width
        let maxX = frame.maxX - right.width
        guard maxX > minX else { return nil }
        rect = NSRect(x: minX, y: frame.maxY - height, width: maxX - minX, height: height)
    }
}

/// A value easing toward a target over a fixed time: how much of each part
/// of the island shows. Aiming somewhere new starts from where it is.
private struct Tween {
    private(set) var target: CGFloat
    private var from: CGFloat
    private var start: CFTimeInterval = 0
    private let duration: CFTimeInterval

    init(_ value: CGFloat, duration: CFTimeInterval) {
        target = value
        from = value
        self.duration = duration
    }

    func value(at now: CFTimeInterval) -> CGFloat {
        let t = CGFloat((now - start) / duration)
        return t >= 1 ? target : from + (target - from) * Ease.inOutCubic(t)
    }

    func isMoving(at now: CFTimeInterval) -> Bool {
        from != target && now - start < duration
    }

    mutating func aim(_ newTarget: CGFloat, now: CFTimeInterval) {
        guard newTarget != target else { return }
        from = value(at: now)
        target = newTarget
        start = now
    }

    mutating func snap(_ value: CGFloat) {
        target = value
        from = value
        start = 0
    }
}

/// A value that springs toward a target: the island's size. Every size
/// change uses this one spring, so the island always moves the same way.
/// Aiming somewhere new starts from where it is.
private struct Spring {
    /// Seconds for one swing.
    private static let response: CGFloat = 0.45
    private(set) var target: CGFloat
    private var from: CGFloat
    private var start: CFTimeInterval = 0
    private var damping: CGFloat = 0.82

    init(_ value: CGFloat) {
        target = value
        from = value
    }

    func value(at now: CFTimeInterval) -> CGFloat {
        from + (target - from) * Self.step(CGFloat(now - start), damping: damping)
    }

    func isMoving(at now: CFTimeInterval) -> Bool {
        from != target && now - start < 0.8
    }

    /// Below 1, `damping` overshoots a little; at 1 it does not.
    mutating func aim(_ newTarget: CGFloat, now: CFTimeInterval, damping: CGFloat, animated: Bool) {
        guard newTarget != target else { return }
        from = animated ? value(at: now) : newTarget
        target = newTarget
        start = now
        self.damping = damping
    }

    mutating func snap(_ value: CGFloat) {
        target = value
        from = value
        start = 0
    }

    /// How far a spring let go at rest has got after `x` seconds.
    private static func step(_ x: CGFloat, damping z: CGFloat) -> CGFloat {
        guard x > 0 else { return 0 }
        let w = 2 * CGFloat.pi / response
        if z >= 1 {
            return 1 - exp(-w * x) * (1 + w * x)
        }
        let wd = w * sqrt(1 - z * z)
        return 1 - exp(-z * w * x) * (cos(wd * x) + (z * w / wd) * sin(wd * x))
    }
}

/// The pill grown out of the notch, the Mac's own "Dynamic Island". The
/// notch grows a short pill under the camera that holds what the floating
/// pill shows, in one row: the dot, your voice, the timer. Words get a
/// second row under it. After a paste the row gives way to a check mark in
/// the middle, right under the camera, and the island pulls back into the
/// notch. Every size change is the same spring and every part only fades in
/// place, so nothing travels. The window never moves or resizes; only the
/// shape inside it does.
private final class IslandView: NSView {
    /// How far the island reaches past the notch on each side.
    private static let side: CGFloat = 23
    /// The row under the camera, and the row for words under that.
    private static let rowHeight: CGFloat = 30
    private static let wordsHeight: CGFloat = 22
    /// Room beside and below the island for its shadow and the spring's
    /// overshoot.
    private static let margin: CGFloat = 24
    /// The top corners curve outward into the screen's edge, as the notch's do.
    private static let ear: CGFloat = 6
    private static let compactRadius: CGFloat = 9
    private static let expandedRadius: CGFloat = 19
    /// In the row: the mark this far in from the left edge, the voice and
    /// any message from `textInset`, the timer ending `timerInset` from the
    /// right edge, after the voice's `timerRoom`.
    private static let markInset: CGFloat = 17
    private static let textInset: CGFloat = 31
    private static let timerInset: CGFloat = 14
    private static let timerRoom: CGFloat = 46
    /// Room either side of the words in the second row.
    private static let wordsPadding: CGFloat = 17
    private static let fontSize: CGFloat = 12
    private static let checkRadius: CGFloat = 10
    private static let fadeDuration: CFTimeInterval = 0.18
    /// Closing, the island is back in the notch by now; the last sliver then fades.
    private static let closeFadeAt: CFTimeInterval = 0.32

    /// The window for a notch: as wide as the widest island, as tall as the
    /// notch and both rows, with the margin beside and below.
    static func windowSize(notch: NSSize) -> NSSize {
        NSSize(width: PillView.maxWidth + margin * 2, height: notch.height + rowHeight + wordsHeight + margin)
    }

    /// The window over a notch: flush with the top of the screen and centred
    /// on the notch.
    static func windowFrame(over notch: NotchGeometry) -> NSRect {
        let size = windowSize(notch: notch.rect.size)
        return NSRect(
            x: notch.rect.midX - size.width / 2,
            y: notch.rect.maxY - size.height,
            width: size.width,
            height: size.height
        )
    }

    let model: PillModel
    /// The notch this island grows out of, in points.
    var notch = NSSize(width: 160, height: 28) {
        didSet {
            if notch != oldValue {
                reset()
            }
        }
    }
    private var now: CFTimeInterval = 0
    private var width = Spring(160)
    private var height = Spring(28)
    /// How much of each part shows.
    private var capture = Tween(0, duration: IslandView.fadeDuration)
    private var wordsShown = Tween(0, duration: IslandView.fadeDuration)
    private var check = Tween(0, duration: IslandView.fadeDuration)
    private var messageShown = Tween(0, duration: IslandView.fadeDuration)
    private var busy = Tween(0, duration: IslandView.fadeDuration)
    /// The second row's words and the row's message, kept while they fade.
    private var words: NSAttributedString?
    private var message: NSAttributedString?
    private let palette = PillPalette.island

    init(model: PillModel) {
        self.model = model
        super.init(frame: .zero)
        // Dynamic colours (System mode's accent) resolve as on a dark menu bar.
        appearance = NSAppearance(named: .darkAqua)
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    override var isFlipped: Bool { true }
    override var isOpaque: Bool { false }

    /// Follow the model to `now`: aim the size and each part at what it
    /// shows. Returns whether the island is still moving on its own.
    @discardableResult
    func update(now: CFTimeInterval) -> Bool {
        self.now = now
        var opening = false, closing = false
        switch model.transition {
        case .opening: opening = true
        case .closing: closing = true
        case nil: break
        }
        let phase = model.phase
        let pasted = phase == .working && model.pastedAt != nil
        let capturing = (phase == .recording || phase == .working) && !pasted
        let messaging = phase == .error || phase == .attention || phase == .notice
        let text = wordsText()
        if let text {
            words = text
        }
        if messaging {
            message = PillArt.captionString(model, palette, emphasized: true, size: Self.fontSize + 0.5)
        }
        let shown = !closing
        capture.aim(shown && capturing ? 1 : 0, now: now)
        wordsShown.aim(shown && text != nil ? 1 : 0, now: now)
        check.aim(shown && pasted ? 1 : 0, now: now)
        messageShown.aim(shown && messaging ? 1 : 0, now: now)
        busy.aim(shown && phase == .busy ? 1 : 0, now: now)

        let size = closing ? notch : targetSize(words: text, pasted: pasted, messaging: messaging)
        let damping: CGFloat = closing ? 1 : opening ? 0.72 : 0.82
        width.aim(size.width, now: now, damping: damping, animated: !model.reduceMotion)
        height.aim(size.height, now: now, damping: damping, animated: !model.reduceMotion)
        needsDisplay = true
        return isMoving
    }

    var isMoving: Bool {
        width.isMoving(at: now) || height.isMoving(at: now)
            || [capture, wordsShown, check, messageShown, busy].contains { $0.isMoving(at: now) }
    }

    func reset() {
        width.snap(notch.width)
        height.snap(notch.height)
        capture.snap(0)
        wordsShown.snap(0)
        check.snap(0)
        messageShown.snap(0)
        busy.snap(0)
        words = nil
        message = nil
        needsDisplay = true
    }

    /// The second row: the start hint, a slow "Transcribing…", the System
    /// label, or what is loading. Nil for no second row.
    private func wordsText() -> NSAttributedString? {
        switch model.phase {
        case .recording, .working:
            guard model.pastedAt == nil, !model.caption.isEmpty || model.systemMode else { return nil }
            // The recording hint fades out before its row folds away; a
            // System label stays.
            let fade = model.phase == .recording && !model.caption.isEmpty
                ? 1 - Ease.clamp(CGFloat((model.clock - model.hintFadeAt) / PillModel.hintFade)) : 1
            return PillArt.captionString(model, palette, fade: fade, serif: true, alignment: .center, size: Self.fontSize)
        case .busy:
            guard !model.caption.isEmpty else { return nil }
            return PillArt.captionString(model, palette, serif: true, alignment: .center, size: Self.fontSize)
        case .idle, .attention, .error, .notice:
            return nil
        }
    }

    /// The size the island heads for: the row under the camera, a second
    /// row for words, the notch's own width for the check.
    private func targetSize(words: NSAttributedString?, pasted: Bool, messaging: Bool) -> NSSize {
        let row = notch.height + Self.rowHeight
        if model.phase == .idle {
            return notch
        }
        if pasted {
            return NSSize(width: notch.width, height: row)
        }
        if messaging {
            let wanted = message.map { Self.textInset + ceil($0.size().width) + Self.timerInset + 2 } ?? 0
            return NSSize(width: fit(wanted), height: row)
        }
        guard let words else {
            return NSSize(width: fit(0), height: row)
        }
        return NSSize(width: fit(ceil(words.size().width) + Self.wordsPadding * 2), height: row + Self.wordsHeight)
    }

    private func fit(_ wanted: CGFloat) -> CGFloat {
        min(PillView.maxWidth, max(notch.width + Self.side * 2, wanted))
    }

    // MARK: Drawing

    /// The whole island: it appears at the notch's size, so opening needs no
    /// fade; closing, it fades the last sliver once back in the notch. Reduce
    /// Motion fades it in and out at full size instead.
    private var alpha: CGFloat {
        switch model.transition {
        case .opening(let since):
            return model.reduceMotion ? Ease.clamp(CGFloat((now - since) / 0.25)) : 1
        case .closing(let since):
            if model.reduceMotion {
                return 1 - Ease.clamp(CGFloat((now - since) / 0.25))
            }
            return 1 - Ease.inOutCubic(CGFloat((now - since - Self.closeFadeAt) / Self.fadeDuration))
        case nil:
            return 1
        }
    }

    /// Opening, the parts wait a moment for the island to start growing.
    private var contentDelay: CGFloat {
        guard case .opening(let since) = model.transition, !model.reduceMotion else { return 1 }
        return Ease.clamp(CGFloat((now - since - 0.08) / Self.fadeDuration))
    }

    override func draw(_ dirtyRect: NSRect) {
        guard let context = NSGraphicsContext.current?.cgContext else { return }
        let alpha = self.alpha
        guard alpha > 0.001 else { return }
        let islandWidth = width.value(at: now)
        let islandHeight = max(notch.height - 1, height.value(at: now))
        let rise = Ease.clamp((islandHeight - notch.height) / Self.rowHeight)
        let radius = Self.compactRadius + (Self.expandedRadius - Self.compactRadius) * rise
        let ear = Self.ear * Ease.clamp((islandWidth - notch.width) / 16)
        let shape = islandPath(width: islandWidth, height: islandHeight, ear: ear, radius: radius)

        context.saveGState()
        context.setAlpha(alpha)
        context.beginTransparencyLayer(auxiliaryInfo: nil)
        // A soft shadow under the pill; none at the notch's own size.
        if islandHeight > notch.height + 0.5 {
            let shadow = NSShadow()
            shadow.shadowBlurRadius = 12
            shadow.shadowOffset = NSSize(width: 0, height: -4)
            shadow.shadowColor = NSColor.black.withAlphaComponent(0.45 * Ease.clamp((islandHeight - notch.height) / 18))
            shadow.set()
        }
        palette.body.setFill()
        shape.fill()
        NSShadow().set()

        context.saveGState()
        shape.addClip()
        let left = bounds.midX - islandWidth / 2, right = bounds.midX + islandWidth / 2
        let row = notch.height + Self.rowHeight / 2
        let delay = contentDelay
        layer(capture.value(at: now) * delay) { drawCapture(left: left, right: right, row: row) }
        layer(busy.value(at: now) * delay) { drawBusy(left: left, right: right, row: row) }
        layer(messageShown.value(at: now) * delay) { drawMessage(left: left, right: right, row: row) }
        layer(wordsShown.value(at: now) * delay) { drawWords(left: left, right: right) }
        layer(check.value(at: now)) { drawCheck(row: row) }
        context.restoreGState()

        context.endTransparencyLayer()
        context.restoreGState()
    }

    /// Draw one part at `alpha`, as a whole, so overlaps inside it do not show.
    private func layer(_ alpha: CGFloat, _ draw: () -> Void) {
        guard alpha > 0.01, let context = NSGraphicsContext.current?.cgContext else { return }
        context.saveGState()
        context.setAlpha(alpha)
        context.beginTransparencyLayer(auxiliaryInfo: nil)
        draw()
        context.endTransparencyLayer()
        context.restoreGState()
    }

    /// The island's outline: flush with the top of the screen, its top
    /// corners curving out into the screen's edge, its bottom corners round.
    private func islandPath(width: CGFloat, height: CGFloat, ear: CGFloat, radius: CGFloat) -> NSBezierPath {
        let left = bounds.midX - width / 2, right = bounds.midX + width / 2
        let r = min(radius, height / 2, width / 2)
        // Control points for a quarter circle drawn with a cubic curve.
        let k: CGFloat = 1 - 0.5523
        let path = NSBezierPath()
        path.move(to: NSPoint(x: left - ear, y: 0))
        path.curve(to: NSPoint(x: left, y: ear),
                   controlPoint1: NSPoint(x: left - ear / 3, y: 0), controlPoint2: NSPoint(x: left, y: ear / 3))
        path.line(to: NSPoint(x: left, y: height - r))
        path.curve(to: NSPoint(x: left + r, y: height),
                   controlPoint1: NSPoint(x: left, y: height - r * k), controlPoint2: NSPoint(x: left + r * k, y: height))
        path.line(to: NSPoint(x: right - r, y: height))
        path.curve(to: NSPoint(x: right, y: height - r),
                   controlPoint1: NSPoint(x: right - r * k, y: height), controlPoint2: NSPoint(x: right, y: height - r * k))
        path.line(to: NSPoint(x: right, y: ear))
        path.curve(to: NSPoint(x: right + ear, y: 0),
                   controlPoint1: NSPoint(x: right, y: ear / 3), controlPoint2: NSPoint(x: right + ear / 3, y: 0))
        path.close()
        return path
    }

    /// The row while you dictate: the dot, your voice, the timer.
    private func drawCapture(left: CGFloat, right: CGFloat, row: CGFloat) {
        let (live, settled) = PillArt.captureColors(model, palette)
        PillArt.captureMark(model, palette, x: left + Self.markInset, y: row, growsCheck: false)
        PillArt.ribbon(model, from: left + Self.textInset, to: right - Self.timerRoom, y: row,
                       tint: live, settled: settled, maxAmplitude: 8, taper: 10, glintWidth: 30)
        PillArt.timer(model, palette, edge: right - Self.timerInset, y: row)
    }

    /// Loading looks like a slow wait: the ink dot and a line with a glint,
    /// with what is loading in the row below.
    private func drawBusy(left: CGFloat, right: CGFloat, row: CGFloat) {
        let x = left + Self.markInset, radius: CGFloat = 4.5
        palette.ink.setFill()
        NSBezierPath(ovalIn: NSRect(x: x - radius, y: row - radius, width: radius * 2, height: radius * 2)).fill()
        let lineLeft = left + Self.textInset, lineRight = right - Self.timerInset - 2
        palette.ink.withAlphaComponent(0.55).setFill()
        NSRect(x: lineLeft, y: row - 1, width: lineRight - lineLeft, height: 2).fill()
        if !model.reduceMotion {
            PillArt.glint(clock: now, from: lineLeft, to: lineRight, y: row, color: palette.ink, width: 30)
        }
    }

    /// A message in the row: amber for something to fix, ink for a notice.
    private func drawMessage(left: CGFloat, right: CGFloat, row: CGFloat) {
        let x = left + Self.markInset
        switch model.phase {
        case .notice:
            PillArt.notice(model, palette, x: x, y: row)
        case .error, .attention:
            PillArt.warning(palette, x: x, y: row)
        default:
            break
        }
        let rect = NSRect(x: left + Self.textInset, y: row - 9, width: right - Self.timerInset - left - Self.textInset, height: 18)
        message?.draw(with: rect, options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
    }

    private func drawWords(left: CGFloat, right: CGFloat) {
        let center = notch.height + Self.rowHeight + Self.wordsHeight / 2 - 2
        let rect = NSRect(x: left + Self.wordsPadding, y: center - 8, width: right - left - Self.wordsPadding * 2, height: 18)
        words?.draw(with: rect, options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
    }

    /// After a paste: a check mark in the middle, right under the camera.
    private func drawCheck(row: CGFloat) {
        guard let pastedAt = model.pastedAt else { return }
        let since = CGFloat(now - pastedAt)
        let grow = model.reduceMotion ? 1 : Ease.outBack((since - 0.08) / 0.34)
        let stroke = model.reduceMotion ? 1 : Ease.outCubic((since - 0.24) / 0.3)
        PillArt.checkDisc(palette, x: bounds.midX, y: row, radius: Self.checkRadius * grow, stroke: stroke)
    }
}

/// The island's window. AppKit may move a window that overlaps the menu
/// bar; this one stays exactly where it is put, flush with the top edge.
private final class IslandPanel: NSPanel {
    override func constrainFrameRect(_ frameRect: NSRect, to screen: NSScreen?) -> NSRect {
        frameRect
    }
}

private enum PaletteLayout {
    static let width: CGFloat = 520
    static let headerHeight: CGFloat = 52
    static let rowHeight: CGFloat = 56
    static let rowSpacing: CGFloat = 2
    static let footerHeight: CGFloat = 28
    static let horizontalInset: CGFloat = 12
    static let cornerRadius: CGFloat = 14
    // Use the person's macOS accent instead of imposing an "AI purple".
    // This keeps System mode native in blue, graphite, or their chosen color.
    static let accent = NSColor.controlAccentColor

    static func height(rowCount: Int) -> CGFloat {
        let rows = CGFloat(max(1, min(rowCount, 5)))
        return headerHeight + 1 + 4 + rows * (rowHeight + rowSpacing) + 4 + 1 + footerHeight
    }
}

private struct PaletteSuggestion {
    let id: String
    let title: String
    let subtitle: String?
    let actionLabel: String

    var displayTitle: String {
        let prefix = actionLabel + " "
        guard title.lowercased().hasPrefix(prefix.lowercased()) else { return title }
        return String(title.dropFirst(prefix.count))
    }
}

private final class SystemPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }
}

private final class PaletteTableView: NSTableView {
    var confirmHandler: (() -> Void)?
    var cancelHandler: (() -> Void)?

    override func keyDown(with event: NSEvent) {
        switch event.keyCode {
        case 36, 76: confirmHandler?()
        case 53: cancelHandler?()
        default: super.keyDown(with: event)
        }
    }
}

private final class PaletteRowBackgroundView: NSTableRowView {
    override func drawSelection(in dirtyRect: NSRect) {
        let rect = bounds.insetBy(dx: 4, dy: 2)
        let path = NSBezierPath(roundedRect: rect, xRadius: 8, yRadius: 8)
        PaletteLayout.accent.withAlphaComponent(0.17).setFill()
        path.fill()
    }
}

private final class PaletteRowView: NSTableCellView {
    private let iconView = NSImageView()
    private let titleLabel = NSTextField(labelWithString: "")
    private let subtitleLabel = NSTextField(labelWithString: "")
    private let actionLabel = NSTextField(labelWithString: "")

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)

        iconView.image = NSImage(
            systemSymbolName: "app.fill",
            accessibilityDescription: "Application"
        )?.withSymbolConfiguration(.init(pointSize: 17, weight: .medium))
        iconView.contentTintColor = PaletteLayout.accent
        iconView.imageScaling = .scaleProportionallyDown

        titleLabel.font = .systemFont(ofSize: 14, weight: .medium)
        titleLabel.lineBreakMode = .byTruncatingTail
        subtitleLabel.font = .systemFont(ofSize: 11)
        subtitleLabel.textColor = .secondaryLabelColor
        subtitleLabel.lineBreakMode = .byTruncatingTail
        actionLabel.font = .systemFont(ofSize: 11, weight: .medium)
        actionLabel.textColor = PaletteLayout.accent
        actionLabel.alignment = .right

        let labels = NSStackView(views: [titleLabel, subtitleLabel])
        labels.orientation = .vertical
        labels.alignment = .leading
        labels.spacing = 1

        for view in [iconView, labels, actionLabel] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
        NSLayoutConstraint.activate([
            iconView.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            iconView.centerYAnchor.constraint(equalTo: centerYAnchor),
            iconView.widthAnchor.constraint(equalToConstant: 30),
            iconView.heightAnchor.constraint(equalToConstant: 30),
            labels.leadingAnchor.constraint(equalTo: iconView.trailingAnchor, constant: 10),
            labels.centerYAnchor.constraint(equalTo: centerYAnchor),
            actionLabel.leadingAnchor.constraint(greaterThanOrEqualTo: labels.trailingAnchor, constant: 12),
            actionLabel.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            actionLabel.centerYAnchor.constraint(equalTo: centerYAnchor),
            actionLabel.widthAnchor.constraint(greaterThanOrEqualToConstant: 52),
        ])
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    func update(with suggestion: PaletteSuggestion) {
        titleLabel.stringValue = suggestion.displayTitle
        subtitleLabel.stringValue = suggestion.subtitle ?? "Application"
        actionLabel.stringValue = suggestion.actionLabel + "  ↵"
        setAccessibilityLabel(suggestion.title)
    }
}

/// A compact, focusable result list. It receives only per-session suggestion
/// tokens; native application identifiers remain inside the Rust daemon.
private final class SystemPaletteController: NSObject, NSTableViewDataSource, NSTableViewDelegate, NSWindowDelegate {
    private let panel: NSPanel
    private let queryLabel = NSTextField(labelWithString: "")
    private let table = PaletteTableView()
    private let onSelect: (UInt64, String) -> Void
    private let onCancel: (UInt64) -> Void
    private var sessionID: UInt64?
    private var suggestions: [PaletteSuggestion] = []
    private var closeGeneration = 0

    init(onSelect: @escaping (UInt64, String) -> Void, onCancel: @escaping (UInt64) -> Void) {
        self.onSelect = onSelect
        self.onCancel = onCancel
        panel = SystemPanel(
            contentRect: NSRect(x: 0, y: 0, width: PaletteLayout.width, height: 150),
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        super.init()

        panel.title = "Sunoto System"
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.isFloatingPanel = true
        panel.isReleasedWhenClosed = false
        panel.hidesOnDeactivate = false
        panel.level = .floating
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        panel.animationBehavior = .utilityWindow
        panel.isMovableByWindowBackground = true
        panel.delegate = self

        let header = NSView()
        let modeIcon = NSImageView()
        modeIcon.image = NSImage(
            systemSymbolName: "command.circle.fill",
            accessibilityDescription: "System mode"
        )?.withSymbolConfiguration(.init(pointSize: 15, weight: .semibold))
        modeIcon.contentTintColor = PaletteLayout.accent
        queryLabel.font = .systemFont(ofSize: 15, weight: .medium)
        queryLabel.lineBreakMode = .byTruncatingTail
        let modeLabel = NSTextField(labelWithString: "System")
        modeLabel.font = .systemFont(ofSize: 11, weight: .semibold)
        modeLabel.textColor = PaletteLayout.accent
        modeLabel.alignment = .right

        table.headerView = nil
        table.rowHeight = PaletteLayout.rowHeight
        table.intercellSpacing = NSSize(width: 0, height: PaletteLayout.rowSpacing)
        table.allowsMultipleSelection = false
        table.allowsEmptySelection = false
        table.backgroundColor = .clear
        table.focusRingType = .none
        table.selectionHighlightStyle = .regular
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.action = #selector(confirmSelection)
        table.confirmHandler = { [weak self] in self?.confirmSelection() }
        table.cancelHandler = { [weak self] in self?.cancelSelection() }
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("suggestion"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)

        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        scroll.borderType = .noBorder
        scroll.documentView = table

        let headerSeparator = NSBox()
        headerSeparator.boxType = .separator
        let footerSeparator = NSBox()
        footerSeparator.boxType = .separator
        let help = NSTextField(labelWithString: "Click or press Return to open. Esc cancels.")
        help.font = .systemFont(ofSize: 10.5)
        help.textColor = .tertiaryLabelColor
        help.alignment = .center

        let content = NSVisualEffectView()
        content.material = .popover
        content.blendingMode = .behindWindow
        content.state = .active
        content.wantsLayer = true
        content.layer?.cornerRadius = PaletteLayout.cornerRadius
        content.layer?.masksToBounds = true
        content.layer?.borderWidth = 1
        content.layer?.borderColor = NSColor.separatorColor.withAlphaComponent(0.45).cgColor
        panel.contentView = content

        for view in [header, modeIcon, queryLabel, modeLabel, headerSeparator, scroll, footerSeparator, help] {
            view.translatesAutoresizingMaskIntoConstraints = false
        }
        content.addSubview(header)
        header.addSubview(modeIcon)
        header.addSubview(queryLabel)
        header.addSubview(modeLabel)
        content.addSubview(headerSeparator)
        content.addSubview(scroll)
        content.addSubview(footerSeparator)
        content.addSubview(help)

        NSLayoutConstraint.activate([
            header.topAnchor.constraint(equalTo: content.topAnchor),
            header.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            header.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            header.heightAnchor.constraint(equalToConstant: PaletteLayout.headerHeight),
            modeIcon.leadingAnchor.constraint(equalTo: header.leadingAnchor, constant: 16),
            modeIcon.centerYAnchor.constraint(equalTo: header.centerYAnchor),
            modeIcon.widthAnchor.constraint(equalToConstant: 30),
            modeIcon.heightAnchor.constraint(equalToConstant: 22),
            queryLabel.leadingAnchor.constraint(equalTo: modeIcon.trailingAnchor, constant: 10),
            queryLabel.centerYAnchor.constraint(equalTo: header.centerYAnchor),
            modeLabel.leadingAnchor.constraint(greaterThanOrEqualTo: queryLabel.trailingAnchor, constant: 12),
            modeLabel.trailingAnchor.constraint(equalTo: header.trailingAnchor, constant: -16),
            modeLabel.centerYAnchor.constraint(equalTo: header.centerYAnchor),
            modeLabel.widthAnchor.constraint(equalToConstant: 54),
            headerSeparator.topAnchor.constraint(equalTo: header.bottomAnchor),
            headerSeparator.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: PaletteLayout.horizontalInset),
            headerSeparator.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -PaletteLayout.horizontalInset),
            scroll.topAnchor.constraint(equalTo: headerSeparator.bottomAnchor, constant: 4),
            scroll.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 4),
            scroll.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -4),
            scroll.bottomAnchor.constraint(equalTo: footerSeparator.topAnchor, constant: -4),
            footerSeparator.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: PaletteLayout.horizontalInset),
            footerSeparator.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -PaletteLayout.horizontalInset),
            help.topAnchor.constraint(equalTo: footerSeparator.bottomAnchor),
            help.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 12),
            help.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -12),
            help.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            help.heightAnchor.constraint(equalToConstant: PaletteLayout.footerHeight),
        ])
    }

    func show(sessionID: UInt64, transcript: String, suggestions: [PaletteSuggestion]) {
        self.sessionID = sessionID
        self.suggestions = suggestions
        queryLabel.stringValue = transcript
        panel.setContentSize(NSSize(
            width: PaletteLayout.width,
            height: PaletteLayout.height(rowCount: suggestions.count)
        ))
        table.reloadData()
        if !suggestions.isEmpty {
            table.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        }
        positionPanel()
        closeGeneration += 1
        // Rise into place while fading in; Reduce Motion keeps the fade.
        let resting = panel.frame
        let reduceMotion = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        if !reduceMotion {
            panel.setFrameOrigin(NSPoint(x: resting.minX, y: resting.minY - 10))
        }
        panel.alphaValue = 0
        NSApp.activate(ignoringOtherApps: true)
        panel.makeKeyAndOrderFront(nil)
        panel.makeFirstResponder(table)
        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0.2
            context.timingFunction = CAMediaTimingFunction(controlPoints: 0.2, 0.9, 0.3, 1)
            panel.animator().alphaValue = 1
            panel.animator().setFrame(resting, display: true)
        }
    }

    /// Fade out while sinking a little, then remove the panel. A palette
    /// shown again meanwhile cancels the removal.
    private func closePanel() {
        closeGeneration += 1
        let generation = closeGeneration
        let reduceMotion = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        let sunk = panel.frame.offsetBy(dx: 0, dy: reduceMotion ? 0 : -6)
        NSAnimationContext.runAnimationGroup({ context in
            context.duration = 0.14
            context.timingFunction = CAMediaTimingFunction(name: .easeIn)
            panel.animator().alphaValue = 0
            panel.animator().setFrame(sunk, display: true)
        }, completionHandler: { [weak self] in
            guard let self, self.closeGeneration == generation else { return }
            self.panel.orderOut(nil)
            self.panel.alphaValue = 1
        })
    }

    func dismiss(sessionID: UInt64) {
        guard self.sessionID == sessionID else { return }
        self.sessionID = nil
        suggestions = []
        closePanel()
    }

    func numberOfRows(in tableView: NSTableView) -> Int {
        suggestions.count
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let view = PaletteRowView(frame: .zero)
        view.update(with: suggestions[row])
        return view
    }

    func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
        PaletteRowBackgroundView()
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        cancelSelection()
        return false
    }

    private func positionPanel() {
        guard let screen = NSScreen.main else {
            panel.center()
            return
        }
        let visible = screen.visibleFrame
        panel.setFrameOrigin(NSPoint(
            x: visible.midX - panel.frame.width / 2,
            y: visible.midY - panel.frame.height / 2 + 86
        ))
    }

    @objc private func confirmSelection() {
        guard let sessionID, table.selectedRow >= 0, table.selectedRow < suggestions.count else { return }
        let suggestionID = suggestions[table.selectedRow].id
        self.sessionID = nil
        suggestions = []
        closePanel()
        onSelect(sessionID, suggestionID)
    }

    @objc private func cancelSelection() {
        guard let sessionID else { return }
        self.sessionID = nil
        suggestions = []
        closePanel()
        onCancel(sessionID)
    }
}

/// A numbered circle on a vertical rail. The rail below a finished step
/// turns green, so progress reads top to bottom.
private final class StepMarker: NSView {
    enum State { case waiting, current, done }

    var number = 1
    var state: State = .waiting { didSet { needsDisplay = true } }
    var railAbove = false
    var railBelow = false
    var aboveDone = false { didSet { needsDisplay = true } }

    override var isFlipped: Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        let cx = bounds.midX
        let cy = bounds.midY
        let radius: CGFloat = 12
        func rail(from top: CGFloat, to bottom: CGFloat, done: Bool) {
            (done ? NSColor.systemGreen : NSColor.separatorColor).setFill()
            NSRect(x: cx - 1, y: top, width: 2, height: max(0, bottom - top)).fill()
        }
        if railAbove { rail(from: 0, to: cy - radius - 4, done: aboveDone) }
        if railBelow { rail(from: cy + radius + 4, to: bounds.maxY, done: state == .done) }

        let circle = NSBezierPath(ovalIn: NSRect(x: cx - radius, y: cy - radius, width: radius * 2, height: radius * 2))
        switch state {
        case .done:
            NSColor.systemGreen.setFill()
            circle.fill()
            let check = NSBezierPath()
            check.move(to: NSPoint(x: cx - 4.5, y: cy + 0.2))
            check.line(to: NSPoint(x: cx - 1.3, y: cy + 3.6))
            check.line(to: NSPoint(x: cx + 4.8, y: cy - 3.6))
            check.lineWidth = 2
            check.lineCapStyle = .round
            check.lineJoinStyle = .round
            NSColor.white.setStroke()
            check.stroke()
        case .current:
            NSColor.controlAccentColor.setFill()
            circle.fill()
            drawNumber(color: .white)
        case .waiting:
            circle.lineWidth = 1.5
            NSColor.tertiaryLabelColor.setStroke()
            circle.stroke()
            drawNumber(color: .secondaryLabelColor)
        }
    }

    private func drawNumber(color: NSColor) {
        let paragraph = NSMutableParagraphStyle()
        paragraph.alignment = .center
        let attributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.monospacedDigitSystemFont(ofSize: 12, weight: .semibold),
            .foregroundColor: color,
            .paragraphStyle: paragraph,
        ]
        ("\(number)" as NSString).draw(
            in: NSRect(x: bounds.midX - 12, y: bounds.midY - 8, width: 24, height: 16),
            withAttributes: attributes
        )
    }
}

/// One permission step. The current step sits on a soft accent wash.
private final class StepRowView: NSView {
    var highlighted = false { didSet { needsDisplay = true } }

    override var isFlipped: Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        guard highlighted else { return }
        let path = NSBezierPath(roundedRect: bounds, xRadius: 12, yRadius: 12)
        NSColor.controlAccentColor.withAlphaComponent(0.09).setFill()
        path.fill()
        NSColor.controlAccentColor.withAlphaComponent(0.28).setStroke()
        path.lineWidth = 1
        path.stroke()
    }
}

/// First-run permission guide. Only the next missing permission is actionable;
/// the panel stays open until every grant is live and the user clicks Done.
private final class OnboardingController: NSObject, NSWindowDelegate {
    private struct Row {
        let key: String
        let permission: String
        let title: String
        let subtitle: String
        let anchor: String
    }

    private static let width: CGFloat = 500
    private static let inset: CGFloat = 32

    // Accessibility covers both noticing the shortcut in other apps and
    // pasting into them; Input Monitoring would only add the first half.
    private let rows: [Row] = [
        Row(
            key: "accessibility",
            permission: "accessibility",
            title: "Accessibility",
            subtitle: "Notices %@ and pastes your words.",
            anchor: "Privacy_Accessibility"
        ),
        Row(
            key: "microphone",
            permission: "microphone",
            title: "Microphone",
            subtitle: "Hears you while you dictate.",
            anchor: "Privacy_Microphone"
        ),
    ]

    private let panel: NSPanel
    private var markers: [String: StepMarker] = [:]
    private var rowViews: [String: StepRowView] = [:]
    private var titles: [String: NSTextField] = [:]
    private var buttons: [String: NSButton] = [:]
    private var allowedLabels: [String: NSTextField] = [:]
    private var subtitles: [String: NSTextField] = [:]
    private let hint = NSTextField(labelWithString: "")
    private var attempted: Set<String> = []
    private let progressLabel = NSTextField(labelWithString: "")
    private let doneButton: NSButton
    private let onAction: (String) -> Void
    private let onDone: () -> Void
    private(set) var visible = false
    private var dismissed = false

    private static func serifFont(ofSize size: CGFloat, weight: NSFont.Weight) -> NSFont {
        let base = NSFont.systemFont(ofSize: size, weight: weight)
        guard let descriptor = base.fontDescriptor.withDesign(.serif) else { return base }
        return NSFont(descriptor: descriptor, size: size) ?? base
    }

    private static func wrappingLabel(_ text: String, size: CGFloat, color: NSColor, alignment: NSTextAlignment, width: CGFloat) -> NSTextField {
        let label = NSTextField(wrappingLabelWithString: text)
        label.font = .systemFont(ofSize: size)
        label.textColor = color
        label.alignment = alignment
        label.preferredMaxLayoutWidth = width
        label.isSelectable = false
        return label
    }

    init(onAction: @escaping (String) -> Void, onDone: @escaping () -> Void) {
        self.onAction = onAction
        self.onDone = onDone
        doneButton = NSButton(title: "Done", target: nil, action: nil)
        panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: Self.width, height: 520),
            styleMask: [.titled, .closable, .nonactivatingPanel, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        super.init()

        panel.title = "Sunoto"
        panel.titleVisibility = .hidden
        panel.titlebarAppearsTransparent = true
        panel.delegate = self
        panel.isFloatingPanel = true
        panel.level = .floating
        panel.isReleasedWhenClosed = false
        panel.hidesOnDeactivate = false
        panel.isMovableByWindowBackground = true
        panel.animationBehavior = .utilityWindow

        let textWidth = Self.width - Self.inset * 2

        // The pill as it will look while you talk, so setup also shows how to use it.
        let hero = PillView(frame: NSRect(x: 0, y: 0, width: PillView.captureWidth, height: PillView.rowHeight))
        hero.margin = 0
        hero.model.showDemo()
        hero.translatesAutoresizingMaskIntoConstraints = false
        hero.setAccessibilityElement(true)
        hero.setAccessibilityRole(.image)
        hero.setAccessibilityLabel("The Sunoto pill while recording")
        hint.font = .systemFont(ofSize: 12.5, weight: .medium)
        hint.textColor = .secondaryLabelColor
        hint.alignment = .center

        let heading = NSTextField(labelWithString: "Let Sunoto listen")
        heading.font = Self.serifFont(ofSize: 27, weight: .semibold)
        heading.textColor = .labelColor
        heading.alignment = .center
        let subtitle = Self.wrappingLabel(
            "macOS asks for two permissions, one at a time. Nothing you say leaves this Mac.",
            size: 13,
            color: .secondaryLabelColor,
            alignment: .center,
            width: textWidth - 40
        )

        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .centerX
        stack.spacing = 0
        stack.edgeInsets = NSEdgeInsets(top: 44, left: Self.inset - 10, bottom: 22, right: Self.inset - 10)
        stack.addArrangedSubview(hero)
        stack.setCustomSpacing(10, after: hero)
        stack.addArrangedSubview(hint)
        stack.setCustomSpacing(22, after: hint)
        stack.addArrangedSubview(heading)
        stack.setCustomSpacing(6, after: heading)
        stack.addArrangedSubview(subtitle)
        stack.setCustomSpacing(22, after: subtitle)
        NSLayoutConstraint.activate([
            hero.widthAnchor.constraint(equalToConstant: PillView.captureWidth),
            hero.heightAnchor.constraint(equalToConstant: PillView.rowHeight),
        ])

        for (index, row) in rows.enumerated() {
            let marker = StepMarker()
            marker.number = index + 1
            marker.railAbove = index > 0
            marker.railBelow = index < rows.count - 1
            markers[row.key] = marker

            let title = NSTextField(labelWithString: row.title)
            title.font = .systemFont(ofSize: 14, weight: .semibold)
            title.textColor = .labelColor
            titles[row.key] = title
            let sub = NSTextField(labelWithString: row.subtitle)
            subtitles[row.key] = sub
            sub.font = .systemFont(ofSize: 12)
            sub.textColor = .secondaryLabelColor
            sub.lineBreakMode = .byTruncatingTail
            let labels = NSStackView(views: [title, sub])
            labels.orientation = .vertical
            labels.alignment = .leading
            labels.spacing = 2

            let button = NSButton(title: "Allow", target: self, action: #selector(openPermission(_:)))
            button.bezelStyle = .rounded
            button.bezelColor = .controlAccentColor
            button.font = .systemFont(ofSize: 12.5, weight: .semibold)
            button.identifier = NSUserInterfaceItemIdentifier(row.key)
            buttons[row.key] = button

            let allowed = NSTextField(labelWithString: "Allowed")
            allowed.font = .systemFont(ofSize: 12, weight: .medium)
            allowed.textColor = .systemGreen
            allowed.alignment = .right
            allowedLabels[row.key] = allowed

            let rowView = StepRowView()
            rowViews[row.key] = rowView
            for view in [marker, labels, button, allowed] {
                view.translatesAutoresizingMaskIntoConstraints = false
                rowView.addSubview(view)
            }
            rowView.translatesAutoresizingMaskIntoConstraints = false
            NSLayoutConstraint.activate([
                rowView.widthAnchor.constraint(equalToConstant: textWidth + 20),
                rowView.heightAnchor.constraint(equalToConstant: 64),
                marker.leadingAnchor.constraint(equalTo: rowView.leadingAnchor, constant: 10),
                marker.topAnchor.constraint(equalTo: rowView.topAnchor),
                marker.bottomAnchor.constraint(equalTo: rowView.bottomAnchor),
                marker.widthAnchor.constraint(equalToConstant: 28),
                labels.leadingAnchor.constraint(equalTo: marker.trailingAnchor, constant: 12),
                labels.centerYAnchor.constraint(equalTo: rowView.centerYAnchor),
                labels.trailingAnchor.constraint(lessThanOrEqualTo: button.leadingAnchor, constant: -12),
                button.trailingAnchor.constraint(equalTo: rowView.trailingAnchor, constant: -14),
                button.centerYAnchor.constraint(equalTo: rowView.centerYAnchor),
                button.widthAnchor.constraint(greaterThanOrEqualToConstant: 96),
                allowed.trailingAnchor.constraint(equalTo: rowView.trailingAnchor, constant: -16),
                allowed.centerYAnchor.constraint(equalTo: rowView.centerYAnchor),
            ])
            stack.addArrangedSubview(rowView)
        }

        let rule = NSBox()
        rule.boxType = .separator
        rule.translatesAutoresizingMaskIntoConstraints = false
        stack.setCustomSpacing(20, after: stack.arrangedSubviews.last!)
        stack.addArrangedSubview(rule)
        rule.widthAnchor.constraint(equalToConstant: textWidth + 20).isActive = true

        progressLabel.font = .systemFont(ofSize: 12)
        progressLabel.textColor = .secondaryLabelColor
        progressLabel.lineBreakMode = .byTruncatingTail
        progressLabel.setContentHuggingPriority(.defaultLow, for: .horizontal)
        progressLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        doneButton.target = self
        doneButton.action = #selector(finishOnboarding)
        doneButton.bezelStyle = .rounded
        doneButton.font = .systemFont(ofSize: 12.5, weight: .semibold)
        doneButton.keyEquivalent = "\r"
        doneButton.isEnabled = false
        doneButton.widthAnchor.constraint(greaterThanOrEqualToConstant: 88).isActive = true

        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        let footer = NSStackView(views: [progressLabel, spacer, doneButton])
        footer.orientation = .horizontal
        footer.alignment = .centerY
        footer.spacing = 12
        footer.translatesAutoresizingMaskIntoConstraints = false
        footer.widthAnchor.constraint(equalToConstant: textWidth).isActive = true
        stack.setCustomSpacing(16, after: rule)
        stack.addArrangedSubview(footer)

        stack.translatesAutoresizingMaskIntoConstraints = false
        let content = NSView()
        content.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: content.topAnchor),
            stack.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            stack.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            content.widthAnchor.constraint(equalToConstant: Self.width),
        ])
        panel.contentView = content
        content.layoutSubtreeIfNeeded()
        panel.setContentSize(NSSize(width: Self.width, height: content.fittingSize.height))
        setShortcut("Right Option")
        update(accessibility: false, microphone: false, reveal: false)
    }

    func update(accessibility: Bool, microphone: Bool, shortcut: String?) {
        if let shortcut, !shortcut.isEmpty {
            setShortcut(shortcut)
        }
        update(accessibility: accessibility, microphone: microphone, reveal: true)
    }

    /// Teach the real shortcut: tap to start, tap again to paste.
    private func setShortcut(_ label: String) {
        hint.stringValue = "Tap \(label), talk, then tap it again to paste."
        for row in rows {
            subtitles[row.key]?.stringValue = row.subtitle.replacingOccurrences(of: "%@", with: label)
        }
    }

    private func update(accessibility: Bool, microphone: Bool, reveal: Bool) {
        let orderedStates = [accessibility, microphone]
        let firstMissing = orderedStates.firstIndex(of: false)
        for (index, row) in rows.enumerated() {
            let granted = orderedStates[index]
            let actionable = firstMissing == index
            markers[row.key]?.state = granted ? .done : (actionable ? .current : .waiting)
            markers[row.key]?.aboveDone = index > 0 && orderedStates[index - 1]
            rowViews[row.key]?.highlighted = actionable
            titles[row.key]?.textColor = granted || actionable ? .labelColor : .secondaryLabelColor
            allowedLabels[row.key]?.isHidden = !granted
            let button = buttons[row.key]
            button?.isHidden = !actionable
            button?.title = attempted.contains(row.key) ? "Open Settings" : "Allow"
        }
        if let firstMissing {
            progressLabel.stringValue = "Step \(firstMissing + 1) of \(rows.count): \(rows[firstMissing].title)"
        } else {
            progressLabel.stringValue = "All set. Click Done and Sunoto restarts once."
        }
        doneButton.isEnabled = firstMissing == nil
        if reveal {
            show()
        }
    }

    func hide() {
        visible = false
        panel.orderOut(nil)
    }

    private func show() {
        guard !dismissed else { return }
        let firstShow = !visible
        visible = true
        if let screen = NSScreen.main {
            let frame = screen.visibleFrame
            panel.setFrameOrigin(NSPoint(
                x: frame.midX - panel.frame.width / 2,
                y: frame.midY - panel.frame.height / 2
            ))
        }
        if firstShow {
            // Steal focus once so the buttons are clickable right away;
            // later updates leave the user's focus alone.
            NSApp.activate(ignoringOtherApps: true)
        }
        panel.orderFront(nil)
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        dismissed = true
        visible = false
        return true
    }

    @objc private func openPermission(_ sender: NSButton) {
        guard let key = sender.identifier?.rawValue,
              let row = rows.first(where: { $0.key == key }) else { return }
        if attempted.contains(key) {
            guard let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(row.anchor)") else { return }
            NSWorkspace.shared.open(url)
            return
        }
        attempted.insert(key)
        sender.isEnabled = false
        sender.title = "Open Settings"
        onAction(row.permission)
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) { [weak sender] in
            sender?.isEnabled = true
        }
    }

    @objc private func finishOnboarding() {
        guard doneButton.isEnabled else { return }
        hide()
        onDone()
    }
}

// MARK: Menu bar

/// The menu bar item: a waveform whose menu shows Sunoto's status, the
/// recent dictations (a click copies one), Check for Updates and the
/// automatic updates switch. The daemon sends the contents as `menu`
/// messages and the answers to Check for Updates as `alert`; picks go back
/// through `emit`.
private final class MenuBarController: NSObject, NSMenuDelegate {
    private var item: NSStatusItem?
    private let menu = NSMenu()
    private var state: [String: Any] = [:]
    private var alertOpen = false
    private let emit: ([String: Any]) -> Void
    private let relative: RelativeDateTimeFormatter = {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .short
        return formatter
    }()

    init(emit: @escaping ([String: Any]) -> Void) {
        self.emit = emit
        super.init()
        menu.delegate = self
        menu.autoenablesItems = false
    }

    /// Put the icon in the menu bar, once the app has launched.
    func install() {
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        item.button?.image = MenuBarController.glyph()
        item.button?.toolTip = "Sunoto"
        item.button?.setAccessibilityLabel("Sunoto")
        item.menu = menu
        self.item = item
    }

    func update(_ message: [String: Any]) {
        state = message
    }

    /// Five rounded bars, a template image that macOS tints for a light or
    /// dark menu bar.
    static func glyph() -> NSImage {
        let image = NSImage(size: NSSize(width: 18, height: 18), flipped: false) { _ in
            NSColor.black.setFill()
            var x: CGFloat = 2.5
            for height: CGFloat in [5, 10, 14, 9, 5] {
                let bar = NSRect(x: x, y: 9 - height / 2, width: 2, height: height)
                NSBezierPath(roundedRect: bar, xRadius: 1, yRadius: 1).fill()
                x += 3.25
            }
            return true
        }
        image.isTemplate = true
        return image
    }

    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        let status = NSMenuItem(title: "", action: nil, keyEquivalent: "")
        status.attributedTitle = NSAttributedString(
            string: state["status"] as? String ?? "Sunoto is starting…",
            attributes: [.font: NSFont.boldSystemFont(ofSize: NSFont.systemFontSize)]
        )
        status.image = MenuBarController.dot(
            (state["ready"] as? Bool ?? false) ? .systemGreen : .systemOrange
        )
        status.isEnabled = false
        menu.addItem(status)
        if let hint = state["hint"] as? String {
            menu.addItem(note(hint))
        }
        menu.addItem(.separator())

        let recent = NSMenuItem(title: "Recent Dictations", action: nil, keyEquivalent: "")
        recent.submenu = recentMenu()
        menu.addItem(recent)
        menu.addItem(.separator())

        let checking = state["checking"] as? Bool ?? false
        let check = action(checking ? "Checking for Updates…" : "Check for Updates…", #selector(checkForUpdates))
        check.isEnabled = !checking
        menu.addItem(check)
        if let line = state["update_note"] as? String {
            menu.addItem(note(line))
        }
        let blocked = state["update_blocked"] as? String
        let automatic = action("Install Updates Automatically", #selector(toggleAutomaticUpdates))
        automatic.state = (state["auto_update"] as? Bool ?? false) && blocked == nil ? .on : .off
        automatic.isEnabled = blocked == nil && !state.isEmpty
        automatic.toolTip = blocked
        menu.addItem(automatic)
        menu.addItem(.separator())
        let version = (state["version"] as? String).map { "Sunoto \($0)" } ?? "Sunoto"
        menu.addItem(note(version))
    }

    private func recentMenu() -> NSMenu {
        let submenu = NSMenu()
        submenu.autoenablesItems = false
        let rows = (state["recent"] as? [[String: Any]]) ?? []
        guard state["history_enabled"] as? Bool ?? false else {
            submenu.addItem(note("History is off"))
            return submenu
        }
        guard !rows.isEmpty else {
            submenu.addItem(note("No dictations yet"))
            return submenu
        }
        let now = Date()
        for row in rows {
            guard let text = row["text"] as? String else { continue }
            let item = action(MenuBarController.preview(text), #selector(copyDictation(_:)))
            item.representedObject = text
            item.toolTip = "Click to copy"
            if let ms = (row["at_ms"] as? NSNumber)?.doubleValue {
                let when = relative.localizedString(for: Date(timeIntervalSince1970: ms / 1000), relativeTo: now)
                let title = NSMutableAttributedString(
                    string: item.title,
                    attributes: [.font: NSFont.menuFont(ofSize: 0)]
                )
                title.append(NSAttributedString(
                    string: "   \(when)",
                    attributes: [
                        .font: NSFont.menuFont(ofSize: NSFont.smallSystemFontSize),
                        .foregroundColor: NSColor.secondaryLabelColor,
                    ]
                ))
                item.attributedTitle = title
            }
            submenu.addItem(item)
        }
        submenu.addItem(.separator())
        submenu.addItem(action("Clear History…", #selector(clearHistory)))
        return submenu
    }

    /// One line, at most 48 characters.
    static func preview(_ text: String) -> String {
        let line = text.split(whereSeparator: { $0.isWhitespace }).joined(separator: " ")
        return line.count > 48 ? String(line.prefix(47)) + "…" : line
    }

    private static func dot(_ color: NSColor) -> NSImage {
        NSImage(size: NSSize(width: 8, height: 8), flipped: false) { rect in
            color.setFill()
            NSBezierPath(ovalIn: rect).fill()
            return true
        }
    }

    private func note(_ title: String) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        item.isEnabled = false
        return item
    }

    private func action(_ title: String, _ selector: Selector) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: selector, keyEquivalent: "")
        item.target = self
        return item
    }

    @objc private func copyDictation(_ sender: NSMenuItem) {
        guard let text = sender.representedObject as? String else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }

    @objc private func checkForUpdates() {
        state["checking"] = true
        emit(["type": "check_updates"])
    }

    @objc private func toggleAutomaticUpdates() {
        let on = !(state["auto_update"] as? Bool ?? false)
        state["auto_update"] = on
        emit(["type": "set_auto_update", "on": on])
    }

    @objc private func clearHistory() {
        let alert = NSAlert()
        alert.messageText = "Delete all dictation history?"
        alert.informativeText = "This removes every dictation Sunoto kept on this Mac. It can't be undone."
        alert.addButton(withTitle: "Delete")
        alert.addButton(withTitle: "Cancel")
        alert.buttons.first?.hasDestructiveAction = true
        if present(alert) == .alertFirstButtonReturn {
            state["recent"] = []
            emit(["type": "clear_history"])
        }
    }

    /// A message from the daemon, such as the answer to Check for Updates.
    func showAlert(_ message: [String: Any]) {
        let alert = NSAlert()
        alert.messageText = message["title"] as? String ?? "Sunoto"
        alert.informativeText = message["text"] as? String ?? ""
        let offerInstall = message["offer_install"] as? Bool ?? false
        if offerInstall {
            alert.addButton(withTitle: "Install Now")
            alert.addButton(withTitle: "Later")
        } else {
            alert.addButton(withTitle: "OK")
        }
        if present(alert) == .alertFirstButtonReturn, offerInstall {
            emit(["type": "install_update"])
        }
    }

    /// Run one alert in front of the other apps; a second one while it is
    /// open is dropped.
    private func present(_ alert: NSAlert) -> NSApplication.ModalResponse? {
        guard !alertOpen else { return nil }
        alertOpen = true
        defer { alertOpen = false }
        NSApp.activate(ignoringOtherApps: true)
        return alert.runModal()
    }
}

private final class OverlayApp: NSObject, NSApplicationDelegate {
    private let model = PillModel()
    private var panel = OverlayApp.makePillPanel()
    private lazy var pill = PillView(frame: NSRect(x: 0, y: 0, width: PillView.captureWidth, height: PillView.rowHeight), model: model)
    /// The island's window, made the first time the pill opens on a notch.
    private var islandPanel: NSPanel?
    private lazy var island = IslandView(model: model)
    /// The notch the island is over.
    private var islandNotch: NotchGeometry?
    /// SUNOTO_OVERLAY_STYLE: "auto" (the default) grows the pill out of the
    /// notch on a screen that has one; "pill" always floats it.
    private let islandAllowed = ProcessInfo.processInfo.environment["SUNOTO_OVERLAY_STYLE"] != "pill"
    /// The island window's frame as last logged.
    private var loggedIslandFrame: NSRect?
    private var palette: SystemPaletteController!
    private var onboarding: OnboardingController!
    private var menuBar: MenuBarController!
    private var visible = false
    private var displayClock: Timer?
    /// A close waiting for the check mark to be seen.
    private var deferredHide: DispatchWorkItem?
    private let stdoutLock = NSLock()

    override init() {
        super.init()
        palette = SystemPaletteController(
            onSelect: { [weak self] sessionID, suggestionID in
                self?.emit([
                    "type": "system_selection",
                    "session_id": sessionID,
                    "suggestion_id": suggestionID,
                ])
            },
            onCancel: { [weak self] sessionID in
                self?.emit(["type": "system_cancelled", "session_id": sessionID])
            }
        )
        onboarding = OnboardingController(
            onAction: { [weak self] permission in
                self?.emit(["type": "permission_action", "permission": permission])
            },
            onDone: { [weak self] in
                self?.emit(["type": "onboarding_done"])
            }
        )
        menuBar = MenuBarController(emit: { [weak self] message in
            self?.emit(message)
        })
        pill.autoresizingMask = [.width, .height]
        panel.contentView = pill
    }

    /// A window for the pill that never takes clicks. The island's sits
    /// above the menu bar (level 24) and its status items (25) but below open
    /// menus (101), so a menu still covers it; the floating pill keeps the
    /// status items' level.
    private static func makePillPanel(island: Bool = false) -> NSPanel {
        let rect = NSRect(x: 0, y: 0, width: PillView.captureWidth, height: PillView.rowHeight)
        let style: NSWindow.StyleMask = [.borderless, .nonactivatingPanel]
        let panel = island
            ? IslandPanel(contentRect: rect, styleMask: style, backing: .buffered, defer: false) as NSPanel
            : NSPanel(contentRect: rect, styleMask: style, backing: .buffered, defer: false)
        panel.isOpaque = false
        panel.backgroundColor = .clear
        // The pill draws its own shadow so it follows the open and close
        // motion; a window shadow would outline the whole margin.
        panel.hasShadow = false
        panel.ignoresMouseEvents = true
        panel.isReleasedWhenClosed = false
        panel.level = island ? NSWindow.Level(rawValue: NSWindow.Level.mainMenu.rawValue + 3) : .statusBar
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
        panel.hidesOnDeactivate = false
        return panel
    }

    /// macOS can leave the hidden pill tied to one desktop even though it
    /// joins all of them: after an hour or so it opened on another desktop,
    /// out of sight of the window being dictated into. A new window joins
    /// every desktop again.
    private func moveToFreshPanel() {
        if model.surface == .island, let stuck = islandPanel {
            let fresh = OverlayApp.makePillPanel(island: true)
            stuck.contentView = nil
            stuck.orderOut(nil)
            fresh.contentView = island
            fresh.setFrame(stuck.frame, display: false)
            islandPanel = fresh
            fresh.alphaValue = 1
            fresh.orderFrontRegardless()
        } else {
            let stuck = panel
            panel = OverlayApp.makePillPanel()
            stuck.contentView = nil
            stuck.orderOut(nil)
            panel.contentView = pill
            panel.setFrame(stuck.frame, display: false)
            // The old window may have been mid-resize: head for the target again.
            frameTarget = nil
            panel.alphaValue = 1
            panel.orderFrontRegardless()
        }
        FileHandle.standardError.write(Data("[overlay] pill was on another desktop; moved it to a new window\n".utf8))
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        hide(animated: false)
        menuBar.install()
        NotificationCenter.default.addObserver(
            forName: NSApplication.didChangeScreenParametersNotification, object: nil, queue: .main
        ) { [weak self] _ in
            self?.screensChanged()
        }
        emit(["type": "ready", "backend": "overlay"])
        DispatchQueue.global(qos: .utility).async { self.pumpStdin() }
    }

    private func emit(_ object: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: object) else { return }
        stdoutLock.lock()
        defer { stdoutLock.unlock() }
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data([0x0A]))
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
        let now = CACurrentMediaTime()
        switch message["type"] as? String {
        case "show":
            model.beginIfIdle(now: now)
            show()
        case "hide":
            hide(animated: true)
        case "pasted":
            model.markPasted(now: now)
        case "recording":
            let peak = number(message["peak"])
            let rms = number(message["rms"])
            let level = min(1.0, max(0.0, CGFloat(peak * 8.0), CGFloat(rms * 35.0)))
            model.pushLevel(level, elapsed: number(message["elapsed_s"]), now: now)
        case "status":
            model.setStatus((message["text"] as? String) ?? "", error: message["error"] as? Bool ?? false, now: now)
        case "state":
            let name = (message["name"] as? String) ?? ""
            if name == "ready" {
                if model.phase == .busy || model.phase == .attention {
                    hide(animated: true)
                }
            } else {
                model.setHealth(name: name, detail: (message["detail"] as? String) ?? "")
                show()
            }
        case "onboarding":
            onboarding.update(
                accessibility: message["accessibility"] as? Bool ?? false,
                microphone: message["microphone"] as? Bool ?? false,
                shortcut: message["shortcut"] as? String
            )
        case "system_palette":
            guard let sessionID = uint64(message["session_id"]),
                  let transcript = message["transcript"] as? String,
                  let rows = message["suggestions"] as? [[String: Any]] else { return }
            let suggestions = rows.compactMap { row -> PaletteSuggestion? in
                guard let id = row["suggestion_id"] as? String,
                      let title = row["title"] as? String,
                      let actionLabel = row["action_label"] as? String else { return nil }
                return PaletteSuggestion(
                    id: id,
                    title: title,
                    subtitle: row["subtitle"] as? String,
                    actionLabel: actionLabel
                )
            }
            hide(animated: false)
            palette.show(sessionID: sessionID, transcript: transcript, suggestions: suggestions)
        case "dismiss_system_palette":
            if let sessionID = uint64(message["session_id"]) {
                palette.dismiss(sessionID: sessionID)
            }
        case "menu":
            menuBar.update(message)
            return
        case "alert":
            menuBar.showAlert(message)
            return
        case "segment", "clear":
            break
        case "shutdown":
            NSApp.terminate(nil)
        default:
            break
        }
        if model.surface == .island {
            island.update(now: now)
        } else {
            pill.needsDisplay = true
        }
        if visible {
            if model.surface == .floating {
                fitToContent(animated: true)
            }
            runClockIfNeeded()
        }
    }

    private func number(_ value: Any?) -> Double {
        if let value = value as? NSNumber { return value.doubleValue }
        if let value = value as? String { return Double(value) ?? 0.0 }
        return 0.0
    }

    private func uint64(_ value: Any?) -> UInt64? {
        if let value = value as? NSNumber { return value.uint64Value }
        if let value = value as? String { return UInt64(value) }
        return nil
    }

    // MARK: Display clock

    private func runClockIfNeeded() {
        let moving = model.isAnimating || (model.surface == .island && island.isMoving)
        guard activePanel.isVisible, moving, displayClock == nil else { return }
        let clock = Timer(timeInterval: 1.0 / 60.0, repeats: true) { [weak self] _ in
            self?.advance()
        }
        RunLoop.main.add(clock, forMode: .common)
        displayClock = clock
    }

    private func advance() {
        let now = CACurrentMediaTime()
        var keepRunning = model.tick(now: now)
        if model.closeFinished {
            finishHiding()
            return
        }
        if model.surface == .island {
            keepRunning = island.update(now: now) || keepRunning
        } else {
            pill.needsDisplay = true
            if visible {
                fitToContent(animated: true)
            }
        }
        if !keepRunning {
            displayClock?.invalidate()
            displayClock = nil
        }
    }

    // MARK: Window

    /// The window showing the pill this session.
    private var activePanel: NSPanel {
        model.surface == .island ? islandPanel ?? panel : panel
    }

    /// Where the pill opens this time: out of the notch when the screen it
    /// opens on has one, otherwise floating. Chosen only as it opens from
    /// hidden, so a dictation never jumps between the two.
    private func chooseSurface() {
        if islandAllowed, let screen = NSScreen.main, let notch = NotchGeometry(screen: screen) {
            model.surface = .island
            placeIsland(notch)
        } else {
            model.surface = .floating
        }
    }

    /// Put the island's window over the notch: flush with the top of the
    /// screen and centred on the notch. It does not resize after this; the
    /// island moves inside it, and the clear rest of it takes no clicks.
    private func placeIsland(_ notch: NotchGeometry) {
        islandNotch = notch
        let frame = IslandView.windowFrame(over: notch)
        island.notch = notch.rect.size
        let window: NSPanel
        if let existing = islandPanel {
            window = existing
        } else {
            window = OverlayApp.makePillPanel(island: true)
            island.autoresizingMask = [.width, .height]
            window.contentView = island
            islandPanel = window
        }
        if window.frame != frame {
            window.setFrame(frame, display: false)
        }
    }

    /// Log where the island's window really is when that changes, so a live
    /// run shows it flush with the top and centred on the notch, and so a
    /// macOS that starts moving windows over the menu bar is caught.
    private func logIslandFrame() {
        guard let window = islandPanel, let notch = islandNotch, window.frame != loggedIslandFrame else { return }
        loggedIslandFrame = window.frame
        let frame = window.frame
        let line = String(
            format: "[overlay] island window x %.1f–%.1f, top %.1f; notch x %.1f–%.1f, top %.1f, %.1f pt tall\n",
            frame.minX, frame.maxX, frame.maxY, notch.rect.minX, notch.rect.maxX, notch.rect.maxY, notch.rect.height
        )
        FileHandle.standardError.write(Data(line.utf8))
    }

    /// A display came or went, or changed resolution. An island on screen
    /// follows its notch if the screen still has one; anything else waits
    /// for the next open to choose again.
    private func screensChanged() {
        guard model.surface == .island, islandPanel?.isVisible == true,
              let screen = NSScreen.main, let notch = NotchGeometry(screen: screen) else { return }
        placeIsland(notch)
        logIslandFrame()
    }

    /// The window includes the pill's transparent margin; the pill body
    /// itself sits 14 pt below the menu bar.
    private func targetFrame(for size: NSSize) -> NSRect? {
        guard let screen = NSScreen.main else { return nil }
        let frame = screen.visibleFrame
        return NSRect(
            x: (frame.midX - size.width / 2).rounded(),
            y: frame.maxY - 14 + pill.margin - size.height,
            width: size.width,
            height: size.height
        )
    }

    /// Where the window is heading. This runs on every display tick and
    /// every audio frame; restarting the animation toward the same frame each
    /// time moved the window a fraction of a point per restart, so after the
    /// hint faded the pill stayed a few points too tall all dictation.
    private var frameTarget: NSRect?

    /// Resize around the top edge so the pill grows downward and sideways.
    private func fitToContent(animated: Bool) {
        let size = pill.windowSize
        guard let target = targetFrame(for: size), target != frameTarget else { return }
        frameTarget = target
        if panel.frame == target { return }
        if animated && panel.isVisible && panel.frame.size != size {
            NSAnimationContext.runAnimationGroup { context in
                context.duration = 0.16
                context.timingFunction = CAMediaTimingFunction(name: .easeOut)
                panel.animator().setFrame(target, display: true)
            }
        } else {
            panel.setFrame(target, display: true)
        }
    }

    private func show() {
        deferredHide?.cancel()
        deferredHide = nil
        let wasVisible = visible
        visible = true
        if !wasVisible {
            if !activePanel.isVisible {
                chooseSurface()
            }
            // Fresh, or caught mid-close: open from wherever the pill is.
            if model.surface == .floating {
                fitToContent(animated: panel.isVisible)
            }
            activePanel.alphaValue = 1
            activePanel.orderFrontRegardless()
            if !activePanel.isOnActiveSpace {
                moveToFreshPanel()
            }
            if model.surface == .island {
                logIslandFrame()
            }
            model.beginOpening(now: CACurrentMediaTime())
        } else if model.surface == .floating {
            fitToContent(animated: true)
        }
        runClockIfNeeded()
    }

    private func hide(animated: Bool) {
        deferredHide?.cancel()
        deferredHide = nil
        let hold = animated && visible ? model.closeDelay(now: CACurrentMediaTime()) : 0
        if hold > 0 {
            let close = DispatchWorkItem { [weak self] in
                self?.deferredHide = nil
                self?.hide(animated: true)
            }
            deferredHide = close
            DispatchQueue.main.asyncAfter(deadline: .now() + hold, execute: close)
            return
        }
        visible = false
        guard animated, activePanel.isVisible else {
            finishHiding()
            return
        }
        model.beginClosing(now: CACurrentMediaTime())
        runClockIfNeeded()
    }

    private func finishHiding() {
        displayClock?.invalidate()
        displayClock = nil
        panel.orderOut(nil)
        islandPanel?.orderOut(nil)
        model.reset()
        island.reset()
    }
}

// MARK: App launch
// tools/ui/render-overlay-frames.sh cuts the file here and appends its own
// main, so keep everything above this line free of top-level side effects.

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
private let delegate = OverlayApp()
app.delegate = delegate
app.run()
