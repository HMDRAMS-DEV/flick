// flick-segment: masks for "Fix with Claude", so an adjustment can apply to part of a photo.
// Usage: flick-segment <image> <mask.png> <overlay.jpg> <mask-width> <mask-height> <models-dir> '<prompt json>'
// The prompt is {"kind": "object" | "subject" | "people", "box": [x0, y0, x1, y1], "points": [[x, y, 1 or 0]]},
// coordinates as fractions of the image from the top left.
// - object: Segment Anything 2.1 (Meta, Apache 2.0, Apple's Core ML conversion), prompted by the box
//   and points (1 = part of it, 0 = not). The models are expected in <models-dir>.
// - subject, people: Apple Vision. A box limits the mask to that area.
// Writes the mask as a grayscale PNG at the given size, and the image with the mask tinted red so
// Claude can check it. Prints the share of the frame the mask covers, 0...1.
import CoreImage
import CoreML
import Foundation
import ImageIO
import UniformTypeIdentifiers
import Vision

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write((msg + "\n").data(using: .utf8)!)
    exit(1)
}

let args = CommandLine.arguments
guard args.count == 8, let outW = Int(args[4]), let outH = Int(args[5]) else {
    fail("usage: flick-segment <image> <mask.png> <overlay.jpg> <mask-width> <mask-height> <models-dir> <prompt-json>")
}
guard let prompt = try? JSONSerialization.jsonObject(with: Data(args[7].utf8)) as? [String: Any] else { fail("bad prompt: \(args[7])") }
let kind = prompt["kind"] as? String ?? "object"
let box = (prompt["box"] as? [NSNumber])?.map { CGFloat($0.doubleValue) }
let points = (prompt["points"] as? [[NSNumber]] ?? []).filter { $0.count == 3 }.map { $0.map { CGFloat($0.doubleValue) } }
let modelsDir = URL(fileURLWithPath: args[6])

let ctx = CIContext(options: [.workingColorSpace: CGColorSpace(name: CGColorSpace.sRGB)!])
guard let loaded = CIImage(contentsOf: URL(fileURLWithPath: args[1]), options: [.applyOrientationProperty: true]) else {
    fail("cannot read \(args[1])")
}
// Segmentation works on what is there, not on how it is exposed: auto-adjust so dark shots show detail.
var image = loaded.transformed(by: CGAffineTransform(translationX: -loaded.extent.minX, y: -loaded.extent.minY))
for f in image.autoAdjustmentFilters(options: [.redEye: false]) {
    f.setValue(image, forKey: kCIInputImageKey)
    image = f.outputImage ?? image
}
image = image.cropped(to: loaded.extent.offsetBy(dx: -loaded.extent.minX, dy: -loaded.extent.minY))
let size = image.extent.size

/// Grayscale mask the size of the image, white where selected.
func fit(_ mask: CIImage) -> CIImage {
    let e = mask.extent
    return mask.transformed(by: CGAffineTransform(scaleX: size.width / e.width, y: size.height / e.height)
        .translatedBy(x: -e.minX, y: -e.minY)).cropped(to: image.extent)
}

