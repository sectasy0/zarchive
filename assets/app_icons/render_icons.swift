import CoreGraphics
import Foundation
import ImageIO

let outputDirectory = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
let scale: CGFloat = 4

func color(_ hex: UInt32, alpha: CGFloat = 1) -> CGColor {
    CGColor(
        red: CGFloat((hex >> 16) & 0xff) / 255,
        green: CGFloat((hex >> 8) & 0xff) / 255,
        blue: CGFloat(hex & 0xff) / 255,
        alpha: alpha
    )
}

func gradient(_ context: CGContext, colors: [CGColor], from: CGPoint, to: CGPoint) {
    let gradient = CGGradient(
        colorsSpace: CGColorSpaceCreateDeviceRGB(),
        colors: colors as CFArray,
        locations: nil
    )!
    context.drawLinearGradient(gradient, start: from, end: to, options: [])
}

func fillGradient(
    _ context: CGContext,
    path: CGPath,
    colors: [CGColor],
    from: CGPoint,
    to: CGPoint
) {
    context.saveGState()
    context.addPath(path)
    context.clip()
    gradient(context, colors: colors, from: from, to: to)
    context.restoreGState()
}

func roundedRect(_ rect: CGRect, radius: CGFloat) -> CGPath {
    CGPath(roundedRect: rect, cornerWidth: radius, cornerHeight: radius, transform: nil)
}

func folderPath(x: CGFloat, y: CGFloat, width: CGFloat, height: CGFloat) -> CGPath {
    let path = CGMutablePath()
    path.move(to: CGPoint(x: x + width * 0.06, y: y + height * 0.18))
    path.addLine(to: CGPoint(x: x + width * 0.36, y: y + height * 0.18))
    path.addCurve(
        to: CGPoint(x: x + width * 0.49, y: y + height * 0.30),
        control1: CGPoint(x: x + width * 0.42, y: y + height * 0.18),
        control2: CGPoint(x: x + width * 0.43, y: y + height * 0.30)
    )
    path.addLine(to: CGPoint(x: x + width * 0.91, y: y + height * 0.30))
    path.addCurve(
        to: CGPoint(x: x + width, y: y + height * 0.40),
        control1: CGPoint(x: x + width, y: y + height * 0.30),
        control2: CGPoint(x: x + width, y: y + height * 0.34)
    )
    path.addLine(to: CGPoint(x: x + width, y: y + height * 0.88))
    path.addCurve(
        to: CGPoint(x: x + width * 0.90, y: y + height),
        control1: CGPoint(x: x + width, y: y + height * 0.95),
        control2: CGPoint(x: x + width * 0.96, y: y + height)
    )
    path.addLine(to: CGPoint(x: x + width * 0.10, y: y + height))
    path.addCurve(
        to: CGPoint(x: x, y: y + height * 0.88),
        control1: CGPoint(x: x + width * 0.04, y: y + height),
        control2: CGPoint(x: x, y: y + height * 0.95)
    )
    path.addLine(to: CGPoint(x: x, y: y + height * 0.28))
    path.addCurve(
        to: CGPoint(x: x + width * 0.06, y: y + height * 0.18),
        control1: CGPoint(x: x, y: y + height * 0.22),
        control2: CGPoint(x: x + width * 0.03, y: y + height * 0.18)
    )
    path.closeSubpath()
    return path
}

func drawLayers(
    _ context: CGContext,
    x: CGFloat,
    y: CGFloat,
    width: CGFloat,
    height: CGFloat
) {
    let heights: [CGFloat] = [0.50, 0.59, 0.68]
    let widths: [CGFloat] = [0.25, 0.21, 0.17]
    for index in heights.indices {
        let rect = CGRect(
            x: x,
            y: y + height * heights[index],
            width: width * widths[index],
            height: width * 0.055
        )
        let path = roundedRect(rect, radius: width * 0.025)
        context.addPath(path)
        context.setFillColor(color(0xd7f7ff, alpha: 0.78))
        context.fillPath()
    }
}

