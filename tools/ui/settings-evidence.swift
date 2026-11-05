// Compile after services/macos/user-settings.swift; exercises production AppKit controls.
extension UserSettingsController {
    func evidence(_ directory: String) throws {
        func snapshot(_ name: String, _ target: NSWindow? = nil) throws {
            let target = target ?? window!
            target.makeKeyAndOrderFront(nil)
            target.contentView!.layoutSubtreeIfNeeded()
            target.display()
            RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.15))
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
            process.arguments = ["-x", "-o", "-l", String(target.windowNumber), URL(fileURLWithPath: directory).appendingPathComponent(name + ".png").path]
            try process.run()
            process.waitUntilExit()
            precondition(process.terminationStatus == 0, "Window screenshot must succeed")

        }
        open()
        RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.3))
        print("UI foreground verified:", NSWorkspace.shared.frontmostApplication?.processIdentifier == ProcessInfo.processInfo.processIdentifier)
        let first = window
        open()
        precondition(window === first, "Settings must reuse its window")
        for (index, name) in ["general", "dictation", "vocabulary", "privacy"].enumerated() {
            tabs.selectTabViewItem(at: index)
            try snapshot(name)
        }
        let original = words.string
        words.string = "Sunoto\nCaseSensitive"
        precondition((baseline["word_list"] as? [String] ?? []).joined(separator: "\n") == original, "Draft edits must not alter saved preferences")
        save()
        precondition(pending == nil && words.string.contains("CaseSensitive"), "Apply must acknowledge saved preferences")
        let invalidID = UUID().uuidString
        pending = invalidID
        receive(["type":"settings_state", "request_id":invalidID, "error":"Settings changed. Reopen Settings and try again."])
        precondition(feedback.stringValue.contains("Settings changed"), "Stale draft error must be visible")
        try snapshot("validation-feedback")
        receive(["type":"controls_state", "paused":true, "busy":false, "preview":false, "practice":false, "microphone":true, "accessibility":true, "login":"Enabled"])
        tabs.selectTabViewItem(at: 0)
        try snapshot("paused")
        receive(["type":"controls_state", "paused":false, "busy":false, "preview":true, "practice":false, "microphone":true, "accessibility":true, "login":"Enabled"])
        for step in 0..<90 {
            let wave = abs(sin(Double(step) / 3.1)) * abs(sin(Double(step) / 11)) * 0.3
            receive(["type":"preview_level", "peak":wave, "rms":wave / 2])
        }
        tabs.selectTabViewItem(at: 1)
        try snapshot("microphone-test")
        receive(["type":"controls_state", "paused":false, "busy":false, "preview":false, "practice":false, "microphone":false, "accessibility":false, "login":"RequiresApproval"])
        tabs.selectTabViewItem(at: 0)
        try snapshot("permissions-denied")
        replaySetup()
        try snapshot("guided-setup")
        func practiceState(paused: Bool, practice: Bool) -> [String:Any] {
            ["type":"controls_state", "paused":paused, "busy":false, "preview":false, "practice":practice, "microphone":true, "accessibility":true, "login":"Enabled"]
        }
        practice()
        precondition(practiceWindow?.isVisible != true, "Practice window must wait for the daemon to accept")
        receive(["type":"controls_result", "action":"practice_start", "error":"This action is unavailable during a session, microphone test, or pause."])
        precondition(feedback.stringValue.contains("unavailable"), "A refused practice must say why")
        precondition(practiceWindow?.isVisible != true, "A refused practice must not show the window")
        practice()
        receive(["type":"controls_result", "action":"practice_start"])
        precondition(practiceWindow?.isVisible == true, "Accepted practice must show the window")
        receive(practiceState(paused: false, practice: true))
        precondition(practiceWindow?.isVisible == true, "Practice must stay open while the daemon is practicing")
        receive(practiceState(paused: true, practice: true))
        precondition(practiceWindow?.isVisible == true && practiceStatus.stringValue == practicePaused, "Paused practice must say it is paused")
        receive(practiceState(paused: false, practice: true))
        precondition(practiceStatus.stringValue == practicePrompt, "Resume must restore the practice prompt")
        receive(["type":"practice_result", "text":"The next meeting is on Thursday at ten."])
        try snapshot("practice", practiceWindow)
        receive(practiceState(paused: false, practice: false))
        precondition(practiceWindow?.isVisible != true, "Practice window must close when the daemon left practice")
        try renderMenuPanel(snapshot)
        print("PASS: reusable Settings window; isolated draft; Apply acknowledgement; stale feedback; all four panes rendered; denied permissions; guided setup; practice view waits for the daemon, reports pause, and closes when practice ends.")
    }
}

