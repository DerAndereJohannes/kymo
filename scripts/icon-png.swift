// Renders kymo.svg, the icon for every platform, to PNGs of the given sizes with 8 bits per
// channel, as macOS draws SVG. Used by icons.sh:
// `swift scripts/icon-png.swift SVG OUTDIR SIZE…` writes OUTDIR/kymo-SIZE.png.
import AppKit
import UniformTypeIdentifiers

func fail(_ message: String) -> Never {
    FileHandle.standardError.write("\(message)\n".data(using: .utf8)!)
    exit(1)
}

let args = Array(CommandLine.arguments.dropFirst())
guard args.count >= 3 else { fail("usage: icon-png.swift SVG OUTDIR SIZE…") }
guard let svg = NSImage(contentsOf: URL(fileURLWithPath: args[0])) else {
    fail("cannot read \(args[0])")
}
for arg in args[2...] {
    guard let size = Int(arg), size > 0 else { fail("not a size: \(arg)") }
    guard
        let context = CGContext(
            data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpace(name: CGColorSpace.sRGB)!,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
    else { fail("cannot draw \(size)×\(size)") }
    NSGraphicsContext.current = NSGraphicsContext(cgContext: context, flipped: false)
    svg.draw(in: NSRect(x: 0, y: 0, width: size, height: size))
    NSGraphicsContext.current = nil
    let url = URL(fileURLWithPath: args[1]).appendingPathComponent("kymo-\(size).png")
    guard let image = context.makeImage(),
        let dest = CGImageDestinationCreateWithURL(
            url as CFURL, UTType.png.identifier as CFString, 1, nil)
    else { fail("cannot write \(url.path)") }
    CGImageDestinationAddImage(dest, image, nil)
    if !CGImageDestinationFinalize(dest) { fail("cannot write \(url.path)") }
}
