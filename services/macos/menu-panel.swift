import AppKit

// The panel under the menu bar icon: what Sunoto is doing, your recent
// dictations, and the few things you reach for. It never activates Sunoto, so
// the app you were writing in keeps focus and dictation still lands there.

final class SunotoMenuPanelWindow: NSPanel {
    var onCancel: (() -> Void)?
    override var canBecomeKey: Bool { true }
    override func cancelOperation(_ sender: Any?) { onCancel?() }
}

/// A clickable row that lights up under the pointer.
private final class MenuHoverRow: NSButton {
    /// Set only by the controller, which knows where the pointer really is.
    var hovering = false {
        didSet {
            guard hovering != oldValue else { return }
            needsDisplay = true
            onHover?(hovering)
        }
    }
    var onHover: ((Bool) -> Void)?

    override var wantsUpdateLayer: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override func updateLayer() {
        layer?.cornerRadius = 7
        layer?.backgroundColor = hovering && isEnabled ? SunotoStyle.hover.cgColor : nil
    }

    /// An invisible button holding a laid-out view of its own.
    static func holding(_ content: NSView, target: AnyObject, action: Selector, label: String, height: CGFloat? = nil) -> MenuHoverRow {
        let row = MenuHoverRow(frame: .zero)
        row.title = ""
        row.isBordered = false
        row.wantsLayer = true
        row.target = target
        row.action = action
        row.setAccessibilityLabel(label)
        content.translatesAutoresizingMaskIntoConstraints = false
        row.addSubview(content)
        NSLayoutConstraint.activate([
            content.leadingAnchor.constraint(equalTo: row.leadingAnchor, constant: 8),
            content.trailingAnchor.constraint(equalTo: row.trailingAnchor, constant: -8),
            content.topAnchor.constraint(equalTo: row.topAnchor, constant: 4),
            content.bottomAnchor.constraint(equalTo: row.bottomAnchor, constant: -4),
        ])
        if let height { row.heightAnchor.constraint(equalToConstant: height).isActive = true }
        return row
    }
}

private final class MenuFlippedStack: NSStackView {
    override var isFlipped: Bool { true }
}

final class MenuPanelController: NSObject {
    struct Actions {
        let settings: () -> Void
        let togglePause: () -> Void
        let quit: () -> Void
        let checkUpdates: () -> Void
        let setAutomaticUpdates: (Bool) -> Void
        let flag: (String) -> Void
        let clearHistory: () -> Void
    }

    private static let width: CGFloat = 340
    /// The same height as the rows below it.
    private static let recentRowHeight: CGFloat = 30
    private static let recentVisible = 5
    /// Holds the time, or the row's buttons under the pointer, so the words
    /// beside it never move.
    private static let recentSlotWidth: CGFloat = 46

    private let actions: Actions
    private var state: [String: Any] = [:]
    private var paused = false
    private let panel: SunotoMenuPanelWindow
    private let root = NSVisualEffectView()
    private let stack = NSStackView()
    private weak var anchor: NSStatusBarButton?
    private var monitors: [Any] = []
    private var activationObserver: NSObjectProtocol?
    private var recentTexts: [Int: String] = [:]
    private var recentIDs: [Int: String] = [:]
    private var copiedRow: Int?
    private weak var recentScroll: NSScrollView?
    private var hoverRows: [MenuHoverRow] = []
    private weak var hoveredRow: MenuHoverRow?
    private let relative: RelativeDateTimeFormatter = {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .short
        return formatter
    }()

    var isOpen: Bool { panel.isVisible }
    var window: NSWindow { panel }