extension UserSettingsController {
    func renderMenuPanel(_ snapshot: (String, NSWindow?) throws -> Void) throws {
        var automaticSent: Bool?
        func automaticSwitch(_ view: NSView) -> NSSwitch? {
            if let found = view as? NSSwitch { return found }
            for child in view.subviews { if let found = automaticSwitch(child) { return found } }
            return nil
        }
        let actions = MenuPanelController.Actions(settings: {}, togglePause: {}, quit: {}, checkUpdates: {}, setAutomaticUpdates: { automaticSent = $0 }, flag: { _ in }, clearHistory: {})
        let panel = MenuPanelController(actions: actions)
        let now = Date().timeIntervalSince1970 * 1000
        let recent: [[String: Any]] = [
            ["id":"a", "at_ms":now - 120_000, "text":"The next meeting is on Thursday at ten, and I'll send the agenda before lunch."],
            ["id":"b", "at_ms":now - 1_500_000, "text":"Can you take a look at the pull request when you get a chance?"],
            ["id":"c", "at_ms":now - 7_400_000, "text":"Let's move the launch to next week."],
            ["id":"d", "at_ms":now - 90_000_000, "text":"Thanks for the quick turnaround on this."],
            ["id":"e", "at_ms":now - 300_000_000, "text":"Please add Priya to the invite."],
            ["id":"f", "at_ms":now - 900_000_000, "text":"I'll follow up with the design team tomorrow morning."],
        ]
        var state: [String: Any] = ["status":"Sunoto is ready", "ready":true, "hint":"Tap Right Option to dictate", "version":"0.1.15", "auto_update":true, "checking":false, "history_enabled":true, "recent":recent]
        panel.renderForEvidence(state: state, paused: false)
        try snapshot("menu-panel", panel.window)
        func firstRow(_ view: NSView) -> MenuHoverRow? {
            if let row = view as? MenuHoverRow, row.identifier?.rawValue == "recent-row" { return row }
            for child in view.subviews { if let row = firstRow(child) { return row } }
            return nil
        }
        firstRow(panel.window.contentView!)?.hovering = true
        panel.window.contentView!.layoutSubtreeIfNeeded()
        try snapshot("menu-panel-hover", panel.window)
        let automatic = automaticSwitch(panel.window.contentView!)
        precondition(automatic?.isEnabled == true && automatic?.state == .on, "An updatable copy must offer a live switch")
        automatic?.performClick(nil)
        precondition(automaticSent == false, "Turning the switch off must reach the daemon")
        automatic?.performClick(nil)
        precondition(automaticSent == true, "Turning the switch on must reach the daemon")
        panel.renderForEvidence(state: state, paused: true)
        try snapshot("menu-panel-paused", panel.window)
        state["history_enabled"] = false
        state["recent"] = [] as [[String: Any]]
        state["update_blocked"] = "This copy of Sunoto was built from source; update it with git pull and install-macos.sh."
        panel.renderForEvidence(state: state, paused: false)
        let dead = automaticSwitch(panel.window.contentView!)
        precondition(dead?.isEnabled == false && dead?.state == .off, "A copy that cannot update itself must show the switch off and greyed")
        try snapshot("menu-panel-history-off", panel.window)
        panel.window.orderOut(nil)
    }
}

let app = NSApplication.shared
if CommandLine.arguments.contains("--light") { app.appearance = NSAppearance(named: .aqua) }
if CommandLine.arguments.contains("--dark") { app.appearance = NSAppearance(named: .darkAqua) }
app.setActivationPolicy(.regular)
app.finishLaunching()
app.activate(ignoringOtherApps: true)
var preferences: [String:Any] = [
    "shortcut":"RightOption", "interaction":"toggle", "microphone":"auto",
    "history_enabled":true, "history_retention_days":90,
    "history_exclude_apps":["com.1password.1password","com.apple.keychainaccess"],
    "word_list":["Sunoto"], "word_list_enabled":true, "word_list_documents":false,
    "word_list_contacts":false, "log_transcripts":false
]
var revision = 0
var controller: UserSettingsController!
controller = UserSettingsController { message in
    if message["type"] as? String == "get_settings" || message["type"] as? String == "apply_settings" {
        if let draft = message["preferences"] as? [String:Any] { preferences = draft; revision += 1 }
        controller.receive(["type":"settings_state", "request_id":message["request_id"]!, "revision":revision, "preferences":preferences])
    }
}
controller.receive(["type":"controls_state", "paused":false, "busy":false, "preview":false, "practice":false, "microphone":true, "accessibility":true, "login":"Enabled"])
controller.receive(["type":"input_devices", "devices":[["uid:builtin","MacBook Pro Microphone"],["uid:usb","USB Microphone"]]])
do {
    try controller.evidence(CommandLine.arguments[1])
} catch {
    fputs("Evidence rendering failed: \(error)\n", stderr)
    exit(1)
}
