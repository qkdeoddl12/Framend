import Foundation
import CoreGraphics
import ImageIO
import UniformTypeIdentifiers

let root = URL(fileURLWithPath: CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "fixtures/generated")
try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
let width = 800, height = 600
guard let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
                              bytesPerRow: width*4, space: CGColorSpaceCreateDeviceRGB(),
                              bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { fatalError("context") }
context.setFillColor(CGColor(red: 0.15, green: 0.32, blue: 0.28, alpha: 1))
context.fill(CGRect(x: 0, y: 0, width: width, height: height))
context.setFillColor(CGColor(red: 0.85, green: 0.72, blue: 0.4, alpha: 1))
context.fillEllipse(in: CGRect(x: 510, y: 300, width: 130, height: 130))
context.setFillColor(CGColor(red: 0.09, green: 0.18, blue: 0.17, alpha: 1))
context.move(to: CGPoint(x: 0, y: 0)); context.addLine(to: CGPoint(x: 330, y: 420))
context.addLine(to: CGPoint(x: 800, y: 0)); context.closePath(); context.fillPath()
let jpegURL = root.appending(path: "original.jpg")
guard let image = context.makeImage(),
      let destination = CGImageDestinationCreateWithURL(jpegURL as CFURL, UTType.jpeg.identifier as CFString, 1, nil) else { fatalError("JPEG") }
CGImageDestinationAddImage(destination, image, [kCGImageDestinationLossyCompressionQuality: 0.9] as CFDictionary)
guard CGImageDestinationFinalize(destination) else { fatalError("write JPEG") }
let jpeg = try Data(contentsOf: jpegURL)
var raf = Data(repeating: 0, count: 148)
raf.replaceSubrange(0..<16, with: Data("FUJIFILMCCD-RAW ".utf8))
raf.replaceSubrange(28..<32, with: Data("X-T5".utf8))
for (index, value) in [(84,148), (88,jpeg.count), (92,148+jpeg.count), (96,16), (100,164+jpeg.count), (104,32)] {
    var value = UInt32(value).bigEndian
    withUnsafeBytes(of: &value) { raf.replaceSubrange(index..<index+4, with: $0) }
}
raf.append(jpeg); raf.append(Data(repeating: 0, count: 16))
raf.append(Data([0x49,0x49,0x2a,0])); raf.append(Data(repeating: 42, count: 28))
try raf.write(to: root.appending(path: "synthetic.raf"))
var disk = Data(repeating: 0, count: 4*1024*1024-1)
disk.append(jpeg); disk.append(Data(repeating: 0, count: 8191))
disk.append(raf); disk.append(Data(repeating: 0, count: 4096))
try disk.write(to: root.appending(path: "demo-card.img"))
print("Generated \(root.path)/demo-card.img — real JPEG + synthetic RAF (not a camera compatibility test)")
