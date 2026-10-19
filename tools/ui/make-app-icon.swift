// Draw Sunoto's app icon (a white voice wave on a warm-to-violet rounded
// square, on the macOS icon grid) at every size macOS asks for, and pack it
// into an .icns with iconutil. Also writes the README's copy,
// docs/assets/sunoto-icon.png. Run from the repository root:
//
//   swift tools/ui/make-app-icon.swift [packaging/macos/AppIcon.icns]
//
// The menu bar glyph that goes with it is drawn by MenuBarController.glyph()
// in services/macos/sunoto-overlay.swift.
import AppKit

let output = URL(fileURLWithPath: CommandLine.arguments.count > 1
    ? CommandLine.arguments[1]
    : "packaging/macos/AppIcon.icns")

func rgb(_ hex: UInt32) -> NSColor {
    NSColor(srgbRed: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255,
            blue: CGFloat(hex & 0xff) / 255, alpha: 1)
}

/// The icon on a `size` x `size` canvas, in units of a 1024 grid.
func drawIcon(size: CGFloat) {
    let k = size / 1024
    // The macOS grid: an 824 rounded square with a soft shadow below it.
    let body = NSRect(x: 100 * k, y: 100 * k, width: 824 * k, height: 824 * k)
    let shape = NSBezierPath(roundedRect: body, xRadius: 185 * k, yRadius: 185 * k)
    NSGraphicsContext.saveGraphicsState()
    let shadow = NSShadow()
    shadow.shadowColor = NSColor(white: 0, alpha: 0.28)
    shadow.shadowBlurRadius = 24 * k
    shadow.shadowOffset = NSSize(width: 0, height: -10 * k)
    shadow.set()
    NSColor.black.setFill()
    shape.fill()
    NSGraphicsContext.restoreGraphicsState()

    NSGraphicsContext.saveGraphicsState()
    shape.addClip()
    NSGradient(starting: rgb(0xFF6A55), ending: rgb(0x6E56F5))!.draw(in: body, angle: -60)
    // Seven rounded bars, tallest just right of centre, like a voice.
    let heights: [CGFloat] = [150, 290, 430, 330, 470, 260, 150]
    let width = 58 * k, gap = 30 * k
    let total = CGFloat(heights.count) * width + CGFloat(heights.count - 1) * gap
    var x = 512 * k - total / 2
    NSColor.white.setFill()
    for height in heights {
        let bar = NSRect(x: x, y: 512 * k - height * k / 2, width: width, height: height * k)
        NSBezierPath(roundedRect: bar, xRadius: width / 2, yRadius: width / 2).fill()
        x += width + gap
    }
    NSGraphicsContext.restoreGraphicsState()
}

func png(pixels: Int) -> Data {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels,
                               bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                               colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    drawIcon(size: CGFloat(pixels))
    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

let iconset = FileManager.default.temporaryDirectory
    .appendingPathComponent("sunoto-\(ProcessInfo.processInfo.processIdentifier).iconset")
try? FileManager.default.removeItem(at: iconset)
try FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
for points in [16, 32, 128, 256, 512] {
    try png(pixels: points).write(to: iconset.appendingPathComponent("icon_\(points)x\(points).png"))
    try png(pixels: points * 2).write(to: iconset.appendingPathComponent("icon_\(points)x\(points)@2x.png"))
}
let iconutil = Process()
iconutil.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
iconutil.arguments = ["-c", "icns", iconset.path, "-o", output.path]
try iconutil.run()
iconutil.waitUntilExit()
try? FileManager.default.removeItem(at: iconset)
guard iconutil.terminationStatus == 0 else {
    FileHandle.standardError.write(Data("iconutil failed\n".utf8))
    exit(1)
}
print("wrote \(output.path)")

// GitHub cannot show an .icns; the README shows this at 128 points.
let readmeIcon = URL(fileURLWithPath: "docs/assets/sunoto-icon.png")
try png(pixels: 256).write(to: readmeIcon)
print("wrote \(readmeIcon.path)")
