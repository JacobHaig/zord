// Renders the Zord app icon (1024×1024 PNG) with CoreGraphics.
// Usage: swift tools/make_icon.swift <out.png>
// Motif: the app's level-meter bars (blue = "Me", orange = "Others") on a
// dark rounded-square gradient — recognizable and on-brand.
import AppKit
import ImageIO
import UniformTypeIdentifiers

let size = 1024
let s = CGFloat(size)
let cs = CGColorSpaceCreateDeviceRGB()
guard
    let ctx = CGContext(
        data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: 0,
        space: cs, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
else { fatalError("ctx") }

// Rounded-square clip.
let rect = CGRect(x: 0, y: 0, width: s, height: s)
let bg = CGPath(roundedRect: rect, cornerWidth: s * 0.22, cornerHeight: s * 0.22, transform: nil)
ctx.addPath(bg)
ctx.clip()

// Background gradient (matches the app's dark panel palette).
let grad = CGGradient(
    colorsSpace: cs,
    colors: [
        CGColor(red: 0.06, green: 0.07, blue: 0.10, alpha: 1),
        CGColor(red: 0.11, green: 0.13, blue: 0.19, alpha: 1),
    ] as CFArray,
    locations: [0, 1])!
ctx.drawLinearGradient(grad, start: CGPoint(x: 0, y: s), end: CGPoint(x: s, y: 0), options: [])

// Three rounded meter bars.
func bar(cx: CGFloat, h: CGFloat, color: CGColor) {
    let w = s * 0.135
    let x = cx - w / 2
    let y = (s - h) / 2
    ctx.addPath(CGPath(roundedRect: CGRect(x: x, y: y, width: w, height: h),
                       cornerWidth: w / 2, cornerHeight: w / 2, transform: nil))
    ctx.setFillColor(color)
    ctx.fillPath()
}
let blue = CGColor(red: 0.30, green: 0.76, blue: 1.0, alpha: 1) // #4cc2ff
let orange = CGColor(red: 1.0, green: 0.71, blue: 0.33, alpha: 1) // #ffb454
bar(cx: s * 0.30, h: s * 0.34, color: blue)
bar(cx: s * 0.50, h: s * 0.58, color: orange)
bar(cx: s * 0.70, h: s * 0.30, color: blue)

guard let img = ctx.makeImage() else { fatalError("img") }
let out = URL(fileURLWithPath: CommandLine.arguments[1])
let dest = CGImageDestinationCreateWithURL(out as CFURL, UTType.png.identifier as CFString, 1, nil)!
CGImageDestinationAddImage(dest, img, nil)
CGImageDestinationFinalize(dest)
print("wrote \(out.path)")