    init(actions: Actions) {
        self.actions = actions
        panel = SunotoMenuPanelWindow(
            contentRect: NSRect(x: 0, y: 0, width: Self.width, height: 200),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        super.init()
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.isFloatingPanel = true
        // After isFloatingPanel, which sets a level of its own.
        panel.level = .popUpMenu
        panel.hidesOnDeactivate = false
        panel.becomesKeyOnlyIfNeeded = false
        panel.isReleasedWhenClosed = false
        panel.animationBehavior = .utilityWindow
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .transient]
        panel.setAccessibilityLabel("Sunoto")
        panel.onCancel = { [weak self] in self?.close() }

        root.material = .menu
        root.blendingMode = .behindWindow
        root.state = .active
        root.maskImage = Self.roundedMask(radius: 12)
        panel.contentView = root

        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 0
        stack.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            stack.topAnchor.constraint(equalTo: root.topAnchor),
            stack.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            root.widthAnchor.constraint(equalToConstant: Self.width),
        ])
        // One watcher for the whole panel. Rows watching for themselves stay lit
        // when a row scrolls out from under the pointer: they never hear it left.
        root.addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .mouseMoved, .activeAlways, .inVisibleRect], owner: self))
    }

    private static func roundedMask(radius: CGFloat) -> NSImage {
        let side = radius * 2 + 1
        let image = NSImage(size: NSSize(width: side, height: side), flipped: false) { rect in
            NSColor.black.setFill()
            NSBezierPath(roundedRect: rect, xRadius: radius, yRadius: radius).fill()
            return true
        }
        image.capInsets = NSEdgeInsets(top: radius, left: radius, bottom: radius, right: radius)
        image.resizingMode = .stretch
        return image
    }

    // MARK: Showing

    func update(state: [String: Any]) {
        self.state = state
        if isOpen { rebuild() }
    }

    func update(paused: Bool) {
        guard paused != self.paused else { return }
        self.paused = paused
        if isOpen { rebuild() }
    }

    func toggle(from button: NSStatusBarButton) {
        if isOpen { close(); return }
        anchor = button
        copiedRow = nil
        rebuild(keepingScroll: false)
        place(under: button)
        button.highlight(true)
        panel.makeKeyAndOrderFront(nil)
        refreshHover()
        // Whole rows fit exactly, so show that more of them are down there.
        recentScroll?.flashScrollers()
        startWatching()
    }

    func close() {
        guard isOpen else { return }
        stopWatching()
        setHovered(nil)
        panel.orderOut(nil)
        anchor?.highlight(false)
    }

    /// Lay out without showing, for tools/ui/settings-evidence.swift.
    func renderForEvidence(state: [String: Any], paused: Bool) {
        self.state = state
        self.paused = paused
        rebuild()
        panel.setFrameOrigin(NSPoint(x: 200, y: 200))
    }

    private func place(under button: NSStatusBarButton) {
        guard let source = button.window else { return }
        let screen = source.screen ?? NSScreen.main
        let size = panel.frame.size
        var x = source.frame.midX - size.width / 2
        if let visible = screen?.visibleFrame {
            x = min(max(x, visible.minX + 8), visible.maxX - size.width - 8)
        }
        panel.setFrameOrigin(NSPoint(x: x, y: source.frame.minY - size.height - 6))
    }

    private func startWatching() {
        stopWatching()
        if let global = NSEvent.addGlobalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown, .otherMouseDown], handler: { [weak self] _ in
            self?.close()
        }) { monitors.append(global) }
        if let local = NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown, .otherMouseDown], handler: { [weak self] event in
            guard let self else { return event }
            // A click on the icon itself toggles through the button's action.
            if event.window !== self.panel && event.window !== self.anchor?.window { self.close() }
            return event
        }) { monitors.append(local) }
        activationObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didActivateApplicationNotification, object: nil, queue: .main
        ) { [weak self] _ in self?.close() }
    }

    private func stopWatching() {
        monitors.forEach(NSEvent.removeMonitor)
        monitors = []
        if let activationObserver {
            NSWorkspace.shared.notificationCenter.removeObserver(activationObserver)
            self.activationObserver = nil
        }
    }

    // MARK: Pointer

    private static func hoverRows(in view: NSView) -> [MenuHoverRow] {
        view.subviews.flatMap { child -> [MenuHoverRow] in
            if let row = child as? MenuHoverRow { return [row] }
            return hoverRows(in: child)
        }
    }

    private func setHovered(_ row: MenuHoverRow?) {
        guard row !== hoveredRow else { return }
        hoveredRow?.hovering = false
        hoveredRow = row
        row?.hovering = true
    }

    /// Lights the one row under the pointer, whatever moved it there.
    private func refreshHover() {
        guard isOpen else { return }
        let pointer = panel.mouseLocationOutsideOfEventStream
        setHovered(hoverRows.first { $0.isEnabled && $0.visibleRect.contains($0.convert(pointer, from: nil)) })
    }

    @objc private func pointerMayHaveMoved() { refreshHover() }
    @objc(mouseEntered:) func pointerEntered(_ event: NSEvent) { refreshHover() }
    @objc(mouseMoved:) func pointerMoved(_ event: NSEvent) { refreshHover() }
    @objc(mouseExited:) func pointerExited(_ event: NSEvent) { setHovered(nil) }

    // MARK: Content

    private func rebuild(keepingScroll: Bool = true) {
        let scrolledTo = keepingScroll ? recentScroll?.contentView.bounds.origin : nil
        setHovered(nil)
        NotificationCenter.default.removeObserver(self, name: NSView.boundsDidChangeNotification, object: nil)
        stack.arrangedSubviews.forEach { $0.removeFromSuperview() }
        recentTexts = [:]
        recentIDs = [:]
        add(header(), insets: NSEdgeInsets(top: 14, left: 16, bottom: 12, right: 16))
        addLine()
        add(recentSection(), insets: NSEdgeInsets(top: 10, left: 8, bottom: 8, right: 8))
        addLine()
        add(actionsSection(), insets: NSEdgeInsets(top: 6, left: 8, bottom: 6, right: 8))
        addLine()
        add(footer(), insets: NSEdgeInsets(top: 4, left: 16, bottom: 6, right: 14))
        root.layoutSubtreeIfNeeded()
        let top = panel.frame.maxY
        let size = NSSize(width: Self.width, height: root.fittingSize.height)
        panel.setContentSize(size)
        panel.setFrameOrigin(NSPoint(x: panel.frame.minX, y: top - size.height))
        panel.invalidateShadow()
        if let scrolledTo, let recentScroll {
            recentScroll.contentView.scroll(to: scrolledTo)
            recentScroll.reflectScrolledClipView(recentScroll.contentView)
        }
        if let clip = recentScroll?.contentView {
            clip.postsBoundsChangedNotifications = true
            NotificationCenter.default.addObserver(self, selector: #selector(pointerMayHaveMoved), name: NSView.boundsDidChangeNotification, object: clip)
        }
        hoverRows = Self.hoverRows(in: root)
        refreshHover()
    }

    private func add(_ view: NSView, insets: NSEdgeInsets) {
        view.translatesAutoresizingMaskIntoConstraints = false
        let holder = NSView()
        holder.translatesAutoresizingMaskIntoConstraints = false
        holder.addSubview(view)
        NSLayoutConstraint.activate([
            view.leadingAnchor.constraint(equalTo: holder.leadingAnchor, constant: insets.left),
            view.trailingAnchor.constraint(equalTo: holder.trailingAnchor, constant: -insets.right),
            view.topAnchor.constraint(equalTo: holder.topAnchor, constant: insets.top),
            view.bottomAnchor.constraint(equalTo: holder.bottomAnchor, constant: -insets.bottom),
        ])
        stack.addArrangedSubview(holder)
        holder.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
    }

    private func addLine() {
        let line = SunotoFilledView()
        line.fill = .separatorColor
        line.translatesAutoresizingMaskIntoConstraints = false
        line.heightAnchor.constraint(equalToConstant: 1).isActive = true
        stack.addArrangedSubview(line)
        line.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
    }

    private func header() -> NSView {
        let ready = state["ready"] as? Bool ?? false
        let dot = SunotoFilledView()
        dot.fill = paused ? SunotoStyle.amber : (ready ? NSColor.systemGreen : SunotoStyle.amber)
        dot.radius = 4
        dot.translatesAutoresizingMaskIntoConstraints = false
        dot.widthAnchor.constraint(equalToConstant: 8).isActive = true
        dot.heightAnchor.constraint(equalToConstant: 8).isActive = true

        let title = NSTextField(wrappingLabelWithString: paused ? "Paused" : (state["status"] as? String ?? "Sunoto is starting…"))
        title.font = SunotoStyle.serif(17)
        title.isSelectable = false
        title.preferredMaxLayoutWidth = Self.width - 56

        let words = NSStackView(views: [title])
        words.orientation = .vertical
        words.alignment = .leading
        words.spacing = 3
        words.translatesAutoresizingMaskIntoConstraints = false
        if let hint = paused ? "The shortcut is off until you resume." : (state["hint"] as? String) {
            words.addArrangedSubview(SunotoStyle.wrapping(hint, width: Self.width - 56, size: 12))
        }

        let holder = NSView()
        holder.addSubview(words)
        holder.addSubview(dot)
        NSLayoutConstraint.activate([
            words.leadingAnchor.constraint(equalTo: holder.leadingAnchor),
            words.topAnchor.constraint(equalTo: holder.topAnchor),
            words.bottomAnchor.constraint(equalTo: holder.bottomAnchor),
            dot.trailingAnchor.constraint(equalTo: holder.trailingAnchor),
            dot.centerYAnchor.constraint(equalTo: title.centerYAnchor),
            words.trailingAnchor.constraint(lessThanOrEqualTo: dot.leadingAnchor, constant: -8),
        ])
        return holder
    }

    private func recentSection() -> NSView {
        let section = NSStackView()
        section.orientation = .vertical
        section.alignment = .leading
        section.spacing = 4

        let heading = SunotoStyle.label("Recent", size: 12, weight: .semibold, color: .secondaryLabelColor)
        let top = NSStackView()
        top.edgeInsets = NSEdgeInsets(top: 0, left: 8, bottom: 2, right: 6)
        top.addArrangedSubview(heading)
        section.addArrangedSubview(top)
        top.widthAnchor.constraint(equalTo: section.widthAnchor).isActive = true

        let rows = (state["recent"] as? [[String: Any]]) ?? []
        let historyOn = state["history_enabled"] as? Bool ?? false
        guard historyOn, !rows.isEmpty else {
            let text = historyOn
                ? "Your dictations show up here so you can copy them again."
                : "History is off, so dictations aren't saved. You can turn it on in Settings."
            let note = SunotoStyle.wrapping(text, width: Self.width - 48, size: 12)
            let holder = NSStackView(views: [note])
            holder.edgeInsets = NSEdgeInsets(top: 0, left: 8, bottom: 4, right: 8)
            section.addArrangedSubview(holder)
            return section
        }

        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        let clear = NSButton(title: "Clear…", target: self, action: #selector(clearHistory))
        clear.isBordered = false
        clear.controlSize = .small
        clear.contentTintColor = .secondaryLabelColor
        clear.setAccessibilityLabel("Clear history")
        top.addArrangedSubview(spacer)
        top.addArrangedSubview(clear)

        let list = MenuFlippedStack()
        list.orientation = .vertical
        list.alignment = .leading
        list.spacing = 0
        list.translatesAutoresizingMaskIntoConstraints = false
        let now = Date()
        for (index, row) in rows.enumerated() {
            guard let text = row["text"] as? String else { continue }
            let view = recentRow(index: index, text: text, row: row, now: now)
            list.addArrangedSubview(view)
            view.widthAnchor.constraint(equalTo: list.widthAnchor).isActive = true
        }
        let scroll = NSScrollView()
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.scrollerStyle = .overlay
        scroll.documentView = list
        scroll.translatesAutoresizingMaskIntoConstraints = false
        let clip = scroll.contentView
        NSLayoutConstraint.activate([
            list.leadingAnchor.constraint(equalTo: clip.leadingAnchor),
            list.trailingAnchor.constraint(equalTo: clip.trailingAnchor),
            list.topAnchor.constraint(equalTo: clip.topAnchor),
            scroll.heightAnchor.constraint(equalToConstant: CGFloat(min(rows.count, Self.recentVisible)) * Self.recentRowHeight),
        ])
        section.addArrangedSubview(scroll)
        scroll.widthAnchor.constraint(equalTo: section.widthAnchor).isActive = true
        // In the margin, clear of the times.
        scroll.verticalScroller?.controlSize = .small
        scroll.scrollerInsets = NSEdgeInsets(top: 0, left: 0, bottom: 0, right: -3)
        recentScroll = rows.count > Self.recentVisible ? scroll : nil
        return section
    }

    /// "now", "12m", "3h", "2d", then "Oct 12".
    static func shortAge(from date: Date, to now: Date) -> String {
        let seconds = Int(now.timeIntervalSince(date))
        if seconds < 60 { return "now" }
        if seconds < 3600 { return "\(seconds / 60)m" }
        if seconds < 86400 { return "\(seconds / 3600)h" }
        if seconds < 7 * 86400 { return "\(seconds / 86400)d" }
        let formatter = DateFormatter()
        formatter.setLocalizedDateFormatFromTemplate("MMMd")
        return formatter.string(from: date)
    }

    private func recentRow(index: Int, text: String, row: [String: Any], now: Date) -> NSView {
        recentTexts[index] = text
        if let id = row["id"] as? String, !id.isEmpty { recentIDs[index] = id }

        let line = text.split(whereSeparator: { $0.isWhitespace }).joined(separator: " ")
        let words = SunotoStyle.label(line)
        words.lineBreakMode = .byTruncatingTail
        words.maximumNumberOfLines = 1
        words.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        var age = ""
        var spokenAge = ""
        if let ms = (row["at_ms"] as? NSNumber)?.doubleValue {
            let date = Date(timeIntervalSince1970: ms / 1000)
            age = Self.shortAge(from: date, to: now)
            spokenAge = ", " + relative.localizedString(for: date, relativeTo: now)
        }
        let copied = copiedRow == index
        let meta = NSTextField(labelWithString: copied ? "Copied" : age)
        meta.font = .monospacedDigitSystemFont(ofSize: 11.5, weight: .regular)
        meta.textColor = copied ? SunotoStyle.ink : .secondaryLabelColor
        meta.alignment = .right

        let flag = NSButton(image: NSImage(systemSymbolName: "exclamationmark.bubble", accessibilityDescription: "This was wrong")!, target: self, action: #selector(flagRow(_:)))
        flag.isBordered = false
        flag.contentTintColor = .secondaryLabelColor
        flag.toolTip = "This was wrong…"
        flag.tag = index
        let copyIcon = NSImageView(image: NSImage(systemSymbolName: "doc.on.doc", accessibilityDescription: nil)!)
        copyIcon.contentTintColor = .secondaryLabelColor
        let icons = NSStackView(views: [flag, copyIcon])
        icons.spacing = 8
        icons.isHidden = true
        flag.isHidden = recentIDs[index] == nil

        let content = NSView()
        for view in [words, meta, icons] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(view)
        }
        NSLayoutConstraint.activate([
            words.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            words.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -(Self.recentSlotWidth + 10)),
            words.centerYAnchor.constraint(equalTo: content.centerYAnchor),
            meta.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            meta.widthAnchor.constraint(equalToConstant: Self.recentSlotWidth),
            meta.firstBaselineAnchor.constraint(equalTo: words.firstBaselineAnchor),
            icons.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            icons.centerYAnchor.constraint(equalTo: content.centerYAnchor),
        ])

        let button = MenuHoverRow.holding(content, target: self, action: #selector(copyRow(_:)), label: "Copy: \(line)\(spokenAge)", height: Self.recentRowHeight)
        button.tag = index
        button.identifier = NSUserInterfaceItemIdentifier("recent-row")
        button.toolTip = line
        // "Copied" stays until the pointer leaves the row; then the row is normal.
        var showingCopied = copied
        button.onHover = { [weak self] on in
            if !on && showingCopied {
                showingCopied = false
                self?.copiedRow = nil
                meta.stringValue = age
                meta.textColor = .secondaryLabelColor
            }
            let showIcons = on && !showingCopied
            icons.isHidden = !showIcons
            meta.isHidden = showIcons
        }
        return button
    }

    private func actionsSection() -> NSView {
        let list = NSStackView()
        list.orientation = .vertical
        list.alignment = .leading
        list.spacing = 0

        func rowView(_ title: String, symbol: String, enabled: Bool = true, _ selector: Selector) -> NSView {
            let icon = NSImageView(image: NSImage(systemSymbolName: symbol, accessibilityDescription: nil)!)
            icon.contentTintColor = .secondaryLabelColor
            icon.translatesAutoresizingMaskIntoConstraints = false
            icon.widthAnchor.constraint(equalToConstant: 18).isActive = true
            let label = SunotoStyle.label(title, color: enabled ? .labelColor : .tertiaryLabelColor)
            let spacer = NSView()
            spacer.setContentHuggingPriority(.init(1), for: .horizontal)
            let line = NSStackView(views: [icon, label, spacer])
            line.spacing = 8
            let button = MenuHoverRow.holding(line, target: self, action: selector, label: title, height: 30)
            button.isEnabled = enabled
            return button
        }
        func addRow(_ view: NSView) {
            list.addArrangedSubview(view)
            view.widthAnchor.constraint(equalTo: list.widthAnchor).isActive = true
        }

        addRow(rowView(paused ? "Resume Sunoto" : "Pause Sunoto", symbol: paused ? "play" : "pause", #selector(togglePause)))
        addRow(rowView("Settings…", symbol: "gearshape", #selector(openSettings)))
        let checking = state["checking"] as? Bool ?? false
        addRow(rowView(checking ? "Checking for Updates…" : "Check for Updates…", symbol: "arrow.down.circle", enabled: !checking, #selector(checkUpdates)))
        if let note = state["update_note"] as? String {
            let label = SunotoStyle.wrapping(note, width: Self.width - 60, size: 11.5)
            let holder = NSStackView(views: [label])
            holder.edgeInsets = NSEdgeInsets(top: 0, left: 34, bottom: 6, right: 8)
            addRow(holder)
        }

        // A copy that cannot update itself keeps the setting in view, off and
        // greyed; the reason is its tooltip.
        let blocked = state["update_blocked"] as? String
        let automatic = NSSwitch()
        automatic.controlSize = .mini
        automatic.state = (state["auto_update"] as? Bool ?? false) && blocked == nil ? .on : .off
        automatic.isEnabled = blocked == nil && !state.isEmpty
        automatic.target = self
        automatic.action = #selector(automaticChanged(_:))
        automatic.toolTip = blocked
        automatic.setAccessibilityLabel("Install updates automatically")
        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        let autoLabel = SunotoStyle.label("Install updates automatically", color: blocked == nil ? .labelColor : .tertiaryLabelColor)
        autoLabel.toolTip = blocked
        let line = NSStackView(views: [autoLabel, spacer, automatic])
        line.edgeInsets = NSEdgeInsets(top: 6, left: 34, bottom: 6, right: 8)
        addRow(line)
        return list
    }

    private func footer() -> NSView {
        let version = (state["version"] as? String).map { "Sunoto \($0)" } ?? "Sunoto"
        let label = SunotoStyle.label(version, size: 11.5, color: .tertiaryLabelColor)
        let quit = NSButton(title: "Quit", target: self, action: #selector(quitSunoto))
        quit.isBordered = false
        quit.contentTintColor = .secondaryLabelColor
        quit.setAccessibilityLabel("Quit Sunoto")
        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        return NSStackView(views: [label, spacer, quit])
    }

    // MARK: Actions

    @objc private func openSettings() { close(); actions.settings() }
    @objc private func togglePause() { close(); actions.togglePause() }
    @objc private func quitSunoto() { close(); actions.quit() }
    @objc private func checkUpdates() { state["checking"] = true; rebuild(); actions.checkUpdates() }
    @objc private func clearHistory() { close(); actions.clearHistory() }
    @objc private func automaticChanged(_ sender: NSSwitch) {
        state["auto_update"] = sender.state == .on
        actions.setAutomaticUpdates(sender.state == .on)
    }
    @objc private func copyRow(_ sender: NSButton) {
        guard let text = recentTexts[sender.tag] else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
        copiedRow = sender.tag
        rebuild()
    }
    @objc private func flagRow(_ sender: NSButton) {
        guard let id = recentIDs[sender.tag] else { return }
        close()
        actions.flag(id)
    }
}
