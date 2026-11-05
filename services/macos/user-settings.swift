import AppKit
import UniformTypeIdentifiers

// MARK: Look shared by Settings and the menu bar panel

enum SunotoStyle {
    private static func isDark(_ appearance: NSAppearance) -> Bool {
        appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
    }
    private static func dynamic(light: NSColor, dark: NSColor) -> NSColor {
        NSColor(name: nil) { isDark($0) ? dark : light }
    }
    // The pill's ink and amber, so the windows and the pill read as one app.
    static let ink = dynamic(
        light: NSColor(srgbRed: 0.18, green: 0.37, blue: 0.84, alpha: 1),
        dark: NSColor(srgbRed: 0.56, green: 0.69, blue: 1.0, alpha: 1)
    )
    static let amber = dynamic(
        light: NSColor(srgbRed: 0.76, green: 0.46, blue: 0.0, alpha: 1),
        dark: NSColor(srgbRed: 1.0, green: 0.72, blue: 0.28, alpha: 1)
    )
    static let voice = dynamic(
        light: NSColor(srgbRed: 0.88, green: 0.22, blue: 0.17, alpha: 1),
        dark: NSColor(srgbRed: 1.0, green: 0.38, blue: 0.32, alpha: 1)
    )
    static let groupFill = dynamic(
        light: NSColor(white: 0, alpha: 0.035),
        dark: NSColor(white: 1, alpha: 0.055)
    )
    static let hover = dynamic(
        light: NSColor(white: 0, alpha: 0.07),
        dark: NSColor(white: 1, alpha: 0.10)
    )

    static func serif(_ size: CGFloat, weight: NSFont.Weight = .regular) -> NSFont {
        let base = NSFont.systemFont(ofSize: size, weight: weight)
        guard let descriptor = base.fontDescriptor.withDesign(.serif) else { return base }
        return NSFont(descriptor: descriptor, size: size) ?? base
    }

    /// What people call the shortcut: "Right Option", "Control-F8".
    static func shortcutName(_ raw: String) -> String {
        let lone = ["RightOption": "Right Option", "RightCommand": "Right Command", "RightControl": "Right Control", "RightShift": "Right Shift"]
        if let name = lone[raw] { return name }
        let parts = ["Ctrl": "Control", "Alt": "Option", "Cmd": "Command"]
        return raw.split(separator: "+").map { parts[String($0)] ?? String($0) }.joined(separator: "-")
    }

    static func label(_ text: String, size: CGFloat = 13, weight: NSFont.Weight = .regular, color: NSColor = .labelColor) -> NSTextField {
        let field = NSTextField(labelWithString: text)
        field.font = .systemFont(ofSize: size, weight: weight)
        field.textColor = color
        return field
    }

    static func wrapping(_ text: String, width: CGFloat, size: CGFloat = 11.5, color: NSColor = .secondaryLabelColor) -> NSTextField {
        let field = NSTextField(wrappingLabelWithString: text)
        field.font = .systemFont(ofSize: size)
        field.textColor = color
        field.preferredMaxLayoutWidth = width
        field.isSelectable = false
        return field
    }
}

/// A view that paints itself with colors that follow light and dark mode.
class SunotoFilledView: NSView {
    var fill: NSColor? { didSet { needsDisplay = true } }
    var edge: NSColor? { didSet { needsDisplay = true } }
    var radius: CGFloat = 0 { didSet { needsDisplay = true } }
    override var wantsUpdateLayer: Bool { true }
    override init(frame: NSRect) { super.init(frame: frame); wantsLayer = true }
    required init?(coder: NSCoder) { fatalError("not used") }
    override func updateLayer() {
        layer?.cornerRadius = radius
        layer?.backgroundColor = fill?.cgColor
        layer?.borderColor = edge?.cgColor
        layer?.borderWidth = edge == nil ? 0 : 1
    }
}

private final class SettingsFlippedStack: NSStackView {
    override var isFlipped: Bool { true }
}

/// One rounded block of rows, the way macOS lays out its own settings.
private final class SettingsGroup: SunotoFilledView {
    let stack = NSStackView()

    init(_ rows: [NSView]) {
        super.init(frame: .zero)
        fill = SunotoStyle.groupFill
        edge = .separatorColor
        radius = 10
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 0
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor),
            stack.topAnchor.constraint(equalTo: topAnchor),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
        setRows(rows)
    }
    required init?(coder: NSCoder) { fatalError("not used") }

    func setRows(_ rows: [NSView]) {
        stack.arrangedSubviews.forEach { $0.removeFromSuperview() }
        for (index, row) in rows.enumerated() {
            if index > 0 {
                let line = SunotoFilledView()
                line.fill = .separatorColor
                line.translatesAutoresizingMaskIntoConstraints = false
                line.heightAnchor.constraint(equalToConstant: 1).isActive = true
                stack.addArrangedSubview(line)
                line.leadingAnchor.constraint(equalTo: stack.leadingAnchor, constant: 12).isActive = true
                line.trailingAnchor.constraint(equalTo: stack.trailingAnchor, constant: -12).isActive = true
            }
            stack.addArrangedSubview(row)
            row.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
    }
}

/// The microphone test's picture of your voice: recent levels as a row of
/// bars, newest on the right, in the pill's ink.
final class VoiceTraceView: NSView {
    private var levels: [CGFloat] = []
    private let barWidth: CGFloat = 3
    private let gap: CGFloat = 2

    override var intrinsicContentSize: NSSize { NSSize(width: NSView.noIntrinsicMetric, height: 44) }

    func push(_ level: Double) {
        levels.append(CGFloat(max(0, min(1, level))))
        let room = Int(bounds.width / (barWidth + gap)) + 1
        if levels.count > max(room, 1) { levels.removeFirst(levels.count - max(room, 1)) }
        setAccessibilityValue("\(Int(level * 100)) percent")
        needsDisplay = true
    }

    func reset() {
        guard !levels.isEmpty else { return }
        levels = []
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        let columns = Int(bounds.width / (barWidth + gap))
        let midline = bounds.midY
        for column in 0..<max(columns, 0) {
            let fromRight = columns - 1 - column
            let index = levels.count - 1 - fromRight
            let level = index >= 0 ? levels[index] : 0
            let height = max(barWidth, level * bounds.height)
            let bar = NSRect(x: CGFloat(column) * (barWidth + gap), y: midline - height / 2, width: barWidth, height: height)
            (index >= 0 ? SunotoStyle.ink : NSColor.quaternaryLabelColor).setFill()
            NSBezierPath(roundedRect: bar, xRadius: barWidth / 2, yRadius: barWidth / 2).fill()
        }
    }
}

/// The shortcut, drawn as the key you press. Clicking it records a new one.
private final class KeyCapButton: NSButton {
    var recording = false { didSet { refresh() } }
    var keyName = "" { didSet { refresh() } }

