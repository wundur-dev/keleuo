// The installer window's background: a night sky over rolling meadows, and an arrow from
// where Keleuo's icon sits to where Applications sits.
//
//   swift scripts/dmg/background.swift <out.png> <scale>
//
// The window is WIDTH x HEIGHT points, but the scene is painted much larger (CANVAS_W x
// CANVAS_H): Finder pins the picture to the window's top left and never scales it, so a window
// made larger shows more sky and meadow rather than a blank edge. Times <scale> (1, or 2 for
// Retina). The
// same seed every time, so the stars stay where they are from one build to the next.

import AppKit
import CoreGraphics

let WIDTH = 660.0
let HEIGHT = 420.0
let CANVAS_W = 1600.0
let CANVAS_H = 1000.0
/// Where the two icons' centres are, in points from the top left; make-dmg places them there.
let APP_AT = CGPoint(x: 170, y: 190)
let APPLICATIONS_AT = CGPoint(x: 490, y: 190)
/// How far below an icon's centre Finder writes its name (for 112-point icons, 13-point text).
let LABEL_BELOW = 74.0

let args = CommandLine.arguments
guard args.count == 3, let scale = Double(args[2]) else {
    FileHandle.standardError.write("usage: background.swift <out.png> <scale>\n".data(using: .utf8)!)
    exit(2)
}
let out = URL(fileURLWithPath: args[1])

