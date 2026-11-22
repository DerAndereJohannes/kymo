// Cuts Kymo's icon out of macOS's tile, for Windows and Linux: takes two renderings of the
// same size by ictool, one over a solid black tile and one over a solid white one, and
// recovers each pixel's coverage from their difference (over black, a pixel is already its
// premultiplied color). Crops to the icon's bounds, shadow included, with a margin of 4%, and
// writes it at each size with 8 bits per channel. Used by icons.sh:
// `swift scripts/icon-bare.swift BLACK WHITE OUTDIR SIZE…` writes OUTDIR/kymo-SIZE.png.
import Foundation
import ImageIO
import UniformTypeIdentifiers

func fail(_ message: String) -> Never {
    FileHandle.standardError.write("\(message)\n".data(using: .utf8)!)
    exit(1)
}

let args = Array(CommandLine.arguments.dropFirst())
guard args.count >= 4 else { fail("usage: icon-bare.swift BLACK WHITE OUTDIR SIZE…") }
let sizes = args[3...].map { arg -> Int in
    guard let size = Int(arg), size > 0 else { fail("not a size: \(arg)") }
    return size
}
let srgb = CGColorSpace(name: CGColorSpace.sRGB)!

func context(_ width: Int, _ height: Int) -> CGContext {
    guard
        let context = CGContext(
            data: nil, width: width, height: height, bitsPerComponent: 8,
            bytesPerRow: width * 4, space: srgb,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
    else { fail("cannot allocate \(width)×\(height)") }
    return context
}

// Premultiplied RGBA, 8 bits per channel.
func pixels(_ path: String) -> (width: Int, height: Int, data: [UInt8]) {
    guard
        let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
        let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
    else { fail("cannot read \(path)") }
    let ctx = context(image.width, image.height)
    ctx.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
    let count = image.width * image.height * 4
    let data = Array(UnsafeBufferPointer(start: ctx.data!.assumingMemoryBound(to: UInt8.self), count: count))
    return (image.width, image.height, data)
}

let black = pixels(args[0])
let white = pixels(args[1])
guard black.width == white.width, black.height == white.height else { fail("sizes differ") }
let (width, height) = (black.width, black.height)

// The tile's glass rim, a highlight along its edge, is no simple overlay the difference
// cancels, so everything within 3% of the edge is dropped (the icon lies well inside): the
// tile's opaque area, eroded by that much in both directions.
let rim = max(1, width * 3 / 100)
var inside = (0..<width * height).map { black.data[$0 * 4 + 3] == 255 }
for (count, stride, lines, step) in [(width, 1, height, width), (height, width, width, 1)] {
    var eroded = inside
    for line in 0..<lines {
        let start = line * step
        // Distance to the nearest pixel outside, forwards and backwards along the line; the
        // tile can reach the image's edge, which counts as outside.
        var run = 0
        for i in 0..<count {
            let p = start + i * stride
            run = inside[p] ? run + 1 : 0
            if run <= rim { eroded[p] = false }
        }
        run = 0
        for i in (0..<count).reversed() {
            let p = start + i * stride
            run = inside[p] ? run + 1 : 0
            if run <= rim { eroded[p] = false }
        }
    }
    inside = eroded
}

// Over black a pixel is t·a·F, over white t·(a·F + 1 − a), with t the tile's coverage and a
// the icon's: the difference is t·(1 − a), so the icon's alpha is t minus it.
var bare = [UInt8](repeating: 0, count: width * height * 4)
var (minX, minY, maxX, maxY) = (width, height, -1, -1)
for y in 0..<height {
    for x in 0..<width where inside[y * width + x] {
        let o = (y * width + x) * 4
        var difference = 0
        for c in 0..<3 { difference += Int(white.data[o + c]) - Int(black.data[o + c]) }
        let alpha = max(0, min(Int(black.data[o + 3]), Int(black.data[o + 3]) - difference / 3))
        for c in 0..<3 { bare[o + c] = min(black.data[o + c], UInt8(alpha)) }
        bare[o + 3] = UInt8(alpha)
        // The bounds leave out the shadow's faintest edge.
        if alpha > 8 {
            (minX, minY, maxX, maxY) = (min(minX, x), min(minY, y), max(maxX, x), max(maxY, y))
        }
    }
}
guard maxX >= 0 else { fail("the icon is empty") }

let cut = context(width, height)
bare.withUnsafeBytes { _ = memcpy(cut.data!, $0.baseAddress!, $0.count) }
let image = cut.makeImage()!
// A square around the bounds' center, with a margin of 4% of its side on each side.
let side = Double(max(maxX - minX, maxY - minY) + 1) / 0.92
let centerX = Double(minX + maxX + 1) / 2
let centerY = Double(minY + maxY + 1) / 2

for size in sizes {
    let out = context(size, size)
    out.interpolationQuality = CGInterpolationQuality.high
    let scale = Double(size) / side
    // CoreGraphics' origin is at the bottom left, the pixels' at the top left.
    let rect = CGRect(
        x: Double(size) / 2 - centerX * scale,
        y: Double(size) / 2 - (Double(height) - centerY) * scale,
        width: Double(width) * scale, height: Double(height) * scale)
    out.draw(image, in: rect)
    let url = URL(fileURLWithPath: args[2]).appendingPathComponent("kymo-\(size).png")
    guard let png = out.makeImage(),
        let dest = CGImageDestinationCreateWithURL(
            url as CFURL, UTType.png.identifier as CFString, 1, nil)
    else { fail("cannot write \(url.path)") }
    CGImageDestinationAddImage(dest, png, nil)
    if !CGImageDestinationFinalize(dest) { fail("cannot write \(url.path)") }
}