    override var wantsUpdateLayer: Bool { true }
    override var intrinsicContentSize: NSSize {
        let size = super.intrinsicContentSize
        return NSSize(width: max(size.width + 28, 120), height: 28)
    }
    private func refresh() {
        attributedTitle = NSAttributedString(
            string: recording ? "Press keys…" : keyName,
            attributes: [
                .font: NSFont.systemFont(ofSize: 13, weight: .medium),
                .foregroundColor: recording ? SunotoStyle.ink : NSColor.labelColor,
            ]
        )
        setAccessibilityLabel(recording ? "Press the new shortcut" : "Dictation shortcut, \(keyName). Click to change.")
        invalidateIntrinsicContentSize()
        needsDisplay = true
    }
    override func updateLayer() {
        layer?.cornerRadius = 7
        layer?.borderWidth = 1
        layer?.borderColor = (recording ? SunotoStyle.ink : NSColor.tertiaryLabelColor).cgColor
        layer?.backgroundColor = (recording ? SunotoStyle.ink.withAlphaComponent(0.14) : SunotoStyle.hover).cgColor
    }
    convenience init(target: AnyObject, action: Selector) {
        self.init(frame: .zero)
        self.target = target
        self.action = action
        isBordered = false
        wantsLayer = true
    }
}

private final class SidebarRow: NSButton {
    var selectedRow = false { didSet { refresh() } }
    private let icon = NSImageView()
    private let name = NSTextField(labelWithString: "")

    convenience init(title: String, symbol: String, tag: Int, target: AnyObject, action: Selector) {
        self.init(frame: .zero)
        self.title = ""
        self.tag = tag
        self.target = target
        self.action = action
        isBordered = false
        wantsLayer = true
        setAccessibilityLabel(title)
        icon.image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil)
        icon.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 13, weight: .regular)
        name.stringValue = title
        for view in [icon, name] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
        NSLayoutConstraint.activate([
            icon.centerXAnchor.constraint(equalTo: leadingAnchor, constant: 20),
            icon.centerYAnchor.constraint(equalTo: centerYAnchor),
            name.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 36),
            name.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
        refresh()
    }
    override var wantsUpdateLayer: Bool { true }
    override func updateLayer() {
        layer?.cornerRadius = 7
        layer?.backgroundColor = selectedRow ? SunotoStyle.hover.cgColor : nil
    }
    private func refresh() {
        icon.contentTintColor = selectedRow ? SunotoStyle.ink : .secondaryLabelColor
        name.font = .systemFont(ofSize: 13, weight: selectedRow ? .medium : .regular)
        needsDisplay = true
    }
}

// MARK: Settings window

final class UserSettingsController: NSObject, NSWindowDelegate, NSTabViewDelegate, NSTextViewDelegate {
    private static let windowSize = NSSize(width: 740, height: 620)
    private static let sidebarWidth: CGFloat = 196
    private static let inset: CGFloat = 28
    private static let columnWidth = windowSize.width - sidebarWidth - inset * 2
    /// Room for a row's words beside its control.
    private static let detailWidth = columnWidth - 200
    /// Mirrors history.rs: these stay excluded whatever is saved.
    private static let alwaysExcluded: Set<String> = [
        "com.1password.1password", "com.agilebits.onepassword7", "com.bitwarden.desktop",
        "com.apple.Passwords", "com.apple.keychainaccess",
    ]
    private static let defaultShortcut = "RightOption"

    private var window: NSWindow?
    private let emit: ([String: Any]) -> Void
    private var revision = 0
    private var baseline: [String: Any] = [:]
    private var pending: String?
    private var applying = false
    private var loaded = false

    private let tabs = NSTabView()
    private var sidebarRows: [SidebarRow] = []
    private let statusDot = SunotoFilledView()
    private let stateStatus = SunotoStyle.wrapping("", width: sidebarWidth - 56)

    private let feedback = SunotoStyle.wrapping("", width: columnWidth - 190)
    private let apply = NSButton(title: "Apply", target: nil, action: nil)
    private let cancelPendingButton = NSButton(title: "Cancel Pending", target: nil, action: nil)

