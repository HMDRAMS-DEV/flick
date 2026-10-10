import AppKit

// Flick icon: a dark tile with a photo flicking off the top of a stack.
let outDir = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "AppIcon.iconset"
try? FileManager.default.createDirectory(atPath: outDir, withIntermediateDirectories: true)

func frame(_ ctx: CGContext, center: CGPoint, size: CGSize, angle: CGFloat, fill: NSColor, border: NSColor, s: CGFloat) {
    ctx.saveGState()
    ctx.translateBy(x: center.x, y: center.y)
    ctx.rotate(by: angle)
    let r = CGRect(x: -size.width / 2, y: -size.height / 2, width: size.width, height: size.height)
    ctx.setShadow(offset: CGSize(width: 0, height: -s * 0.012), blur: s * 0.03, color: NSColor.black.withAlphaComponent(0.45).cgColor)
    ctx.setFillColor(border.cgColor)
    ctx.fill(r)
    ctx.setShadow(offset: .zero, blur: 0)
    ctx.setFillColor(fill.cgColor)
    ctx.fill(r.insetBy(dx: s * 0.022, dy: s * 0.022))
    ctx.restoreGState()
}

func render(_ size: Int) -> Data? {
    guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size,
                                     bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true,
                                     isPlanar: false, colorSpaceName: .deviceRGB,
                                     bytesPerRow: 0, bitsPerPixel: 0) else { return nil }
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let ctx = NSGraphicsContext.current!.cgContext
    let s = CGFloat(size)
    let inset = s * 0.09
    let tile = NSRect(x: inset, y: inset, width: s - inset * 2, height: s - inset * 2)
    NSColor(srgbRed: 0.08, green: 0.08, blue: 0.085, alpha: 1).setFill()
    NSBezierPath(roundedRect: tile, xRadius: s * 0.19, yRadius: s * 0.19).fill()

    let photo = CGSize(width: s * 0.44, height: s * 0.32)
    let c = CGPoint(x: s * 0.47, y: s * 0.45)
    let paper = NSColor(srgbRed: 0.93, green: 0.92, blue: 0.9, alpha: 1)
    let sky = NSColor(srgbRed: 0.24, green: 0.25, blue: 0.27, alpha: 1)
    frame(ctx, center: CGPoint(x: c.x, y: c.y - s * 0.04), size: photo, angle: 0, fill: sky, border: paper, s: s)
    frame(ctx, center: CGPoint(x: c.x + s * 0.01, y: c.y), size: photo, angle: 0.0, fill: sky, border: paper, s: s)
    // The flicked photo, mid-flight, in the accent orange.
    frame(ctx, center: CGPoint(x: c.x + s * 0.09, y: c.y + s * 0.13), size: photo, angle: 0.28,
          fill: NSColor(srgbRed: 0.91, green: 0.35, blue: 0.05, alpha: 1), border: paper, s: s)
    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])
}

for (sz, name) in [(16, "icon_16x16.png"), (32, "icon_16x16@2x.png"), (32, "icon_32x32.png"), (64, "icon_32x32@2x.png"),
                   (128, "icon_128x128.png"), (256, "icon_128x128@2x.png"), (256, "icon_256x256.png"),
                   (512, "icon_256x256@2x.png"), (512, "icon_512x512.png"), (1024, "icon_512x512@2x.png")] {
    if let d = render(sz) { try? d.write(to: URL(fileURLWithPath: outDir + "/" + name)) }
}
