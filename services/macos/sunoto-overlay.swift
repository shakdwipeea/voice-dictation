import AppKit
import Foundation

// The passive pill is used while recording and resolving. System mode uses a
// purple accent so it cannot be confused with ordinary red dictation capture.
private final class PillView: NSView {
    var level: CGFloat = 0.0 {
        didSet { needsDisplay = true }
    }
    var status: String = "" {
        didSet { needsDisplay = true }
    }
    /// True while the pill is showing daemon health (loading, warming,
    /// blocked) rather than a capture. Draws a neutral dot so a user never
    /// mistakes "not ready" for "recording".
    var attention: Bool = false {
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
        let systemMode = status.lowercased().hasPrefix("system")
        let dotColor: NSColor
        if attention {
            dotColor = NSColor(calibratedWhite: 0.55, alpha: 1.0)
        } else if systemMode {
            dotColor = NSColor.controlAccentColor
        } else {
            dotColor = NSColor(calibratedRed: 0.94, green: 0.26, blue: 0.26, alpha: 1.0)
        }
        dotColor.setFill()
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
        let modeLabel = NSTextField(labelWithString: "SYSTEM")
        modeLabel.font = .systemFont(ofSize: 9.5, weight: .semibold)
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
        let help = NSTextField(labelWithString: "Click or ↵ to open   ·   esc to cancel")
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
        panel.alphaValue = 0
        NSApp.activate(ignoringOtherApps: true)
        panel.makeKeyAndOrderFront(nil)
        panel.makeFirstResponder(table)
        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0.12
            panel.animator().alphaValue = 1
        }
    }

    func dismiss(sessionID: UInt64) {
        guard self.sessionID == sessionID else { return }
        self.sessionID = nil
        suggestions = []
        panel.orderOut(nil)
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
        panel.orderOut(nil)
        onSelect(sessionID, suggestionID)
    }

    @objc private func cancelSelection() {
        guard let sessionID else { return }
        self.sessionID = nil
        suggestions = []
        panel.orderOut(nil)
        onCancel(sessionID)
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

    private let rows: [Row] = [
        Row(
            key: "listen",
            permission: "input_monitoring",
            title: "Input Monitoring",
            subtitle: "Lets Sunoto hear your shortcut in other apps",
            anchor: "Privacy_ListenEvent"
        ),
        Row(
            key: "accessibility",
            permission: "accessibility",
            title: "Accessibility",
            subtitle: "Lets Sunoto paste text where you are typing",
            anchor: "Privacy_Accessibility"
        ),
        Row(
            key: "microphone",
            permission: "microphone",
            title: "Microphone",
            subtitle: "Lets Sunoto listen while you hold the shortcut",
            anchor: "Privacy_Microphone"
        ),
    ]

    private let panel: NSPanel
    private var dots: [String: NSImageView] = [:]
    private var buttons: [String: NSButton] = [:]
    private var cards: [String: NSBox] = [:]
    private var attempted: Set<String> = []
    private let doneButton: NSButton
    private let onAction: (String) -> Void
    private let onDone: () -> Void
    private(set) var visible = false
    private var dismissed = false

    private static func roundedFont(ofSize size: CGFloat, weight: NSFont.Weight) -> NSFont {
        let base = NSFont.systemFont(ofSize: size, weight: weight)
        guard let descriptor = base.fontDescriptor.withDesign(.rounded) else { return base }
        return NSFont(descriptor: descriptor, size: size) ?? base
    }

    init(onAction: @escaping (String) -> Void, onDone: @escaping () -> Void) {
        self.onAction = onAction
        self.onDone = onDone
        doneButton = NSButton(title: "Done", target: nil, action: nil)
        panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 480, height: 326),
            styleMask: [.titled, .closable, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        super.init()

        panel.title = "Sunoto"
        panel.delegate = self
        panel.isFloatingPanel = true
        panel.level = .floating
        panel.isReleasedWhenClosed = false
        panel.hidesOnDeactivate = false
        panel.isMovableByWindowBackground = true
        panel.animationBehavior = .utilityWindow

        let heading = NSTextField(labelWithString: "Set up permissions")
        heading.font = .systemFont(ofSize: 20, weight: .semibold)
        heading.textColor = .labelColor
        let subtitle = NSTextField(labelWithString: "Grant access one step at a time. Your choices stay on this Mac.")
        subtitle.font = .systemFont(ofSize: 12.5, weight: .regular)
        subtitle.textColor = .secondaryLabelColor
        subtitle.maximumNumberOfLines = 2
        subtitle.lineBreakMode = .byWordWrapping

        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 10
        stack.edgeInsets = NSEdgeInsets(top: 22, left: 22, bottom: 18, right: 22)
        stack.addArrangedSubview(heading)
        stack.addArrangedSubview(subtitle)
        stack.setCustomSpacing(16, after: subtitle)

        for row in rows {
            let dot = NSImageView()
            dot.image = NSImage(systemSymbolName: "circle", accessibilityDescription: "status")?
                .withSymbolConfiguration(.init(pointSize: 13, weight: .semibold))
            dot.contentTintColor = .tertiaryLabelColor
            dot.widthAnchor.constraint(equalToConstant: 16).isActive = true
            dots[row.key] = dot

            let title = NSTextField(labelWithString: row.title)
            title.font = Self.roundedFont(ofSize: 13.5, weight: .semibold)
            title.textColor = .labelColor
            let sub = NSTextField(labelWithString: row.subtitle)
            sub.font = .systemFont(ofSize: 11.5, weight: .regular)
            sub.textColor = .secondaryLabelColor
            sub.lineBreakMode = .byTruncatingTail
            let labels = NSStackView(views: [title, sub])
            labels.orientation = .vertical
            labels.alignment = .leading
            labels.spacing = 2
            labels.widthAnchor.constraint(equalToConstant: 260).isActive = true

            let button = NSButton(title: "Request Access", target: self, action: #selector(openPermission(_:)))
            button.bezelStyle = .rounded
            button.font = .systemFont(ofSize: 12, weight: .medium)
            button.identifier = NSUserInterfaceItemIdentifier(row.key)
            buttons[row.key] = button

            let rowView = NSStackView(views: [dot, labels, button])
            rowView.orientation = .horizontal
            rowView.alignment = .centerY
            rowView.spacing = 9
            rowView.distribution = .gravityAreas
            labels.setContentHuggingPriority(.defaultLow, for: .horizontal)
            button.widthAnchor.constraint(equalToConstant: 106).isActive = true

            let card = NSBox()
            card.boxType = .custom
            card.borderColor = .separatorColor
            card.borderWidth = 1
            card.cornerRadius = 9
            card.fillColor = .controlBackgroundColor
            card.contentViewMargins = NSSize(width: 10, height: 8)
            card.contentView = rowView
            card.widthAnchor.constraint(equalToConstant: 436).isActive = true
            card.heightAnchor.constraint(equalToConstant: 56).isActive = true
            cards[row.key] = card
            stack.addArrangedSubview(card)
        }

        let footer = NSTextField(labelWithString: "Done becomes available when all three permissions are allowed.")
        footer.font = .systemFont(ofSize: 11.5)
        footer.textColor = .tertiaryLabelColor
        footer.setContentHuggingPriority(.defaultLow, for: .horizontal)

        doneButton.target = self
        doneButton.action = #selector(finishOnboarding)
        doneButton.bezelStyle = .rounded
        doneButton.font = .systemFont(ofSize: 12.5, weight: .semibold)
        doneButton.keyEquivalent = "\r"
        doneButton.isEnabled = false
        doneButton.widthAnchor.constraint(equalToConstant: 88).isActive = true

        let footerRow = NSStackView(views: [footer, doneButton])
        footerRow.orientation = .horizontal
        footerRow.alignment = .centerY
        footerRow.distribution = .gravityAreas
        footerRow.widthAnchor.constraint(equalToConstant: 436).isActive = true
        stack.setCustomSpacing(14, after: stack.arrangedSubviews.last!)
        stack.addArrangedSubview(footerRow)

        stack.translatesAutoresizingMaskIntoConstraints = false
        let content = NSView()
        content.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: content.topAnchor),
            stack.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            stack.bottomAnchor.constraint(equalTo: content.bottomAnchor),
        ])
        panel.contentView = content
    }

    func update(listen: Bool, accessibility: Bool, microphone: Bool) {
        let states: [String: Bool] = [
            "listen": listen,
            "accessibility": accessibility,
            "microphone": microphone,
        ]
        let orderedStates = [listen, accessibility, microphone]
        let firstMissing = orderedStates.firstIndex(of: false)
        for (index, row) in rows.enumerated() {
            let granted = states[row.key] ?? false
            let actionable = firstMissing == index
            dots[row.key]?.image = NSImage(
                systemSymbolName: granted ? "checkmark.circle.fill" : (actionable ? "arrow.right.circle.fill" : "circle"),
                accessibilityDescription: granted ? "allowed" : (actionable ? "next step" : "waiting")
            )?.withSymbolConfiguration(.init(pointSize: 13, weight: .semibold))
            dots[row.key]?.contentTintColor = granted ? .systemGreen : (actionable ? .controlAccentColor : .tertiaryLabelColor)
            cards[row.key]?.borderColor = actionable
                ? .controlAccentColor.withAlphaComponent(0.65)
                : .separatorColor
            let button = buttons[row.key]
            button?.title = granted ? "Allowed" : (attempted.contains(row.key) ? "Open Settings" : "Request Access")
            button?.isEnabled = actionable
        }
        doneButton.isEnabled = listen && accessibility && microphone
        show()
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
    private let pill = PillView(frame: NSRect(x: 0, y: 0, width: 214, height: 34))
    private var palette: SystemPaletteController!
    private var onboarding: OnboardingController!
    private var visible = false
    private let stdoutLock = NSLock()

    override init() {
        panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 214, height: 34),
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
        switch message["type"] as? String {
        case "show":
            show()
        case "hide":
            hide()
        case "recording":
            let peak = number(message["peak"])
            let rms = number(message["rms"])
            pill.attention = false
            pill.level = min(1.0, max(0.0, CGFloat(peak * 8.0), CGFloat(rms * 35.0)))
        case "status":
            pill.attention = false
            pill.status = (message["text"] as? String) ?? ""
            resizeForStatus()
        case "state":
            let name = (message["name"] as? String) ?? ""
            if name == "ready" {
                pill.attention = false
                pill.status = ""
                hide()
            } else {
                pill.attention = true
                pill.level = 0
                pill.status = (message["detail"] as? String) ?? name
                resizeForStatus()
                show()
            }
        case "onboarding":
            onboarding.update(
                listen: message["listen"] as? Bool ?? false,
                accessibility: message["accessibility"] as? Bool ?? false,
                microphone: message["microphone"] as? Bool ?? false
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
            hide()
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