    private var shortcutValue = defaultShortcut
    private lazy var shortcutCap = KeyCapButton(target: self, action: #selector(recordShortcut))
    private let resetShortcut = NSButton(title: "Reset", target: nil, action: nil)
    private let shortcutDetail = SunotoStyle.wrapping("Press it in any app to start dictating.", width: detailWidth - 60)
    private let interaction = NSSegmentedControl(labels: ["Tap", "Hold"], trackingMode: .selectOne, target: nil, action: nil)
    private let interactionDetail = SunotoStyle.wrapping("", width: detailWidth)

    private let microphones = NSPopUpButton()
    private let trace = VoiceTraceView()
    private let testButton = NSButton(title: "Start Test", target: nil, action: nil)
    private let testDetail = SunotoStyle.wrapping("", width: columnWidth - 130)
    private var devices: [(String, String)] = []

    private let words = NSTextView()
    private let wordsPlaceholder = SunotoStyle.label("Names, products and terms, one per line", color: .tertiaryLabelColor)
    private let contactsDetail = SunotoStyle.wrapping("macOS asks the first time.", width: columnWidth - 90)
    private var switches: [String: NSSwitch] = [:]

    private let retention = NSPopUpButton()
    private var excludedApps: [String] = []
    private var excludedGroup: SettingsGroup?

    private let loginSwitch = NSSwitch()
    private let loginDetail = SunotoStyle.wrapping("Sunoto starts when you log in to this Mac.", width: detailWidth)
    private let loginApprove = NSButton(title: "Open Login Items", target: nil, action: nil)
    private var permissionStates: [String: NSTextField] = [:]
    private var permissionIcons: [String: NSImageView] = [:]
    private var permissionButtons: [String: NSButton] = [:]

    private var recorder: Any?
    private var controls: [String: Any] = [:]

    private var practiceWindow: NSWindow?
    private let practiceText = NSTextView()
    private let practiceStatus = SunotoStyle.wrapping("", width: 440, size: 12.5)
    private let practicePrompt = "Use your shortcut, say the sentence, then finish."
    private let practicePaused = "Sunoto is paused. Resume it from the menu bar to keep practicing."

    private var setupStep = 0
    private let setupBanner = SunotoFilledView()
    private let setupInstructions = SunotoStyle.wrapping("", width: columnWidth - 110, size: 12.5, color: .labelColor)
    private let setupNext = NSButton(title: "Next", target: nil, action: nil)

    init(emit: @escaping ([String: Any]) -> Void) {
        self.emit = emit; super.init()
        NSWorkspace.shared.notificationCenter.addObserver(self, selector: #selector(interruptSession), name: NSWorkspace.willSleepNotification, object: nil)
    }
    @objc private func interruptSession() { stopRecorder(); action("interrupt", confirmed: true) }
    func action(_ name: String, confirmed: Bool = false) { emit(["type": "user_control", "action": name, "confirmed": confirmed]) }

    func open() {
        if let window, window.isVisible { window.makeKeyAndOrderFront(nil); NSApp.activate(ignoringOtherApps: true); return }
        installEditMenu()
        if window == nil { build() }
        let id = UUID().uuidString
        pending = id
        applying = false
        refreshDraftState()
        emit(["type": "get_settings", "request_id": id])
        action("refresh")
        window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    /// An app that lives in the menu bar has no menus of its own, and without
    /// one Cmd-V and Cmd-W do nothing in its windows.
    private func installEditMenu() {
        guard NSApp.mainMenu == nil else { return }
        let bar = NSMenu()
        let appItem = NSMenuItem()
        let appMenu = NSMenu(title: "Sunoto")
        appMenu.addItem(withTitle: "Close Window", action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w")
        appItem.submenu = appMenu
        bar.addItem(appItem)
        let editItem = NSMenuItem()
        let edit = NSMenu(title: "Edit")
        edit.addItem(withTitle: "Undo", action: Selector(("undo:")), keyEquivalent: "z")
        let redo = edit.addItem(withTitle: "Redo", action: Selector(("redo:")), keyEquivalent: "z")
        redo.keyEquivalentModifierMask = [.command, .shift]
        edit.addItem(.separator())
        edit.addItem(withTitle: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        edit.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        edit.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        edit.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        editItem.submenu = edit
        bar.addItem(editItem)
        NSApp.mainMenu = bar
    }

    func receive(_ message: [String: Any]) {
        switch message["type"] as? String {
        case "controls_state":
            controls = message
            applyControls(message)
            syncPracticeWindow(message)
        case "controls_result":
            let name = message["action"] as? String ?? ""
            if message["confirmation"] as? Bool == true {
                let alert = NSAlert()
                alert.messageText = name == "quit" ? "Discard this dictation and quit?" : "Discard this dictation and pause?"
                alert.informativeText = "What you said so far won't be transcribed or pasted."
                alert.addButton(withTitle: "Cancel")
                alert.addButton(withTitle: name == "quit" ? "Discard and Quit" : "Discard and Pause")
                if alert.runModal() == .alertSecondButtonReturn { action(name, confirmed: true) } else { action("cancel_control") }
            } else if let error = message["error"] as? String {
                feedback.stringValue = error
                feedback.textColor = SunotoStyle.voice
            } else if name == "recorder_start" {
                startRecorder()
            } else if name == "practice_start" {
                // Only now is the daemon keeping results out of other apps, so
                // only now may the window make that promise.
                setFeedback("")
                practiceWindow?.makeKeyAndOrderFront(nil)
            } else if name == "settings_pending" {
                setFeedback("Your changes will apply when this dictation or microphone test ends.")
                cancelPendingButton.isHidden = false
            } else if name == "cancel_pending" {
                pending = nil
                applying = false
                cancelPendingButton.isHidden = true
                setFeedback("Pending changes cancelled.")
                refreshDraftState()
            }
        case "input_devices":
            let selected = microphones.selectedItem?.representedObject as? String ?? baseline["microphone"] as? String ?? "auto"
            let next = (message["devices"] as? [[String]] ?? []).compactMap { $0.count == 2 ? ($0[0], $0[1]) : nil }
            // This arrives every second; rebuilding would close an open list.
            guard next.map({ $0.0 + $0.1 }) != devices.map({ $0.0 + $0.1 }) else { return }
            devices = next
            updateDevices(selected)
        case "preview_level": trace.push(min(1, (message["peak"] as? Double ?? 0) * 3))
        case "practice_result":
            guard practiceWindow?.isVisible == true else { return }
            practiceText.string = message["text"] as? String ?? ""
            practiceStatus.stringValue = practiceText.string.isEmpty ? practicePrompt : "That's what Sunoto heard. Go again whenever you like."
        default:
            guard message["request_id"] as? String == pending else { return }
            pending = nil
            cancelPendingButton.isHidden = true
            let wasApplying = applying
            applying = false
            if let error = message["error"] as? String {
                feedback.stringValue = error
                feedback.textColor = SunotoStyle.voice
                refreshDraftState()
                return
            }
            revision = message["revision"] as? Int ?? 0
            baseline = message["preferences"] as? [String: Any] ?? [:]
            loaded = true
            loadBaseline()
            setFeedback(wasApplying ? "Saved. Your next dictation uses these settings." : "")
        }
    }

    private func setFeedback(_ text: String) {
        feedback.stringValue = text
        feedback.textColor = .secondaryLabelColor
    }

    private func applyControls(_ message: [String: Any]) {
        let paused = message["paused"] as? Bool == true
        let busy = message["busy"] as? Bool == true
        let preview = message["preview"] as? Bool == true
        let status = message["status"] as? String ?? (paused ? "Paused" : "Sunoto is ready")
        stateStatus.stringValue = status
        statusDot.fill = paused ? SunotoStyle.amber
            : (busy || preview) ? SunotoStyle.voice
            : status.hasSuffix("ready") ? NSColor.systemGreen : SunotoStyle.amber

        updatePermission("microphone", allowed: message["microphone"] as? Bool == true)
        updatePermission("accessibility", allowed: message["accessibility"] as? Bool == true)
        updateLogin(message["login"] as? String ?? "")
        let contacts = (message["contacts"] as? String ?? "").lowercased()
        contactsDetail.stringValue = contacts == "denied" || contacts == "restricted"
            ? "Blocked in System Settings, so contacts are skipped."
            : "macOS asks the first time."

        if !preview { trace.reset() }
        testButton.title = preview ? "Stop Test" : "Start Test"
        refreshDraftState()
    }

    private func updatePermission(_ key: String, allowed: Bool) {
        permissionStates[key]?.stringValue = allowed ? "Allowed" : "Not allowed"
        permissionStates[key]?.textColor = allowed ? .secondaryLabelColor : SunotoStyle.amber
        permissionIcons[key]?.image = NSImage(systemSymbolName: allowed ? "checkmark.circle.fill" : "exclamationmark.circle.fill", accessibilityDescription: nil)
        permissionIcons[key]?.contentTintColor = allowed ? .systemGreen : SunotoStyle.amber
        permissionButtons[key]?.title = allowed ? "Open Settings" : "Allow"
    }

    private func updateLogin(_ status: String) {
        let needsApproval = status.lowercased().contains("approval")
        let enabled = status.hasPrefix("Enabled")
        let known = enabled || status.hasPrefix("Disabled") || needsApproval
        loginSwitch.state = enabled ? .on : .off
        loginSwitch.isEnabled = known && !needsApproval
        loginApprove.isHidden = !needsApproval && !status.contains("Legacy")
        if needsApproval {
            loginDetail.stringValue = "macOS is waiting for you to approve Sunoto in Login Items."
        } else if status.contains("Legacy") {
            loginDetail.stringValue = "An older login entry may also exist. Remove it in Login Items."
        } else if !known {
            loginDetail.stringValue = "Available in the installed Sunoto app."
        } else {
            loginDetail.stringValue = "Sunoto starts when you log in to this Mac."
        }
    }

    /// The practice window says nothing leaves it, so it may only be open while
    /// the daemon is in practice, and it says so when the shortcut is paused.
    private func syncPracticeWindow(_ state: [String: Any]) {
        guard let window = practiceWindow, window.isVisible else { return }
        if state["practice"] as? Bool != true {
            window.close()
        } else if state["paused"] as? Bool == true {
            practiceStatus.stringValue = practicePaused
        } else if practiceStatus.stringValue == practicePaused {
            practiceStatus.stringValue = practicePrompt
        }
    }

    private func updateDevices(_ selected: String) {
        microphones.removeAllItems()
        microphones.addItem(withTitle: "Follow System Default"); microphones.lastItem?.representedObject = "auto"
        if !devices.isEmpty { microphones.menu?.addItem(.separator()) }
        for (uid, name) in devices { microphones.addItem(withTitle: name); microphones.lastItem?.representedObject = uid }
        if !microphones.itemArray.contains(where: { $0.representedObject as? String == selected }) {
            microphones.addItem(withTitle: "Saved microphone (not connected)"); microphones.lastItem?.representedObject = selected
        }
        microphones.select(microphones.itemArray.first { $0.representedObject as? String == selected })
    }

    // MARK: Draft

    private var interactionValue: String { interaction.selectedSegment == 1 ? "hold" : "toggle" }

    private func draft() -> [String: Any] {
        var result: [String: Any] = [
            "word_list": words.string.components(separatedBy: .newlines).filter { !$0.isEmpty },
            "history_exclude_apps": excludedApps,
            "shortcut": shortcutValue,
            "interaction": interactionValue,
            "microphone": microphones.selectedItem?.representedObject as? String ?? "auto",
            "history_retention_days": retention.selectedItem?.representedObject as? Int ?? 90,
        ]
        for (key, control) in switches { result[key] = control.state == .on }
        return result
    }

    private var isDirty: Bool { loaded && !NSDictionary(dictionary: draft()).isEqual(to: baseline) }

    /// Put every control back to what is saved.
    private func loadBaseline() {
        words.string = (baseline["word_list"] as? [String] ?? []).joined(separator: "\n")
        excludedApps = baseline["history_exclude_apps"] as? [String] ?? []
        rebuildExclusions()
        shortcutValue = baseline["shortcut"] as? String ?? Self.defaultShortcut
        interaction.selectedSegment = baseline["interaction"] as? String == "hold" ? 1 : 0
        let days = baseline["history_retention_days"] as? Int ?? 90
        if !retention.itemArray.contains(where: { $0.representedObject as? Int == days }) {
            retention.addItem(withTitle: "\(days) days"); retention.lastItem?.representedObject = days
        }
        retention.select(retention.itemArray.first { $0.representedObject as? Int == days })
        updateDevices(baseline["microphone"] as? String ?? "auto")
        for (key, control) in switches { control.state = baseline[key] as? Bool == true ? .on : .off }
        refreshDraftState()
    }

    private func refreshDraftState() {
        apply.isEnabled = pending == nil && isDirty
        shortcutCap.keyName = SunotoStyle.shortcutName(shortcutValue)
        resetShortcut.isHidden = shortcutValue == Self.defaultShortcut
        interactionDetail.stringValue = interactionValue == "hold"
            ? "Hold the shortcut while you talk. Let go to paste. Esc cancels."
            : "Tap once to start, tap again to paste. Esc cancels."
        wordsPlaceholder.isHidden = !words.string.isEmpty
        let micChanged = (microphones.selectedItem?.representedObject as? String) != (baseline["microphone"] as? String) && loaded
        let previewing = controls["preview"] as? Bool == true
        testButton.isEnabled = previewing || !micChanged
        testDetail.stringValue = micChanged
            ? "Apply your change to test this microphone."
            : "Speak and watch the bars move. Nothing is recorded or transcribed, and the test stops after 30 seconds."
        let wordList = switches["word_list_enabled"]?.state == .on
        switches["word_list_documents"]?.isEnabled = wordList
        switches["word_list_contacts"]?.isEnabled = wordList
        let history = switches["history_enabled"]?.state == .on
        retention.isEnabled = history
    }

    @objc private func draftChanged() { refreshDraftState() }
    func textDidChange(_ notification: Notification) { refreshDraftState() }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        if sender === practiceWindow { return true }
        if applying && pending != nil {
            feedback.stringValue = "Wait for saving to finish, or cancel the pending changes first."
            feedback.textColor = SunotoStyle.voice
            return false
        }
        if !isDirty { return true }
        let alert = NSAlert()
        alert.messageText = "Discard your changes?"
        alert.informativeText = "These settings haven't been applied yet."
        alert.addButton(withTitle: "Keep Editing"); alert.addButton(withTitle: "Discard")
        guard alert.runModal() == .alertSecondButtonReturn else { return false }
        loadBaseline()
        return true
    }
    func windowWillClose(_ notification: Notification) {
        if notification.object as? NSWindow === practiceWindow {
            practiceText.string = ""; action("practice_stop", confirmed: true)
        } else {
            stopRecorder(); action("settings_closed")
        }
    }
    func windowDidBecomeKey(_ notification: Notification) { action("refresh") }
    func windowDidResignKey(_ notification: Notification) { stopRecorder() }

    @objc private func save() {
        let old = baseline["history_retention_days"] as? Int ?? 90
        let new = retention.selectedItem?.representedObject as? Int ?? 90
        if new != 0 && (old == 0 || new < old) {
            let alert = NSAlert()
            alert.messageText = "Delete history older than \(new) days?"
            alert.informativeText = "Applying this removes saved dictations past that age. Reports and logs stay."
            alert.addButton(withTitle: "Cancel")
            let delete = alert.addButton(withTitle: "Apply and Delete")
            delete.hasDestructiveAction = true
            if alert.runModal() != .alertSecondButtonReturn { return }
        }
        let id = UUID().uuidString
        pending = id
        applying = true
        apply.isEnabled = false
        setFeedback("Saving…")
        emit(["type": "apply_settings", "request_id": id, "revision": revision, "preferences": draft()])
    }
    @objc private func cancel() {
        if loaded && !(applying && pending != nil) { loadBaseline(); setFeedback("") }
        window?.performClose(nil)
    }
    @objc private func cancelPending() { action("cancel_pending") }
    @objc private func toggleTest() {
        action(controls["preview"] as? Bool == true ? "preview_stop" : "preview_start")
    }
    @objc private func loginChanged() { action(loginSwitch.state == .on ? "login_on" : "login_off") }
    @objc private func openLoginItems() {
        if let url = URL(string: "x-apple.systempreferences:com.apple.LoginItems-Settings.extension") { NSWorkspace.shared.open(url) }
    }
    @objc private func microphonePermission() { emit(["type": "permission_action", "permission": "microphone"]) }
    @objc private func accessibilityPermission() { emit(["type": "permission_action", "permission": "accessibility"]) }
    @objc private func restoreShortcut() { shortcutValue = Self.defaultShortcut; refreshDraftState() }
    @objc private func clearHistory() {
        let alert = NSAlert()
        alert.messageText = "Delete all saved dictations?"
        alert.informativeText = "This can't be undone. Reports and logs stay."
        alert.addButton(withTitle: "Cancel")
        let delete = alert.addButton(withTitle: "Delete")
        delete.hasDestructiveAction = true
        if alert.runModal() == .alertSecondButtonReturn { emit(["type": "clear_history"]) }
    }
    @objc private func chooseApp() {
        let picker = NSOpenPanel()
        picker.directoryURL = URL(fileURLWithPath: "/Applications")
        picker.canChooseDirectories = false
        picker.allowedContentTypes = [.applicationBundle]
        picker.prompt = "Add"
        if picker.runModal() == .OK, let url = picker.url, let id = Bundle(url: url)?.bundleIdentifier, !excludedApps.contains(id) {
            excludedApps.append(id)
            rebuildExclusions()
            refreshDraftState()
        }
    }
    @objc private func removeExcluded(_ sender: NSButton) {
        guard let id = sender.identifier?.rawValue else { return }
        excludedApps.removeAll { $0 == id }
        rebuildExclusions()
        refreshDraftState()
    }

    // MARK: Shortcut recorder

    @objc private func recordShortcut() {
        if recorder != nil { stopRecorder(); return }
        if controls["busy"] as? Bool == true || controls["preview"] as? Bool == true {
            setFeedback("Finish the dictation or microphone test before changing the shortcut."); return
        }
        action("recorder_start")
    }
    private func startRecorder() {
        stopRecorder()
        shortcutCap.recording = true
        shortcutDetail.stringValue = "Press a right-side modifier key, or a modifier with an F-key or Space. Esc cancels."
        recorder = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .flagsChanged]) { [weak self] event in
            guard let self else { return event }
            if event.keyCode == 53 { self.stopRecorder(); return nil }
            var name: String?
            if event.type == .flagsChanged {
                name = [UInt16(61): "RightOption", 62: "RightControl", 54: "RightCommand", 60: "RightShift"][event.keyCode]
            } else {
                let keys: [UInt16: String] = [49:"Space",122:"F1",120:"F2",99:"F3",118:"F4",96:"F5",97:"F6",98:"F7",100:"F8",101:"F9",109:"F10",103:"F11",111:"F12"]
                if let key = keys[event.keyCode] {
                    var modifiers: [String] = []
                    for (flag, label) in [(NSEvent.ModifierFlags.control,"Ctrl"),(.option,"Alt"),(.shift,"Shift"),(.command,"Cmd")] {
                        if event.modifierFlags.contains(flag) { modifiers.append(label) }
                    }
                    if !modifiers.isEmpty { name = (modifiers + [key]).joined(separator: "+") }
                }
            }
            if let name { self.shortcutValue = name; self.stopRecorder() }
            return nil
        }
    }
    private func stopRecorder() {
        guard let recorder else { return }
        NSEvent.removeMonitor(recorder)
        self.recorder = nil
        shortcutCap.recording = false
        shortcutDetail.stringValue = "Press it in any app to start dictating."
        refreshDraftState()
        action("recorder_stop")
    }

    // MARK: Practice

    @objc func practice() {
        action("preview_stop")
        if controls["busy"] as? Bool == true || controls["paused"] as? Bool == true {
            setFeedback("Finish the dictation or resume Sunoto before practicing."); return
        }
        if practiceWindow == nil { buildPracticeWindow() }
        practiceText.string = ""
        let name = SunotoStyle.shortcutName(shortcutValue)
        practiceStatus.stringValue = interactionValue == "hold"
            ? "Hold \(name), say the sentence, then let go."
            : "Tap \(name), say the sentence, then tap it again."
        action("practice_start")
    }

    private func buildPracticeWindow() {
        let panel = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 520, height: 400), styleMask: [.titled, .closable, .fullSizeContentView], backing: .buffered, defer: false)
        panel.title = "Practice Dictation"
        panel.titleVisibility = .hidden
        panel.titlebarAppearsTransparent = true
        panel.isReleasedWhenClosed = false
        panel.delegate = self
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 12
        stack.translatesAutoresizingMaskIntoConstraints = false
        panel.contentView!.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: panel.contentView!.leadingAnchor, constant: 32),
            stack.trailingAnchor.constraint(equalTo: panel.contentView!.trailingAnchor, constant: -32),
            stack.topAnchor.constraint(equalTo: panel.contentView!.topAnchor, constant: 44),
            stack.bottomAnchor.constraint(equalTo: panel.contentView!.bottomAnchor, constant: -28),
        ])
        let prompt = SunotoStyle.label("Say this", size: 12, color: .secondaryLabelColor)
        let sentence = NSTextField(wrappingLabelWithString: "The next meeting is on Thursday at ten.")
        sentence.font = SunotoStyle.serif(24)
        sentence.preferredMaxLayoutWidth = 456
        sentence.isSelectable = false
        stack.addArrangedSubview(prompt)
        stack.addArrangedSubview(sentence)
        stack.setCustomSpacing(6, after: prompt)
        stack.setCustomSpacing(14, after: sentence)
        stack.addArrangedSubview(practiceStatus)
        let editor = editorGroup(practiceText, label: "Practice result", height: 120)
        stack.addArrangedSubview(editor)
        editor.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        let promise = SunotoStyle.wrapping("Your words stay in this window. Nothing is pasted or copied, so this doesn't test pasting into other apps.", width: 456)
        stack.addArrangedSubview(promise)
        panel.center()
        practiceWindow = panel
    }

    // MARK: Guided setup

    @objc func replaySetup() {
        setupStep = 0
        setupBanner.isHidden = false
        advanceSetup()
    }
    @objc private func advanceSetup() {
        let steps = [
            ("Welcome to Sunoto. Check that both permissions below say Allowed.", 0),
            ("Choose a microphone, then start a test and speak.", 1),
            ("Pick your shortcut and how it works, then apply your changes.", 1),
            ("Try it out with Practice, or skip this for now.", 0),
            ("Decide whether Sunoto opens when you log in.", 0),
            ("You're set. Everything here stays available from the menu bar.", 0),
        ]
        if setupStep >= steps.count {
            setupBanner.isHidden = true
            action("setup_complete")
            return
        }
        let item = steps[setupStep]
        setupInstructions.stringValue = "Step \(setupStep + 1) of \(steps.count). \(item.0)"
        tabs.selectTabViewItem(at: item.1)
        setupStep += 1
        setupNext.title = setupStep >= steps.count ? "Done" : "Next"
    }

    // MARK: Building blocks

    private func detail(_ text: String) -> NSTextField { SunotoStyle.wrapping(text, width: Self.detailWidth) }
    /// Beside a switch there is room to run wider.
    private func wideDetail(_ text: String) -> NSTextField { SunotoStyle.wrapping(text, width: Self.columnWidth - 90) }

    /// Words on the left, its control on the right.
    private func row(_ title: String, _ detail: NSTextField? = nil, _ trailing: NSView? = nil) -> NSView {
        let words = NSStackView(views: [SunotoStyle.label(title)])
        words.orientation = .vertical
        words.alignment = .leading
        words.spacing = 2
        if let detail { words.addArrangedSubview(detail) }
        words.setContentHuggingPriority(.defaultLow, for: .horizontal)
        words.setContentCompressionResistancePriority(.defaultHigh, for: .horizontal)
        let line = NSStackView()
        line.orientation = .horizontal
        line.alignment = .centerY
        line.spacing = 12
        line.edgeInsets = NSEdgeInsets(top: 10, left: 14, bottom: 10, right: 14)
        line.addArrangedSubview(words)
        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        line.addArrangedSubview(spacer)
        if let trailing { line.addArrangedSubview(trailing) }
        // A horizontal stack doesn't grow to fit a taller view by itself, and
        // a description that wraps would otherwise be clipped.
        words.topAnchor.constraint(greaterThanOrEqualTo: line.topAnchor, constant: 10).isActive = true
        words.bottomAnchor.constraint(lessThanOrEqualTo: line.bottomAnchor, constant: -10).isActive = true
        return line
    }

    private func toggle(_ key: String, label: String) -> NSSwitch {
        let control = NSSwitch()
        control.controlSize = .small
        control.target = self
        control.action = #selector(draftChanged)
        control.setAccessibilityLabel(label)
        switches[key] = control
        return control
    }

    private func smallButton(_ title: String, _ selector: Selector) -> NSButton {
        let button = NSButton(title: title, target: self, action: selector)
        button.controlSize = .regular
        return button
    }

    /// A text box that sits in the window rather than punching a hole in it.
    private func editorGroup(_ editor: NSTextView, label: String, height: CGFloat) -> SettingsGroup {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        editor.drawsBackground = false
        editor.isRichText = false
        editor.font = .systemFont(ofSize: 13)
        editor.textContainerInset = NSSize(width: 10, height: 10)
        editor.setAccessibilityLabel(label)
        editor.isVerticallyResizable = true
        editor.autoresizingMask = [.width]
        editor.textContainer?.widthTracksTextView = true
        editor.allowsUndo = true
        editor.isAutomaticQuoteSubstitutionEnabled = false
        editor.isAutomaticDashSubstitutionEnabled = false
        editor.isAutomaticTextReplacementEnabled = false
        editor.isAutomaticSpellingCorrectionEnabled = false
        editor.delegate = self
        scroll.documentView = editor
        scroll.heightAnchor.constraint(equalToConstant: height).isActive = true
        return SettingsGroup([scroll])
    }

    private func pane(_ name: String) -> NSStackView {
        let stack = SettingsFlippedStack()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 8
        stack.edgeInsets = NSEdgeInsets(top: 8, left: Self.inset, bottom: 24, right: Self.inset)
        stack.translatesAutoresizingMaskIntoConstraints = false
        let title = NSTextField(labelWithString: name)
        title.font = SunotoStyle.serif(26)
        stack.addArrangedSubview(title)
        stack.setCustomSpacing(18, after: title)

        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        scroll.automaticallyAdjustsContentInsets = false
        scroll.documentView = stack
        let clip = scroll.contentView
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: clip.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: clip.trailingAnchor),
            stack.topAnchor.constraint(equalTo: clip.topAnchor),
        ])
        let tab = NSTabViewItem(identifier: name)
        tab.label = name
        tab.view = scroll
        tabs.addTabViewItem(tab)
        return stack
    }

    private func section(_ stack: NSStackView, _ heading: String?, _ group: SettingsGroup, after: String? = nil) {
        if let heading {
            let label = SunotoStyle.label(heading, size: 12, weight: .semibold, color: .secondaryLabelColor)
            stack.addArrangedSubview(label)
            stack.setCustomSpacing(6, after: label)
        }
        stack.addArrangedSubview(group)
        group.widthAnchor.constraint(equalToConstant: Self.columnWidth).isActive = true
        stack.setCustomSpacing(after == nil ? 20 : 6, after: group)
        if let after {
            let note = SunotoStyle.wrapping(after, width: Self.columnWidth - 8)
            stack.addArrangedSubview(note)
            stack.setCustomSpacing(20, after: note)
        }
    }

    private func permissionRow(_ key: String, title: String, detailText: String, selector: Selector) -> NSView {
        let icon = NSImageView()
        icon.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 12, weight: .regular)
        permissionIcons[key] = icon
        let state = SunotoStyle.label("Checking…", size: 12, color: .secondaryLabelColor)
        permissionStates[key] = state
        let button = smallButton("Allow", selector)
        button.setAccessibilityLabel("\(title) permission")
        permissionButtons[key] = button
        let trailing = NSStackView(views: [icon, state, button])
        trailing.spacing = 5
        trailing.setCustomSpacing(10, after: state)
        return row(title, detail(detailText), trailing)
    }

    private func appRow(_ id: String) -> NSView {
        var name = id
        var icon = NSWorkspace.shared.icon(for: .applicationBundle)
        if let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: id) {
            name = FileManager.default.displayName(atPath: url.path).replacingOccurrences(of: ".app", with: "")
            icon = NSWorkspace.shared.icon(forFile: url.path)
        }
        icon.size = NSSize(width: 20, height: 20)
        let image = NSImageView(image: icon)
        image.widthAnchor.constraint(equalToConstant: 20).isActive = true
        image.heightAnchor.constraint(equalToConstant: 20).isActive = true
        let title = SunotoStyle.label(name)
        title.lineBreakMode = .byTruncatingMiddle
        title.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        let line = NSStackView(views: [image, title, spacer])
        line.spacing = 10
        line.edgeInsets = NSEdgeInsets(top: 7, left: 14, bottom: 7, right: 14)
        if Self.alwaysExcluded.contains(id) {
            line.addArrangedSubview(SunotoStyle.label("Always", size: 11.5, color: .tertiaryLabelColor))
        } else {
            let remove = NSButton(image: NSImage(systemSymbolName: "minus.circle", accessibilityDescription: "Remove \(name)")!, target: self, action: #selector(removeExcluded(_:)))
            remove.isBordered = false
            remove.contentTintColor = .secondaryLabelColor
            remove.identifier = NSUserInterfaceItemIdentifier(id)
            line.addArrangedSubview(remove)
        }
        return line
    }

    private func rebuildExclusions() {
        // Apps on this Mac first; the rest are password managers it may not have.
        let installed = excludedApps.filter { NSWorkspace.shared.urlForApplication(withBundleIdentifier: $0) != nil }
        let others = excludedApps.filter { !installed.contains($0) && !Self.alwaysExcluded.contains($0) }
        var rows = (installed + others).map(appRow)
        let add = smallButton("Add App…", #selector(chooseApp))
        rows.append(row("Other password managers are always left out", nil, add))
        excludedGroup?.setRows(rows)
    }

    // MARK: Window

    @objc private func sidebarClicked(_ sender: NSButton) { tabs.selectTabViewItem(at: sender.tag) }

    func tabView(_ tabView: NSTabView, didSelect tabViewItem: NSTabViewItem?) {
        let index = tabViewItem.map(tabView.indexOfTabViewItem) ?? 0
        for row in sidebarRows { row.selectedRow = row.tag == index }
    }

    private func buildSidebar(in content: NSView) -> NSView {
        let sidebar = NSVisualEffectView()
        sidebar.material = .sidebar
        sidebar.blendingMode = .behindWindow
        sidebar.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(sidebar)
        let items: [(String, String)] = [
            ("General", "gearshape"),
            ("Dictation", "mic"),
            ("Vocabulary", "character.book.closed"),
            ("Privacy", "hand.raised"),
        ]
        let list = NSStackView()
        list.orientation = .vertical
        list.spacing = 2
        list.translatesAutoresizingMaskIntoConstraints = false
        sidebar.addSubview(list)
        for (index, item) in items.enumerated() {
            let row = SidebarRow(title: item.0, symbol: item.1, tag: index, target: self, action: #selector(sidebarClicked(_:)))
            row.selectedRow = index == 0
            row.heightAnchor.constraint(equalToConstant: 30).isActive = true
            list.addArrangedSubview(row)
            row.widthAnchor.constraint(equalTo: list.widthAnchor).isActive = true
            sidebarRows.append(row)
        }
        statusDot.fill = NSColor.tertiaryLabelColor
        statusDot.radius = 4
        statusDot.translatesAutoresizingMaskIntoConstraints = false
        stateStatus.translatesAutoresizingMaskIntoConstraints = false
        sidebar.addSubview(statusDot)
        sidebar.addSubview(stateStatus)
        NSLayoutConstraint.activate([
            list.topAnchor.constraint(equalTo: sidebar.topAnchor, constant: 52),
            list.leadingAnchor.constraint(equalTo: sidebar.leadingAnchor, constant: 10),
            list.trailingAnchor.constraint(equalTo: sidebar.trailingAnchor, constant: -10),
            statusDot.widthAnchor.constraint(equalToConstant: 8),
            statusDot.heightAnchor.constraint(equalToConstant: 8),
            statusDot.leadingAnchor.constraint(equalTo: sidebar.leadingAnchor, constant: 20),
            statusDot.topAnchor.constraint(equalTo: stateStatus.topAnchor, constant: 3),
            stateStatus.leadingAnchor.constraint(equalTo: statusDot.trailingAnchor, constant: 8),
            stateStatus.trailingAnchor.constraint(lessThanOrEqualTo: sidebar.trailingAnchor, constant: -14),
            stateStatus.bottomAnchor.constraint(equalTo: sidebar.bottomAnchor, constant: -18),
        ])
        return sidebar
    }

    private func buildFooter() -> NSView {
        let footer = NSStackView()
        footer.orientation = .horizontal
        footer.alignment = .centerY
        footer.spacing = 8
        footer.edgeInsets = NSEdgeInsets(top: 12, left: Self.inset, bottom: 14, right: Self.inset)
        cancelPendingButton.target = self
        cancelPendingButton.action = #selector(cancelPending)
        cancelPendingButton.isHidden = true
        let cancelButton = NSButton(title: "Cancel", target: self, action: #selector(cancel))
        cancelButton.keyEquivalent = "\u{1b}"
        apply.target = self
        apply.action = #selector(save)
        apply.keyEquivalent = "\r"
        feedback.setContentHuggingPriority(.defaultLow, for: .horizontal)
        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        for view in [feedback, spacer, cancelPendingButton, cancelButton, apply] as [NSView] { footer.addArrangedSubview(view) }
        return footer
    }

    private func buildSetupBanner() {
        setupBanner.fill = SunotoStyle.ink.withAlphaComponent(0.14)
        setupBanner.radius = 10
        setupBanner.isHidden = true
        setupNext.target = self
        setupNext.action = #selector(advanceSetup)
        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        let line = NSStackView(views: [setupInstructions, spacer, setupNext])
        line.spacing = 12
        line.alignment = .centerY
        line.translatesAutoresizingMaskIntoConstraints = false
        setupBanner.addSubview(line)
        NSLayoutConstraint.activate([
            line.leadingAnchor.constraint(equalTo: setupBanner.leadingAnchor, constant: 14),
            line.trailingAnchor.constraint(equalTo: setupBanner.trailingAnchor, constant: -10),
            line.topAnchor.constraint(equalTo: setupBanner.topAnchor, constant: 10),
            line.bottomAnchor.constraint(equalTo: setupBanner.bottomAnchor, constant: -10),
        ])
    }

    private func buildGeneral() {
        let general = pane("General")
        loginSwitch.controlSize = .small
        loginSwitch.target = self
        loginSwitch.action = #selector(loginChanged)
        loginSwitch.setAccessibilityLabel("Open at login")
        loginApprove.target = self
        loginApprove.action = #selector(openLoginItems)
        loginApprove.isHidden = true
        let loginTrailing = NSStackView(views: [loginApprove, loginSwitch])
        loginTrailing.spacing = 10
        section(general, nil, SettingsGroup([row("Open at login", loginDetail, loginTrailing)]))
        section(general, "Permissions", SettingsGroup([
            permissionRow("microphone", title: "Microphone", detailText: "Hears you while you dictate.", selector: #selector(microphonePermission)),
            permissionRow("accessibility", title: "Accessibility", detailText: "Notices your shortcut and pastes your words.", selector: #selector(accessibilityPermission)),
        ]))
        section(general, "Getting started", SettingsGroup([
            row("Practice dictation", detail("Try it in a private window. Nothing is pasted."), smallButton("Practice…", #selector(practice))),
            row("Setup guide", detail("Walk through these settings again."), smallButton("Start Guide", #selector(replaySetup))),
        ]))
    }

    private func buildDictation() {
        let dictation = pane("Dictation")
        resetShortcut.target = self
        resetShortcut.action = #selector(restoreShortcut)
        resetShortcut.isBordered = false
        resetShortcut.contentTintColor = SunotoStyle.ink
        resetShortcut.isHidden = true
        let shortcutTrailing = NSStackView(views: [resetShortcut, shortcutCap])
        shortcutTrailing.spacing = 10
        interaction.target = self
        interaction.action = #selector(draftChanged)
        interaction.selectedSegment = 0
        interaction.setAccessibilityLabel("How the shortcut works")
        section(dictation, "Shortcut", SettingsGroup([
            row("Dictation shortcut", shortcutDetail, shortcutTrailing),
            row("How it works", interactionDetail, interaction),
        ]))

        microphones.target = self
        microphones.action = #selector(draftChanged)
        microphones.setAccessibilityLabel("Microphone")
        updateDevices("auto")
        testButton.target = self
        testButton.action = #selector(toggleTest)
        trace.setAccessibilityRole(.levelIndicator)
        trace.setAccessibilityLabel("Microphone level")
        let testLine = NSStackView(views: [testDetail, NSView(), testButton])
        testLine.alignment = .centerY
        testLine.spacing = 12
        let testStack = NSStackView(views: [trace, testLine])
        testStack.orientation = .vertical
        testStack.alignment = .leading
        testStack.spacing = 10
        testStack.edgeInsets = NSEdgeInsets(top: 12, left: 14, bottom: 12, right: 14)
        trace.widthAnchor.constraint(equalTo: testStack.widthAnchor, constant: -28).isActive = true
        testLine.widthAnchor.constraint(equalTo: testStack.widthAnchor, constant: -28).isActive = true
        section(dictation, "Microphone", SettingsGroup([row("Input", nil, microphones), testStack]))
    }

    private func buildVocabulary() {
        let vocabulary = pane("Vocabulary")
        section(vocabulary, nil, SettingsGroup([
            row("Help with names and unusual words", wideDetail("Favors your words below and ones from the window you're in, its project and your apps. Off turns all of it off."), toggle("word_list_enabled", label: "Help with names and unusual words")),
        ]))
        let editor = editorGroup(words, label: "Your words", height: 150)
        wordsPlaceholder.translatesAutoresizingMaskIntoConstraints = false
        editor.addSubview(wordsPlaceholder)
        NSLayoutConstraint.activate([
            wordsPlaceholder.leadingAnchor.constraint(equalTo: editor.leadingAnchor, constant: 15),
            wordsPlaceholder.topAnchor.constraint(equalTo: editor.topAnchor, constant: 10),
        ])
        section(vocabulary, "Your words", editor, after: "Sunoto keeps your spelling exactly as you type it.")
        section(vocabulary, "Also look in", SettingsGroup([
            row("Documents and recent files", wideDetail("macOS asks the first time."), toggle("word_list_documents", label: "Documents and recent files")),
            row("Contacts", contactsDetail, toggle("word_list_contacts", label: "Contacts")),
        ]))
    }

    private func buildPrivacy() {
        let privacy = pane("Privacy")
        retention.setAccessibilityLabel("Keep history for")
        retention.target = self
        retention.action = #selector(draftChanged)
        for days in [7, 30, 90, 0] {
            retention.addItem(withTitle: days == 0 ? "Forever" : "\(days) days")
            retention.lastItem?.representedObject = days
        }
        section(privacy, "History", SettingsGroup([
            row("Save dictation history", wideDetail("Saved only on this Mac. Turning it off keeps what's already saved."), toggle("history_enabled", label: "Save dictation history")),
            row("Keep for", nil, retention),
            row("Saved dictations", nil, smallButton("Delete All…", #selector(clearHistory))),
        ]))
        let group = SettingsGroup([])
        excludedGroup = group
        section(privacy, "Don't save text from", group, after: "Sunoto still dictates in these apps and keeps timings, but not your words.")
        rebuildExclusions()
        section(privacy, "Troubleshooting", SettingsGroup([
            row("Include dictated text in logs", wideDetail("Leave this off unless you're tracking down a problem. The apps above aren't left out of logs."), toggle("log_transcripts", label: "Include dictated text in logs")),
        ]))
    }

    private func build() {
        let panel = NSWindow(
            contentRect: NSRect(origin: .zero, size: Self.windowSize),
            styleMask: [.titled, .closable, .miniaturizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        panel.title = "Sunoto Settings"
        panel.titleVisibility = .hidden
        panel.titlebarAppearsTransparent = true
        panel.isMovableByWindowBackground = true
        panel.isReleasedWhenClosed = false
        panel.delegate = self
        let content = panel.contentView!

        let sidebar = buildSidebar(in: content)
        tabs.tabViewType = .noTabsNoBorder
        tabs.drawsBackground = false
        tabs.delegate = self
        buildGeneral()
        buildDictation()
        buildVocabulary()
        buildPrivacy()
        buildSetupBanner()

        let titlebarRoom = NSView()
        titlebarRoom.heightAnchor.constraint(equalToConstant: 34).isActive = true
        let bannerSlot = NSStackView(views: [setupBanner])
        bannerSlot.edgeInsets = NSEdgeInsets(top: 0, left: Self.inset, bottom: 10, right: Self.inset)
        setupBanner.widthAnchor.constraint(equalToConstant: Self.columnWidth).isActive = true
        let line = SunotoFilledView()
        line.fill = .separatorColor
        line.heightAnchor.constraint(equalToConstant: 1).isActive = true
        let footer = buildFooter()
        let column = NSStackView(views: [titlebarRoom, bannerSlot, tabs, line, footer])
        column.orientation = .vertical
        column.alignment = .leading
        column.spacing = 0
        column.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(column)
        for view in [titlebarRoom, bannerSlot, tabs, line, footer] as [NSView] {
            view.widthAnchor.constraint(equalTo: column.widthAnchor).isActive = true
        }
        tabs.setContentHuggingPriority(.init(1), for: .vertical)
        tabs.setContentCompressionResistancePriority(.init(1), for: .vertical)

        NSLayoutConstraint.activate([
            sidebar.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            sidebar.topAnchor.constraint(equalTo: content.topAnchor),
            sidebar.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            sidebar.widthAnchor.constraint(equalToConstant: Self.sidebarWidth),
            column.leadingAnchor.constraint(equalTo: sidebar.trailingAnchor),
            column.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            column.topAnchor.constraint(equalTo: content.topAnchor),
            column.bottomAnchor.constraint(equalTo: content.bottomAnchor),
        ])
        panel.center()
        window = panel
        refreshDraftState()
    }
}
