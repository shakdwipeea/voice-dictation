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
}

/// Easing curves for the pill's open and close motion.
private enum Ease {
    static func clamp(_ t: CGFloat) -> CGFloat { max(0, min(1, t)) }
    static func outCubic(_ t: CGFloat) -> CGFloat { 1 - pow(1 - clamp(t), 3) }
    static func inCubic(_ t: CGFloat) -> CGFloat { pow(clamp(t), 3) }
    /// Overshoots by a few percent, then settles: a gentle spring.
    static func outBack(_ t: CGFloat) -> CGFloat {
        let t = clamp(t), c1: CGFloat = 1.2, c3 = c1 + 1
        return 1 + c3 * pow(t - 1, 3) + c1 * pow(t - 1, 2)
    }
}

/// How the pill enters and leaves. The record dot appears first and the
/// pill unrolls out of it to the right; closing rolls it back into the dot,
/// which then shrinks away. With Reduce Motion both are a plain fade.
private struct PillMotion {
    /// Scale and opacity of the round cap that holds the dot.
    var seedScale: CGFloat = 1
    var seedAlpha: CGFloat = 1
    /// 0 is just the round cap, 1 the full pill (briefly above 1 on open).
    var unroll: CGFloat = 1
    var contentAlpha: CGFloat = 1
    /// Vertical offset; negative is up.
    var lift: CGFloat = 0
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

/// The passive pill shown at the top of the screen. It never takes clicks.
private final class PillView: NSView {
    static let rowHeight: CGFloat = 40
    static let captionHeight: CGFloat = 24
    static let captureWidth: CGFloat = 232
    static let maxWidth: CGFloat = 440
    private static let ribbonLeft: CGFloat = 38
    private static let timerWidth: CGFloat = 54
    private static let sampleSpacing: CGFloat = 3
    private static let historyLength = 160
    private static let sampleInterval: CFTimeInterval = 0.02
    /// Audio frames arrive every 20 ms while the key is held; a longer gap
    /// means the user let go.
    private static let releaseGap: CFTimeInterval = 0.14
    private static let settleDuration: CFTimeInterval = 0.36
    private static let glintPeriod: CFTimeInterval = 1.25
    private static let openDuration: CFTimeInterval = 0.42
    private static let closeDuration: CFTimeInterval = 0.3
    /// The recording hint fades once you start talking, or after this long,
    /// and the pill narrows to the ribbon and timer.
    private static let hintDuration: CFTimeInterval = 3
    private static let hintFade: CFTimeInterval = 0.2
    /// Talking means this many 20 ms frames in a row above `voiceLevel`, so
    /// a click or a cough does not count.
    private static let voiceLevel: CGFloat = 0.35
    private static let voiceFrames = 4
    /// Captions under the ribbon: the hint and the rare progress word, in
    /// the serif italic of the setup panel's heading.
    private static let captionFont: NSFont = {
        let base = NSFont.systemFont(ofSize: 13, weight: .regular)
        let serif = base.fontDescriptor.withDesign(.serif)?.withSymbolicTraits(.italic)
        return serif.flatMap { NSFont(descriptor: $0, size: 13) } ?? base
    }()
    /// After the second tap the pill shows no words unless the wait is
    /// longer than this.
    private static let slowWorkDelay: CFTimeInterval = 1.5
    /// The check mark grows in, then holds this long before the pill closes.
    private static let checkPop: CFTimeInterval = 0.18
    private static let checkHold: CFTimeInterval = 0.45

