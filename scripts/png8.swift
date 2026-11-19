// Rewrites PNGs in place with 8 bits per channel (ictool writes 16): a fraction of the size,
// no visible difference. With --canvas N, centers each on a transparent N×N canvas first.
// Used by icons.sh: `swift scripts/png8.swift [--canvas N] FILE…`.
import Foundation
import ImageIO
import UniformTypeIdentifiers

var paths = Array(CommandLine.arguments.dropFirst())
var canvas: Int? = nil
if paths.first == "--canvas", paths.count >= 2 {
    canvas = Int(paths[1])
    paths.removeFirst(2)
}

func fail(_ message: String) -> Never {
    FileHandle.standardError.write("\(message)\n".data(using: .utf8)!)
    exit(1)
}

for path in paths {
    let url = URL(fileURLWithPath: path)
    guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
        let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
    else { fail("cannot read \(path)") }
    let width = canvas ?? image.width
    let height = canvas ?? image.height
    guard
        let context = CGContext(
            data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpace(name: CGColorSpace.sRGB)!,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
    else { fail("cannot draw \(path)") }
    let x = (width - image.width) / 2
    let y = (height - image.height) / 2
    context.draw(image, in: CGRect(x: x, y: y, width: image.width, height: image.height))
    guard let eight = context.makeImage(),
        let dest = CGImageDestinationCreateWithURL(
            url as CFURL, UTType.png.identifier as CFString, 1, nil)
    else { fail("cannot write \(path)") }
    CGImageDestinationAddImage(dest, eight, nil)
    if !CGImageDestinationFinalize(dest) { fail("cannot write \(path)") }
}