func drawFolder(_ context: CGContext, size: CGFloat, fullTile: Bool) {
    let x = size * (fullTile ? 0.12 : 0.055)
    let y = size * (fullTile ? 0.09 : 0.04)
    let width = size * (fullTile ? 0.76 : 0.89)
    let height = size * (fullTile ? 0.70 : 0.78)
    let back = folderPath(x: x, y: y, width: width, height: height)

    context.saveGState()
    context.setShadow(
        offset: CGSize(width: 0, height: size * 0.018),
        blur: size * 0.035,
        color: color(0x062b58, alpha: fullTile ? 0.35 : 0.30)
    )
    context.addPath(back)
    context.setFillColor(color(0x1477d4))
    context.fillPath()
    context.restoreGState()

    fillGradient(
        context,
        path: back,
        colors: [color(0x50b9ff), color(0x1477d4)],
        from: CGPoint(x: x, y: y),
        to: CGPoint(x: x + width, y: y + height)
    )

    let frontRect = CGRect(
        x: x,
        y: y + height * 0.34,
        width: width,
        height: height * 0.66
    )
    let front = roundedRect(frontRect, radius: width * 0.075)
    context.saveGState()
    context.setShadow(
        offset: CGSize(width: 0, height: size * 0.010),
        blur: size * 0.020,
        color: color(0x06356a, alpha: 0.20)
    )
    fillGradient(
        context,
        path: front,
        colors: [color(0x36a5f5), color(0x0872ce)],
        from: CGPoint(x: x, y: frontRect.minY),
        to: CGPoint(x: x + width, y: frontRect.maxY)
    )
    context.restoreGState()

    context.addPath(front)
    context.setStrokeColor(color(0xa6e1ff, alpha: 0.45))
    context.setLineWidth(size * 0.003)
    context.strokePath()

    if fullTile && size >= 96 {
        drawLayers(context, x: x + width * 0.15, y: y, width: width, height: height)
    }

    let strapWidth = width * 0.105
    let strapX = x + (width - strapWidth) / 2
    let strapRect = CGRect(
        x: strapX,
        y: y + height * 0.20,
        width: strapWidth,
        height: height * 0.80
    )
    let strap = roundedRect(strapRect, radius: strapWidth * 0.24)
    fillGradient(
        context,
        path: strap,
        colors: [color(0xffc15c), color(0xf47d1e)],
        from: CGPoint(x: strapRect.minX, y: strapRect.minY),
        to: CGPoint(x: strapRect.maxX, y: strapRect.maxY)
    )

    for offset in [-1, 1] {
        let stitchX = x + width / 2 + CGFloat(offset) * strapWidth * 0.31
        context.saveGState()
        context.setLineDash(phase: 0, lengths: [size * 0.009, size * 0.010])
        context.setStrokeColor(color(0xb95216, alpha: 0.58))
        context.setLineWidth(size * 0.0025)
        context.move(to: CGPoint(x: stitchX, y: strapRect.minY + size * 0.015))
        context.addLine(to: CGPoint(x: stitchX, y: strapRect.maxY - size * 0.015))
        context.strokePath()
        context.restoreGState()
    }

    let pullWidth = width * 0.20
    let pullHeight = height * 0.22
    let pullRect = CGRect(
        x: x + (width - pullWidth) / 2,
        y: y + height * 0.43,
        width: pullWidth,
        height: pullHeight
    )
    let pull = roundedRect(pullRect, radius: pullWidth * 0.32)
    context.saveGState()
    context.setShadow(
        offset: CGSize(width: 0, height: size * 0.006),
        blur: size * 0.010,
        color: color(0x8d3c10, alpha: 0.30)
    )
    fillGradient(
        context,
        path: pull,
        colors: [color(0xffd17a), color(0xf18427)],
        from: CGPoint(x: pullRect.minX, y: pullRect.minY),
        to: CGPoint(x: pullRect.maxX, y: pullRect.maxY)
    )
    context.restoreGState()

    let openingRect = pullRect.insetBy(dx: pullWidth * 0.34, dy: pullHeight * 0.31)
    let opening = roundedRect(openingRect, radius: pullWidth * 0.12)
    context.addPath(opening)
    context.setFillColor(color(0xb84f17, alpha: 0.92))
    context.fillPath()
}

func render(size: Int, fullTile: Bool, name: String) throws {
    let dimension = CGFloat(size)
    let pixels = size * Int(scale)
    let context = CGContext(
        data: nil,
        width: pixels,
        height: pixels,
        bitsPerComponent: 8,
        bytesPerRow: pixels * 4,
        space: CGColorSpaceCreateDeviceRGB(),
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    )!
    context.translateBy(x: 0, y: CGFloat(pixels))
    context.scaleBy(x: scale, y: -scale)

    let canvas = CGRect(x: 0, y: 0, width: size, height: size)
    if fullTile {
        gradient(
            context,
            colors: [color(0x1742a4), color(0x067eaf), color(0x12c6cf)],
            from: CGPoint(x: 0, y: 0),
            to: CGPoint(x: dimension, y: dimension)
        )
        let glowColors = [
            color(0x85eaff, alpha: 0.25),
            color(0x85eaff, alpha: 0.0),
        ] as CFArray
        let glow = CGGradient(colorsSpace: CGColorSpaceCreateDeviceRGB(), colors: glowColors, locations: [0, 1])!
        context.drawRadialGradient(
            glow,
            startCenter: CGPoint(x: dimension * 0.78, y: dimension * 0.18),
            startRadius: 0,
            endCenter: CGPoint(x: dimension * 0.78, y: dimension * 0.18),
            endRadius: dimension * 0.72,
            options: []
        )
        context.addRect(canvas.insetBy(dx: dimension * 0.012, dy: dimension * 0.012))
        context.setStrokeColor(color(0xc5f4ff, alpha: 0.24))
        context.setLineWidth(dimension * 0.002)
        context.strokePath()
    }

    drawFolder(context, size: CGFloat(size), fullTile: fullTile)

    guard let supersampledImage = context.makeImage() else {
        throw NSError(domain: "IconRenderer", code: 1)
    }
    let outputContext = CGContext(
        data: nil,
        width: size,
        height: size,
        bitsPerComponent: 8,
        bytesPerRow: size * 4,
        space: CGColorSpaceCreateDeviceRGB(),
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    )!
    outputContext.interpolationQuality = .high
    outputContext.draw(supersampledImage, in: CGRect(x: 0, y: 0, width: size, height: size))
    guard let image = outputContext.makeImage() else {
        throw NSError(domain: "IconRenderer", code: 4)
    }
    let destinationURL = outputDirectory.appendingPathComponent(name)
    guard let destination = CGImageDestinationCreateWithURL(
        destinationURL as CFURL,
        "public.png" as CFString,
        1,
        nil
    ) else {
        throw NSError(domain: "IconRenderer", code: 2)
    }
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination) else {
        throw NSError(domain: "IconRenderer", code: 3)
    }
}

do {
    for size in [128, 256, 512, 1024] {
        try render(size: size, fullTile: true, name: "icon_full_\(size).png")
    }
    try render(size: 1024, fullTile: true, name: "icon_full_512@2x.png")
    for size in [16, 24, 32, 48, 64] {
        try render(size: size, fullTile: false, name: "icon_folder_\(size).png")
    }
} catch {
    fputs("Icon generation failed: \(error)\n", stderr)
    exit(1)
}