    private(set) var phase: PillPhase = .idle
    private(set) var caption = ""
    private var systemMode = false
    private var levels = [CGFloat](repeating: 0, count: PillView.historyLength)
    private var elapsed: Double = 0
    private var sawSample = false
    private var lastSampleAt: CFTimeInterval = 0
    private var settle: CGFloat = 0
    private var settleStartedAt: CFTimeInterval = 0
    private var clock: CFTimeInterval = 0
    /// When the recording hint starts to fade.
    private var hintFadeAt: CFTimeInterval = .infinity
    private var voicedFrames = 0
    private var workingSince: CFTimeInterval = 0
    /// "Transcribing…" or "Polishing…", held back until the wait is long.
    private var pendingCaption = ""
    /// When the text was pasted: the dot turns into a check mark.
    private var pastedAt: CFTimeInterval?
    var reduceMotion = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
    /// Transparent room around the pill for its shadow and the spring's
    /// overshoot. The onboarding panel's still copy uses none.
    var margin: CGFloat = 16
    private enum Transition {
        case opening(since: CFTimeInterval)
        case closing(since: CFTimeInterval)
    }
    private var transition: Transition?
    /// Set when a close finishes; the app then hides the window.
    private(set) var closeFinished = false
    /// Width of the pill body, without the margin.
    private var bodyWidth: CGFloat { bounds.width - margin * 2 }

    override var isFlipped: Bool { true }
    override var isOpaque: Bool { false }

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

    /// A new capture is about to start (or the pill was just shown).
    func beginIfIdle(now: CFTimeInterval) {
        guard phase == .idle else { return }
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
        needsDisplay = true
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
        needsDisplay = true
    }

    /// The text landed at the cursor. Shown as a check mark in the dot.
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
        needsDisplay = true
    }

    /// How long a close should wait so the check mark is seen.
    func closeDelay(now: CFTimeInterval) -> CFTimeInterval {
        guard let pastedAt, phase == .working else { return 0 }
        return max(0, pastedAt + Self.checkHold - now)
    }

    func setHealth(name: String, detail: String) {
        let selfResolving = ["loading_asr", "warming_polish", "mic_starting"]
        phase = selfResolving.contains(name) ? .busy : .attention
        caption = Self.sentenceCase(Self.tidy(detail.isEmpty ? name : detail))
        systemMode = false
        needsDisplay = true
    }

    // MARK: Open and close

    /// How far the pill is shown, 0 to 1, assuming linear time. Used to turn
    /// an interrupted close into an open (and back) without a jump.
    private func presence(at now: CFTimeInterval) -> CGFloat {
        switch transition {
        case .opening(let since):
            return Ease.clamp(CGFloat((now - since) / Self.openDuration))
        case .closing(let since):
            return 1 - Ease.clamp(CGFloat((now - since) / Self.closeDuration))
        case nil:
            return closeFinished ? 0 : 1
        }
    }

    func beginOpening(now: CFTimeInterval) {
        let shown = transition == nil && !closeFinished ? 0 : presence(at: now)
        transition = .opening(since: now - CFTimeInterval(shown) * Self.openDuration)
        closeFinished = false
        clock = now
        needsDisplay = true
    }

    func beginClosing(now: CFTimeInterval) {
        let shown = presence(at: now)
        transition = .closing(since: now - CFTimeInterval(1 - shown) * Self.closeDuration)
        clock = now
        needsDisplay = true
    }

    /// Stop any open or close motion and show the pill as it is.
    func settleMotion() {
        transition = nil
        closeFinished = false
    }