let space = CGColorSpace(name: CGColorSpace.sRGB)!
let ctx = CGContext(
    data: nil, width: Int(CANVAS_W * scale), height: Int(CANVAS_H * scale), bitsPerComponent: 8,
    bytesPerRow: 0, space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
// Points, from the top left, as the layout above is given.
ctx.scaleBy(x: scale, y: scale)
ctx.translateBy(x: 0, y: CANVAS_H)
ctx.scaleBy(x: 1, y: -1)

func rgb(_ hex: UInt32, _ alpha: Double = 1) -> CGColor {
    CGColor(
        srgbRed: Double((hex >> 16) & 0xff) / 255, green: Double((hex >> 8) & 0xff) / 255,
        blue: Double(hex & 0xff) / 255, alpha: alpha)
}

/// A small fixed-seed random source (the same sky every build).
var seed: UInt64 = 0x6b656c65756f
func random() -> Double {
    seed = seed &* 6364136223846793005 &+ 1442695040888963407
    return Double(seed >> 11) / Double(1 << 53)
}

// The sky: deep blue at the top, a little lighter and warmer toward the horizon.
let sky = CGGradient(
    colorsSpace: space,
    colors: [rgb(0x070b1f), rgb(0x0f1638), rgb(0x232a5c), rgb(0x3b3470)] as CFArray,
    locations: [0, 0.45, 0.78, 1])!
ctx.drawLinearGradient(sky, start: CGPoint(x: 0, y: 0), end: CGPoint(x: 0, y: HEIGHT * 0.78), options: [.drawsAfterEndLocation])

// A faint band of the galaxy, across the sky at a slant.
ctx.saveGState()
ctx.translateBy(x: WIDTH * 0.5, y: HEIGHT * 0.32)
ctx.rotate(by: -0.32)
for i in 0..<60 {
    let x = (random() - 0.5) * WIDTH * 1.4
    let y = (random() - 0.5) * 70
    let r = 30 + random() * 60
    let haze = CGGradient(
        colorsSpace: space, colors: [rgb(0x8f8fd8, 0.035), rgb(0x8f8fd8, 0)] as CFArray, locations: [0, 1])!
    ctx.drawRadialGradient(haze, startCenter: CGPoint(x: x, y: y), startRadius: 0, endCenter: CGPoint(x: x, y: y), endRadius: r, options: [])
    _ = i
}
ctx.restoreGState()

// Stars: many faint and small, a few bright with a soft glow; fewer near the horizon.
for _ in 0..<260 {
    let x = random() * WIDTH
    let y = pow(random(), 1.35) * HEIGHT * 0.72
    let bright = random()
    let r = 0.35 + bright * bright * 1.1
    let alpha = 0.35 + bright * 0.6
    let tint: UInt32 = random() < 0.15 ? 0xffe9c4 : (random() < 0.2 ? 0xc9d6ff : 0xffffff)
    // None behind the icons, their names, or the words: the sky stays clear there.
    // In Double throughout (newer Swift will not guess between Double and CGFloat).
    let nearIcon = [APP_AT, APPLICATIONS_AT].contains { (p: CGPoint) -> Bool in
        hypot(Double(p.x) - x, Double(p.y) + 12.0 - y) < 78.0
    }
    let nearWords = abs(x - WIDTH / 2) < 120 && abs(y - (Double(APP_AT.y) - 49.0)) < 16
    if nearIcon || nearWords { continue }
    if bright > 0.93 {
        let glow = CGGradient(
            colorsSpace: space, colors: [rgb(tint, 0.35), rgb(tint, 0)] as CFArray, locations: [0, 1])!
        ctx.drawRadialGradient(glow, startCenter: CGPoint(x: x, y: y), startRadius: 0, endCenter: CGPoint(x: x, y: y), endRadius: r * 6, options: [])
    }
    ctx.setFillColor(rgb(tint, alpha))
    ctx.fillEllipse(in: CGRect(x: x - r, y: y - r, width: r * 2, height: r * 2))
}

// More sky beyond the window, for a window made larger.
for _ in 0..<380 {
    let x = WIDTH + random() * (CANVAS_W - WIDTH)
    let y = pow(random(), 1.35) * HEIGHT * 0.72
    let r = 0.35 + pow(random(), 2) * 1.1
    ctx.setFillColor(rgb(0xffffff, 0.35 + random() * 0.55))
    ctx.fillEllipse(in: CGRect(x: x - r, y: y - r, width: r * 2, height: r * 2))
}

// The moon: a crescent at the top right, with a soft glow.
let moon = CGPoint(x: WIDTH - 86, y: 66)
let moonGlow = CGGradient(
    colorsSpace: space, colors: [rgb(0xfff4d6, 0.22), rgb(0xfff4d6, 0)] as CFArray, locations: [0, 1])!
ctx.drawRadialGradient(moonGlow, startCenter: moon, startRadius: 0, endCenter: moon, endRadius: 70, options: [])
ctx.saveGState()
ctx.beginTransparencyLayer(auxiliaryInfo: nil)
ctx.setFillColor(rgb(0xfff2d0))
ctx.fillEllipse(in: CGRect(x: moon.x - 17, y: moon.y - 17, width: 34, height: 34))
ctx.setBlendMode(.clear)
ctx.fillEllipse(in: CGRect(x: moon.x - 9, y: moon.y - 22, width: 34, height: 34))
ctx.endTransparencyLayer()
ctx.restoreGState()

/// A rolling hill: a smooth line through `heights` (fractions of the height, evenly across the
/// width), filled down to the bottom with a gradient from `top` to `bottom`.
func hill(_ heights: [Double], top: CGColor, bottom: CGColor) {
    let path = CGMutablePath()
    // The same rolls again beyond the window, as far as the canvas goes.
    let step = WIDTH / Double(heights.count - 1)
    let rolls = Int((CANVAS_W / step).rounded(.up)) + 1
    let points = (0...rolls).map { i in
        CGPoint(x: Double(i) * step, y: HEIGHT * heights[i % (heights.count - 1)])
    }
    path.move(to: CGPoint(x: 0, y: CANVAS_H))
    path.addLine(to: points[0])
    for i in 1..<points.count {
        let a = points[i - 1], b = points[i]
        let mid = (a.x + b.x) / 2
        path.addCurve(to: b, control1: CGPoint(x: mid, y: a.y), control2: CGPoint(x: mid, y: b.y))
    }
    path.addLine(to: CGPoint(x: points.last!.x, y: CANVAS_H))
    path.closeSubpath()
    let crest = heights.min()! * HEIGHT
    ctx.saveGState()
    ctx.addPath(path)
    ctx.clip()
    let fill = CGGradient(colorsSpace: space, colors: [top, bottom] as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(fill, start: CGPoint(x: 0, y: crest), end: CGPoint(x: 0, y: HEIGHT), options: [.drawsAfterEndLocation])
    ctx.restoreGState()
    // Moonlight along the crest.
    ctx.saveGState()
    ctx.addPath(path)
    ctx.setStrokeColor(rgb(0xb8c4ff, 0.10))
    ctx.setLineWidth(1.2)
    ctx.strokePath()
    ctx.restoreGState()
}

// Three meadows, the far ones bluer and lighter, the near one darkest and greenest.
hill([0.70, 0.64, 0.68, 0.61, 0.66, 0.60, 0.67], top: rgb(0x2a3566), bottom: rgb(0x1b2347))
hill([0.80, 0.74, 0.70, 0.76, 0.71, 0.75, 0.72], top: rgb(0x1d3446), bottom: rgb(0x122233))
hill([0.86, 0.90, 0.83, 0.87, 0.92, 0.85, 0.88], top: rgb(0x173225), bottom: rgb(0x0a1712))

// Fireflies over the near meadow.
for _ in 0..<14 {
    let x = random() * WIDTH
    let y = HEIGHT * (0.78 + random() * 0.17)
    let glow = CGGradient(
        colorsSpace: space, colors: [rgb(0xf6f08a, 0.55), rgb(0xf6f08a, 0)] as CFArray, locations: [0, 1])!
    ctx.drawRadialGradient(glow, startCenter: CGPoint(x: x, y: y), startRadius: 0, endCenter: CGPoint(x: x, y: y), endRadius: 4.5, options: [])
    ctx.setFillColor(rgb(0xfffbd0, 0.9))
    ctx.fillEllipse(in: CGRect(x: x - 0.7, y: y - 0.7, width: 1.4, height: 1.4))
}

// More fireflies beyond the window: to the right, and in the meadow below.
for _ in 0..<70 {
    let below = random() < 0.4
    let x = below ? random() * WIDTH : WIDTH + random() * (CANVAS_W - WIDTH)
    let y = below ? HEIGHT + random() * (CANVAS_H - HEIGHT) : HEIGHT * 0.78 + random() * (CANVAS_H - HEIGHT * 0.78)
    let glow = CGGradient(
        colorsSpace: space, colors: [rgb(0xf6f08a, 0.5), rgb(0xf6f08a, 0)] as CFArray, locations: [0, 1])!
    ctx.drawRadialGradient(glow, startCenter: CGPoint(x: x, y: y), startRadius: 0, endCenter: CGPoint(x: x, y: y), endRadius: 4.5, options: [])
    ctx.setFillColor(rgb(0xfffbd0, 0.85))
    ctx.fillEllipse(in: CGRect(x: x - 0.7, y: y - 0.7, width: 1.4, height: 1.4))
}

// The arrow from Keleuo to Applications: a gentle arc of dashes, and its head.
let from = CGPoint(x: APP_AT.x + 70, y: APP_AT.y)
let to = CGPoint(x: APPLICATIONS_AT.x - 70, y: APPLICATIONS_AT.y)
let lift = 26.0
let arc = CGMutablePath()
arc.move(to: from)
arc.addQuadCurve(to: to, control: CGPoint(x: (from.x + to.x) / 2, y: from.y - lift))
ctx.saveGState()
ctx.setShadow(offset: .zero, blur: 6, color: rgb(0xc9d2ff, 0.5))
ctx.addPath(arc)
ctx.setStrokeColor(rgb(0xe6eaff, 0.85))
ctx.setLineWidth(2.4)
ctx.setLineCap(.round)
ctx.setLineDash(phase: 0, lengths: [7, 7])
ctx.strokePath()
ctx.setLineDash(phase: 0, lengths: [])
// The head, along the arc's direction at its end.
let control = CGPoint(x: (from.x + to.x) / 2, y: from.y - lift)
let angle = atan2(to.y - control.y, to.x - control.x)
let head = CGMutablePath()
for side in [-1.0, 1.0] {
    head.move(to: to)
    head.addLine(to: CGPoint(x: to.x - 12 * cos(angle + side * 0.5), y: to.y - 12 * sin(angle + side * 0.5)))
}
ctx.addPath(head)
ctx.setLineWidth(2.6)
ctx.strokePath()
ctx.restoreGState()

// "Drag Keleuo to Applications", over the arrow.
let words = NSAttributedString(
    string: "Drag Keleuo to Applications",
    attributes: [
        .font: NSFont.systemFont(ofSize: 13, weight: .medium),
        .foregroundColor: NSColor(srgbRed: 0.88, green: 0.90, blue: 1.0, alpha: 0.8),
    ])
let line = CTLineCreateWithAttributedString(words)
let width = CTLineGetTypographicBounds(line, nil, nil, nil)
ctx.saveGState()
ctx.textMatrix = CGAffineTransform(scaleX: 1, y: -1)
ctx.textPosition = CGPoint(x: (WIDTH - width) / 2, y: APP_AT.y - 44)
CTLineDraw(line, ctx)
ctx.restoreGState()

// Under each icon's name, a pill of misty moonlight. Finder writes the names itself, black in
// light mode and white in dark, and its colour cannot be set: this shade (a relative luminance
// near 0.18) keeps both readable, each at about 4.5 to 1.
for (at, width) in [(APP_AT, 86.0), (APPLICATIONS_AT, 118.0)] {
    let pill = CGRect(x: at.x - width / 2, y: at.y + LABEL_BELOW - 12, width: width, height: 24)
    let path = CGPath(roundedRect: pill, cornerWidth: 12, cornerHeight: 12, transform: nil)
    ctx.saveGState()
    ctx.setShadow(offset: .zero, blur: 10, color: rgb(0x8a8ac0, 0.45))
    ctx.addPath(path)
    ctx.setFillColor(rgb(0x7a7aa2))
    ctx.fillPath()
    ctx.restoreGState()
    ctx.addPath(path)
    ctx.setStrokeColor(rgb(0xc8ccff, 0.25))
    ctx.setLineWidth(1)
    ctx.strokePath()
}

let image = ctx.makeImage()!
let dest = CGImageDestinationCreateWithURL(out as CFURL, "public.png" as CFString, 1, nil)!
CGImageDestinationAddImage(dest, image, [kCGImagePropertyDPIWidth: 72 * scale, kCGImagePropertyDPIHeight: 72 * scale] as CFDictionary)
CGImageDestinationFinalize(dest)
print("wrote \(out.path) (\(Int(CANVAS_W * scale))x\(Int(CANVAS_H * scale)))")