func render(_ img: CIImage) -> CVPixelBuffer {
    var pb: CVPixelBuffer?
    let w = Int(img.extent.width), h = Int(img.extent.height)
    CVPixelBufferCreate(nil, w, h, kCVPixelFormatType_32ARGB, [kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary, &pb)
    guard let pb else { fail("cannot allocate a \(w)x\(h) buffer") }
    ctx.render(img, to: pb)
    return pb
}

func people() -> CIImage {
    let req = VNGeneratePersonSegmentationRequest()
    req.qualityLevel = .accurate
    req.outputPixelFormat = kCVPixelFormatType_OneComponent8
    let handler = VNImageRequestHandler(ciImage: image)
    do { try handler.perform([req]) } catch { fail("person segmentation failed: \(error.localizedDescription)") }
    guard let r = req.results?.first else { return CIImage(color: .black).cropped(to: image.extent) }
    return fit(CIImage(cvPixelBuffer: r.pixelBuffer))
}

func subject() -> CIImage {
    guard #available(macOS 14.0, *) else { fail("subject masks need macOS 14 or later") }
    let req = VNGenerateForegroundInstanceMaskRequest()
    let handler = VNImageRequestHandler(cvPixelBuffer: render(image))
    do { try handler.perform([req]) } catch { fail("subject segmentation failed: \(error.localizedDescription)") }
    guard let r = req.results?.first,
          let pb = try? r.generateScaledMaskForImage(forInstances: r.allInstances, from: handler)
    else { return CIImage(color: .black).cropped(to: image.extent) }
    return fit(CIImage(cvPixelBuffer: pb))
}

/// Loads `<name>.mlmodelc`, compiling `<name>.mlpackage` on first use.
func model(_ name: String) -> MLModel {
    let compiled = modelsDir.appendingPathComponent("\(name).mlmodelc")
    let fm = FileManager.default
    if !fm.fileExists(atPath: compiled.path) {
        do {
            let tmp = try MLModel.compileModel(at: modelsDir.appendingPathComponent("\(name).mlpackage"))
            try? fm.removeItem(at: compiled)
            try fm.moveItem(at: tmp, to: compiled)
        } catch { fail("cannot compile \(name): \(error.localizedDescription)") }
    }
    let config = MLModelConfiguration()
    config.computeUnits = .cpuAndGPU
    do { return try MLModel(contentsOf: compiled, configuration: config) } catch { fail("cannot load \(name): \(error.localizedDescription)") }
}

func object() -> CIImage {
    let prefix = "SAM2_1Large"
    let encoder = model("\(prefix)ImageEncoderFLOAT16")
    let promptEncoder = model("\(prefix)PromptEncoderFLOAT16")
    let decoder = model("\(prefix)MaskDecoderFLOAT16")
    guard let (inputName, input) = encoder.modelDescription.inputDescriptionsByName.first(where: { $0.value.type == .image }),
          let c = input.imageConstraint else { fail("unexpected image encoder") }
    // The encoder takes the whole frame squeezed to a square; prompts are in that square.
    let side = CGFloat(c.pixelsWide)
    let squeezed = image.transformed(by: CGAffineTransform(scaleX: side / size.width, y: CGFloat(c.pixelsHigh) / size.height))
    do {
        let enc = try encoder.prediction(from: MLDictionaryFeatureProvider(dictionary: [inputName: MLFeatureValue(pixelBuffer: render(squeezed))]))
        var prompts: [(CGFloat, CGFloat, Int32)] = []
        if let b = box, b.count == 4 { prompts += [(b[0], b[1], 2), (b[2], b[3], 3)] }
        prompts += points.map { ($0[0], $0[1], $0[2] > 0.5 ? 1 : 0) }
        if prompts.isEmpty { fail("an object mask needs a box or points") }
        let pts = try MLMultiArray(shape: [1, NSNumber(value: prompts.count), 2], dataType: .float32)
        let labels = try MLMultiArray(shape: [1, NSNumber(value: prompts.count)], dataType: .int32)
        for (i, p) in prompts.enumerated() {
            pts[[0, i, 0] as [NSNumber]] = NSNumber(value: Float(p.0 * side))
            pts[[0, i, 1] as [NSNumber]] = NSNumber(value: Float(p.1 * CGFloat(c.pixelsHigh)))
            labels[[0, i] as [NSNumber]] = NSNumber(value: p.2)
        }
        let pe = try promptEncoder.prediction(from: MLDictionaryFeatureProvider(dictionary: ["points": pts, "labels": labels]))
        let dec = try decoder.prediction(from: MLDictionaryFeatureProvider(dictionary: [
            "image_embedding": enc.featureValue(for: "image_embedding")!,
            "feats_s0": enc.featureValue(for: "feats_s0")!,
            "feats_s1": enc.featureValue(for: "feats_s1")!,
            "sparse_embedding": pe.featureValue(for: "sparse_embeddings")!,
            "dense_embedding": pe.featureValue(for: "dense_embeddings")!,
        ]))
        guard let masks = dec.featureValue(for: "low_res_masks")?.multiArrayValue,
              let scores = dec.featureValue(for: "scores")?.multiArrayValue else { fail("unexpected mask decoder output") }
        let n = masks.shape[1].intValue, h = masks.shape[2].intValue, w = masks.shape[3].intValue
        let best = (0..<n).max { scores[[0, $0] as [NSNumber]].floatValue < scores[[0, $1] as [NSNumber]].floatValue } ?? 0
        // The model answers at 256 px in logits. As SAM does, scale the logits up and cut at 0,
        // which gives a smoother edge than scaling a cut mask.
        var logits = [Float](repeating: 0, count: w * h)
        for i in 0..<(w * h) { logits[i] = masks[[0, NSNumber(value: best), NSNumber(value: i / w), NSNumber(value: i % w)]].floatValue }
        let coarse = CIImage(bitmapData: logits.withUnsafeBufferPointer { Data(buffer: $0) }, bytesPerRow: w * 4,
                             size: CGSize(width: w, height: h), format: .Rf, colorSpace: nil)
        let v = CIVector(x: 0.5, y: 0, z: 0, w: 0)
        return fit(coarse.clampedToExtent().cropped(to: coarse.extent)).applyingFilter("CIColorMatrix", parameters: [
            "inputRVector": v, "inputGVector": v, "inputBVector": v, "inputAVector": CIVector(x: 0, y: 0, z: 0, w: 1),
            "inputBiasVector": CIVector(x: 0.5, y: 0.5, z: 0.5, w: 0),
        ]).applyingFilter("CIColorClamp")
    } catch { fail("segmentation failed: \(error.localizedDescription)") }
}

var mask: CIImage
switch kind {
case "people": mask = people()
case "subject": mask = subject()
case "object": mask = object()
default: fail("unknown kind \(kind)")
}
// Sharpen the soft edge to a few pixels, and keep it gray: one channel, 0...1.
mask = mask.applyingFilter("CIColorMatrix", parameters: [
    "inputRVector": CIVector(x: 3, y: 0, z: 0, w: 0), "inputGVector": CIVector(x: 3, y: 0, z: 0, w: 0),
    "inputBVector": CIVector(x: 3, y: 0, z: 0, w: 0), "inputAVector": CIVector(x: 0, y: 0, z: 0, w: 1),
    "inputBiasVector": CIVector(x: -1, y: -1, z: -1, w: 0),
]).applyingFilter("CIColorClamp")
if kind != "object", let b = box, b.count == 4 {
    let r = CGRect(x: b[0] * size.width, y: (1 - b[3]) * size.height, width: (b[2] - b[0]) * size.width, height: (b[3] - b[1]) * size.height)
    mask = mask.cropped(to: r).composited(over: CIImage(color: .black).cropped(to: image.extent))
}
mask = mask.cropped(to: image.extent)

var coverage: Float = 0
var px = [Float](repeating: 0, count: 4)
let avg = mask.applyingFilter("CIAreaAverage", parameters: [kCIInputExtentKey: CIVector(cgRect: image.extent)])
ctx.render(avg, toBitmap: &px, rowBytes: 16, bounds: CGRect(x: 0, y: 0, width: 1, height: 1), format: .RGBAf, colorSpace: nil)
coverage = px[0]

let gray = CGColorSpaceCreateDeviceGray()
let full = mask.transformed(by: CGAffineTransform(scaleX: CGFloat(outW) / size.width, y: CGFloat(outH) / size.height))
do {
    try ctx.writePNGRepresentation(of: full, to: URL(fileURLWithPath: args[2]), format: .L8, colorSpace: gray)
} catch { fail("cannot write \(args[2]): \(error.localizedDescription)") }

// Small enough for Claude to look at; the tint is translucent so the edge can be checked against the photo.
let scale = min(1, 1024 / max(size.width, size.height))
let red = CIImage(color: CIColor(red: 1, green: 0.1, blue: 0.1)).cropped(to: image.extent)
let half = CIVector(x: 0.55, y: 0, z: 0, w: 0)
let tint = mask.applyingFilter("CIColorMatrix", parameters: ["inputRVector": half, "inputGVector": half, "inputBVector": half])
let overlay = red.applyingFilter("CIBlendWithMask", parameters: [kCIInputBackgroundImageKey: image, kCIInputMaskImageKey: tint])
    .transformed(by: CGAffineTransform(scaleX: scale, y: scale))
do {
    try ctx.writeJPEGRepresentation(of: overlay, to: URL(fileURLWithPath: args[3]), colorSpace: CGColorSpace(name: CGColorSpace.sRGB)!,
                                    options: [kCGImageDestinationLossyCompressionQuality as CIImageRepresentationOption: 0.85])
} catch { fail("cannot write \(args[3]): \(error.localizedDescription)") }
print(String(format: "%.3f", coverage))