    private var motion: PillMotion {
        var motion = PillMotion()
        switch transition {
        case nil:
            return motion
        case .opening(let since):
            let t = CGFloat((clock - since) / Self.openDuration)
            if reduceMotion {
                motion.seedAlpha = Ease.clamp(t)
                return motion
            }
            motion.seedScale = 0.5 + 0.5 * Ease.outCubic(t / 0.35)
            motion.seedAlpha = Ease.outCubic(t / 0.3)
            motion.lift = -8 * (1 - Ease.outCubic(t / 0.6))
            motion.unroll = Ease.outBack((t - 0.18) / 0.82)
            motion.contentAlpha = Ease.outCubic((t - 0.25) / 0.5)
        case .closing(let since):
            let t = CGFloat((clock - since) / Self.closeDuration)
            if reduceMotion {
                motion.seedAlpha = 1 - Ease.clamp(t)
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

    /// Advance animations to `now`. Returns whether to keep the clock running.
    func tick(now: CFTimeInterval) -> Bool {
        clock = now
        switch transition {
        case .opening(let since) where now - since >= Self.openDuration:
            transition = nil
        case .closing(let since) where now - since >= Self.closeDuration:
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
        needsDisplay = true
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
        needsDisplay = true
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

    // MARK: Size

    private var labelFont: NSFont { .systemFont(ofSize: 13, weight: .medium) }

    private func captionString(_ palette: PillPalette?, emphasized: Bool = false, fade: CGFloat = 1, serif: Bool = false) -> NSAttributedString {
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineBreakMode = .byTruncatingTail
        let base = palette.map { emphasized ? $0.primary : $0.secondary } ?? NSColor.labelColor
        let color = base.withAlphaComponent(base.alphaComponent * fade)
        let result = NSMutableAttributedString()
        if systemMode {
            result.append(NSAttributedString(string: "System  ", attributes: [
                .font: NSFont.systemFont(ofSize: 12, weight: .semibold),
                .foregroundColor: NSColor.controlAccentColor,
                .paragraphStyle: paragraph,
            ]))
        }
        result.append(NSAttributedString(string: caption, attributes: [
            .font: serif ? Self.captionFont : labelFont,
            .foregroundColor: color,
            .paragraphStyle: paragraph,
        ]))
        return result
    }

    /// The pill grows to fit its caption. Widths snap to 16 pt steps so a
    /// changing caption does not make the window twitch.
    var preferredSize: NSSize {
        let width: CGFloat
        let height: CGFloat
        switch phase {
        case .idle:
            width = Self.captureWidth
            height = Self.rowHeight
        case .recording, .working:
            let hasCaption = !caption.isEmpty || systemMode
            let textWidth = hasCaption ? ceil(captionString(nil, serif: true).size().width) : 0
            width = max(Self.captureWidth, Self.ribbonLeft + textWidth + 20)
            height = Self.rowHeight + (hasCaption ? Self.captionHeight : 0)
        case .busy, .attention, .error, .notice:
            let textWidth = ceil(captionString(nil).size().width)
            width = max(150, Self.ribbonLeft + textWidth + 20)
            height = Self.rowHeight
        }
        let snapped = min(Self.maxWidth, ceil(width / 16) * 16)
        return NSSize(width: snapped, height: height)
    }

    /// The window around the pill: its body plus the margin on every side.
    var windowSize: NSSize {
        let body = preferredSize
        return NSSize(width: body.width + margin * 2, height: body.height + margin * 2)
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
        context.translateBy(x: margin + cap, y: margin + motion.lift + cap)
        context.scaleBy(x: scale, y: scale)
        context.translateBy(x: -cap, y: -cap)
        drawMark(palette)
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

    private func drawMark(_ palette: PillPalette) {
        switch phase {
        case .idle, .recording, .working:
            drawCaptureMark(palette)
        case .busy:
            drawSpinner(palette)
        case .attention, .error:
            drawWarning(palette)
        case .notice:
            drawNotice(palette)
        }
    }

    /// The pill's outline for this frame of the open or close motion.
    private func bodyPath(_ motion: PillMotion) -> NSBezierPath {
        let fullWidth = bodyWidth
        let fullHeight = bounds.height - margin * 2
        let cap = min(Self.rowHeight, fullHeight)
        let width = min(fullWidth + margin - 2, cap + (fullWidth - cap) * motion.unroll)
        let height = cap + (fullHeight - cap) * Ease.clamp(motion.unroll)
        // Scale about the centre of the round cap that holds the dot.
        let scale = motion.seedScale
        let center = NSPoint(x: margin + cap / 2, y: margin + cap / 2 + motion.lift)
        let rect = NSRect(
            x: center.x + (margin - center.x) * scale,
            y: center.y + (margin + motion.lift - center.y) * scale,
            width: width * scale,
            height: height * scale
        ).insetBy(dx: 0.5, dy: 0.5)
        let radius = min(rect.height / 2, 20 * scale)
        return NSBezierPath(roundedRect: rect, xRadius: radius, yRadius: radius)
    }

    private func drawContents(_ palette: PillPalette) {
        switch phase {
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

    /// The record dot. It breathes with your voice while recording, then
    /// crossfades from red to ink as the sound settles (a hue blend would
    /// pass through purple, which reads as System mode).
    private func drawCaptureMark(_ palette: PillPalette) {
        let live = systemMode ? NSColor.controlAccentColor : palette.voice
        let settled = systemMode ? NSColor.controlAccentColor : palette.ink
        let latest = levels.last ?? 0
        let radius = 4.5 + (phase == .recording ? latest * 1.8 : 0)
        if phase == .recording, latest > 0.05 {
            live.withAlphaComponent(0.2 * min(1, latest * 1.5)).setFill()
            let halo = radius + 4
            NSBezierPath(ovalIn: NSRect(x: markX - halo, y: center - halo, width: halo * 2, height: halo * 2)).fill()
        }
        if let pastedAt {
            drawCheck(palette, tint: settled, progress: CGFloat((clock - pastedAt) / Self.checkPop))
            return
        }
        let dot = NSRect(x: markX - radius, y: center - radius, width: radius * 2, height: radius * 2)
        live.withAlphaComponent(1 - settle).setFill()
        NSBezierPath(ovalIn: dot).fill()
        settled.withAlphaComponent(settle).setFill()
        NSBezierPath(ovalIn: dot).fill()
    }

    /// The settled dot grows into a disc with a check cut out of it.
    private func drawCheck(_ palette: PillPalette, tint: NSColor, progress: CGFloat) {
        let grow = reduceMotion ? 1 : Ease.outBack(progress)
        let radius = 4.5 + 3 * grow
        tint.setFill()
        NSBezierPath(ovalIn: NSRect(x: markX - radius, y: center - radius, width: radius * 2, height: radius * 2)).fill()
        let check = NSBezierPath()
        check.move(to: NSPoint(x: markX - 3.3, y: center + 0.2))
        check.line(to: NSPoint(x: markX - 0.9, y: center + 2.6))
        check.line(to: NSPoint(x: markX + 3.4, y: center - 2.4))
        check.lineWidth = 1.8
        check.lineCapStyle = .round
        check.lineJoinStyle = .round
        palette.body.withAlphaComponent(Ease.clamp(progress * 1.5)).setStroke()
        check.stroke()
    }

    private func drawCapture(_ palette: PillPalette) {
        let live = systemMode ? NSColor.controlAccentColor : palette.voice
        let settled = systemMode ? NSColor.controlAccentColor : palette.ink
        let left = Self.ribbonLeft
        let right = bodyWidth - Self.timerWidth
        drawRibbon(from: left, to: right, tint: live, settled: settled, palette: palette)
        drawTimer(palette, right: bodyWidth - 16)

        if !caption.isEmpty || systemMode {
            let rect = NSRect(x: left, y: Self.rowHeight - 6, width: bodyWidth - left - 18, height: 18)
            // The recording hint fades out before the pill narrows; a
            // System label stays.
            let fade = phase == .recording && !caption.isEmpty
                ? 1 - Ease.clamp(CGFloat((clock - hintFadeAt) / Self.hintFade)) : 1
            captionString(palette, fade: fade, serif: true)
                .draw(with: rect, options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
        }
    }

    private func drawRibbon(from left: CGFloat, to right: CGFloat, tint: NSColor, settled: NSColor, palette: PillPalette) {
        guard right > left else { return }
        let spacing = Self.sampleSpacing
        let maxAmplitude: CGFloat = 10
        let drift = phase == .recording && sawSample
            ? CGFloat(min(1, (clock - lastSampleAt) / Self.sampleInterval)) : 0
        var points: [NSPoint] = []
        var step = 0
        while step < levels.count {
            let x = right - (CGFloat(step) + drift) * spacing
            if x < left { break }
            let taper = min(1, (x - left) / 30) * min(1, (right - x) / 8 + 0.35)
            let amplitude = levels[levels.count - 1 - step] * maxAmplitude * (1 - settle) * max(0, taper)
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
        if phase == .working, settle >= 1, pastedAt == nil, !reduceMotion {
            let progress = CGFloat(clock.truncatingRemainder(dividingBy: Self.glintPeriod) / Self.glintPeriod)
            let eased = progress < 0.5 ? 2 * progress * progress : 1 - pow(-2 * progress + 2, 2) / 2
            let glintWidth: CGFloat = 64
            let x = left - glintWidth + (right - left + glintWidth) * eased
            NSGraphicsContext.saveGraphicsState()
            NSRect(x: left, y: center - 1.5, width: right - left, height: 3).clip()
            NSGradient(colors: [settled.withAlphaComponent(0), settled, settled.withAlphaComponent(0)])?
                .draw(in: NSRect(x: x, y: center - 1.5, width: glintWidth, height: 3), angle: 0)
            NSGraphicsContext.restoreGraphicsState()
        }
    }

    private func drawTimer(_ palette: PillPalette, right: CGFloat) {
        let seconds = Int(elapsed.rounded(.down))
        let text = String(format: "%d:%02d", seconds / 60, seconds % 60)
        let paragraph = NSMutableParagraphStyle()
        paragraph.alignment = .right
        let attributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.monospacedDigitSystemFont(ofSize: 11.5, weight: .medium),
            .foregroundColor: phase == .recording ? palette.secondary : palette.secondary.withAlphaComponent(0.4),
            .paragraphStyle: paragraph,
        ]
        let height: CGFloat = 15
        (text as NSString).draw(
            in: NSRect(x: right - 40, y: center - height / 2, width: 40, height: height),
            withAttributes: attributes
        )
    }

    private func drawLabel(_ palette: PillPalette) {
        let rect = NSRect(x: Self.ribbonLeft, y: center - 8.5, width: bodyWidth - Self.ribbonLeft - 18, height: 18)
        captionString(palette, emphasized: true)
            .draw(with: rect, options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
    }

    private func drawSpinner(_ palette: PillPalette) {
        let radius: CGFloat = 6.5
        let ring = NSBezierPath(ovalIn: NSRect(x: markX - radius, y: center - radius, width: radius * 2, height: radius * 2))
        ring.lineWidth = 2
        palette.faint.setStroke()
        ring.stroke()
        guard !reduceMotion else { return }
        let start = CGFloat(clock.truncatingRemainder(dividingBy: 1.1) / 1.1) * 360
        let arc = NSBezierPath()
        arc.appendArc(withCenter: NSPoint(x: markX, y: center), radius: radius, startAngle: start, endAngle: start + 100)
        arc.lineWidth = 2
        arc.lineCapStyle = .round
        palette.secondary.setStroke()
        arc.stroke()
    }

    private func drawWarning(_ palette: PillPalette) {
        let radius: CGFloat = 7.5
        palette.amber.setFill()
        NSBezierPath(ovalIn: NSRect(x: markX - radius, y: center - radius, width: radius * 2, height: radius * 2)).fill()
        palette.onAmber.setFill()
        NSBezierPath(roundedRect: NSRect(x: markX - 0.9, y: center - 4.2, width: 1.8, height: 5), xRadius: 0.9, yRadius: 0.9).fill()
        NSBezierPath(ovalIn: NSRect(x: markX - 1.05, y: center + 2.1, width: 2.1, height: 2.1)).fill()
    }

    private func drawNotice(_ palette: PillPalette) {
        let radius: CGFloat = 4.5
        let tint = systemMode ? NSColor.controlAccentColor : palette.ink
        tint.setFill()
        NSBezierPath(ovalIn: NSRect(x: markX - radius, y: center - radius, width: radius * 2, height: radius * 2)).fill()
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
        hero.showDemo()
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

private final class OverlayApp: NSObject, NSApplicationDelegate {
    private let panel: NSPanel
    private let pill = PillView(frame: NSRect(x: 0, y: 0, width: PillView.captureWidth, height: PillView.rowHeight))
    private var palette: SystemPaletteController!
    private var onboarding: OnboardingController!
    private var visible = false
    private var displayClock: Timer?
    /// A close waiting for the check mark to be seen.
    private var deferredHide: DispatchWorkItem?
    private let stdoutLock = NSLock()

    override init() {
        panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: PillView.captureWidth, height: PillView.rowHeight),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
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
        pill.autoresizingMask = [.width, .height]
        panel.contentView = pill
        panel.isOpaque = false
        panel.backgroundColor = .clear
        // The pill draws its own shadow so it follows the open and close
        // motion; a window shadow would outline the whole margin.
        panel.hasShadow = false
        panel.ignoresMouseEvents = true
        panel.isReleasedWhenClosed = false
        panel.level = .statusBar
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
        panel.hidesOnDeactivate = false
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        hide(animated: false)
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
            pill.beginIfIdle(now: now)
            show()
        case "hide":
            hide(animated: true)
        case "pasted":
            pill.markPasted(now: now)
        case "recording":
            let peak = number(message["peak"])
            let rms = number(message["rms"])
            let level = min(1.0, max(0.0, CGFloat(peak * 8.0), CGFloat(rms * 35.0)))
            pill.pushLevel(level, elapsed: number(message["elapsed_s"]), now: now)
        case "status":
            pill.setStatus((message["text"] as? String) ?? "", error: message["error"] as? Bool ?? false, now: now)
        case "state":
            let name = (message["name"] as? String) ?? ""
            if name == "ready" {
                if pill.phase == .busy || pill.phase == .attention {
                    hide(animated: true)
                }
            } else {
                pill.setHealth(name: name, detail: (message["detail"] as? String) ?? "")
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
        case "segment", "clear":
            break
        case "shutdown":
            NSApp.terminate(nil)
        default:
            break
        }
        if visible {
            fitToContent(animated: true)
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
        guard panel.isVisible, pill.isAnimating, displayClock == nil else { return }
        let clock = Timer(timeInterval: 1.0 / 60.0, repeats: true) { [weak self] _ in
            self?.advance()
        }
        RunLoop.main.add(clock, forMode: .common)
        displayClock = clock
    }

    private func advance() {
        let keepRunning = pill.tick(now: CACurrentMediaTime())
        if pill.closeFinished {
            finishHiding()
            return
        }
        if visible {
            fitToContent(animated: true)
        }
        if !keepRunning {
            displayClock?.invalidate()
            displayClock = nil
        }
    }

    // MARK: Window

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

    /// Resize around the top edge so the pill grows downward and sideways.
    private func fitToContent(animated: Bool) {
        let size = pill.windowSize
        guard let target = targetFrame(for: size) else { return }
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
            // Fresh, or caught mid-close: open from wherever the pill is.
            fitToContent(animated: panel.isVisible)
            panel.alphaValue = 1
            panel.orderFrontRegardless()
            pill.beginOpening(now: CACurrentMediaTime())
        } else {
            fitToContent(animated: true)
        }
        runClockIfNeeded()
    }

    private func hide(animated: Bool) {
        deferredHide?.cancel()
        deferredHide = nil
        let hold = animated && visible ? pill.closeDelay(now: CACurrentMediaTime()) : 0
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
        guard animated, panel.isVisible else {
            finishHiding()
            return
        }
        pill.beginClosing(now: CACurrentMediaTime())
        runClockIfNeeded()
    }

    private func finishHiding() {
        displayClock?.invalidate()
        displayClock = nil
        panel.orderOut(nil)
        pill.reset()
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
private let delegate = OverlayApp()
app.delegate = delegate
app.run()
