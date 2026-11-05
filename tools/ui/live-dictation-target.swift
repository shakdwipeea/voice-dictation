// Disposable dictation target. Keeps the user's clipboard in memory and puts it back on exit.
import AppKit
setvbuf(stdout, nil, _IOLBF, 0)
let board = NSPasteboard.general
let saved: [[(NSPasteboard.PasteboardType, Data)]] = (board.pasteboardItems ?? []).map { item in
    item.types.compactMap { type in item.data(forType: type).map { (type, $0) } }
}
func restoreClipboard() {
    board.clearContents()
    board.writeObjects(saved.map { pairs in
        let item = NSPasteboardItem()
        for (type, data) in pairs { item.setData(data, forType: type) }
        return item
    })
}
final class Delegate: NSObject, NSTextViewDelegate {
    func textDidChange(_ notification: Notification) { print("changed", (notification.object as! NSTextView).string.count) }
}
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let mainMenu = NSMenu()
let editItem = NSMenuItem()
mainMenu.addItem(editItem)
let editMenu = NSMenu(title: "Edit")
editMenu.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
editItem.submenu = editMenu
app.mainMenu = mainMenu
let window = NSWindow(contentRect: NSRect(x: 80, y: 80, width: 420, height: 180), styleMask: [.titled], backing: .buffered, defer: false)
window.title = "Sunoto test target (safe to ignore)"
let text = NSTextView(frame: window.contentView!.bounds)
let delegate = Delegate()
text.delegate = delegate
text.isRichText = false
window.contentView!.addSubview(text)
func focus() {
    app.activate(ignoringOtherApps: true)
    window.makeKeyAndOrderFront(nil)
    window.makeFirstResponder(text)
}
DispatchQueue.global().async {
    while let line = readLine() {
        DispatchQueue.main.async {
            switch line {
            case "focus":
                focus()
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { print("active", NSApp.isActive && window.isKeyWindow) }
            case "active": print("active", NSApp.isActive && window.isKeyWindow)
            case "text": print("text", text.string.replacingOccurrences(of: "\n", with: " "))
            case "quit": restoreClipboard(); print("bye"); exit(0)
            default: break
            }
        }
    }
    DispatchQueue.main.async { restoreClipboard(); exit(0) }
}
app.finishLaunching()
focus()
print("ready")
app.run()
