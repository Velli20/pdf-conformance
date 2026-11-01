// Renders PDF pages with Apple's PDFKit, which Safari shows PDFs with, for the conformance
// harness's WebKit context column.
//
//   render --corpus <root> --out <dir> [--scale 1.5] [--max-side 3000] [--max-pages 10]
//          [--annotations] [--jobs N] [--timeout 60]
//   render --pdf <file> --out <dir> [same options]
//
// Each page is written to `<out>/<PDF path relative to the corpus root>/pN.png`, sized like the
// harness sizes its renders: the rotated crop box at `scale` pixels per point, shrunk so no side
// exceeds `max-side`, each side rounded up. With `--corpus`, every PDF is rendered in its own
// child process under a time limit, so a document that crashes or hangs PDFKit costs only itself.

import CoreGraphics
import Foundation
import ImageIO
import PDFKit
import UniformTypeIdentifiers

struct Options {
    var corpus: URL?
    var pdf: URL?
    var out = URL(fileURLWithPath: ".")
    var relative = ""
    var scale: CGFloat = 1.5
    var maxSide: CGFloat = 3000
    var maxPages = 10
    var annotations = false
    var jobs = ProcessInfo.processInfo.activeProcessorCount
    var timeout: TimeInterval = 60

    init(_ arguments: [String]) throws {
        var iterator = arguments.dropFirst().makeIterator()
        func value(_ flag: String) throws -> String {
            guard let value = iterator.next() else { throw Failure("\(flag) needs a value") }
            return value
        }
        while let flag = iterator.next() {
            switch flag {
            case "--corpus": corpus = URL(fileURLWithPath: try value(flag)).standardizedFileURL
            case "--pdf": pdf = URL(fileURLWithPath: try value(flag)).standardizedFileURL
            case "--out": out = URL(fileURLWithPath: try value(flag)).standardizedFileURL
            case "--relative": relative = try value(flag)
            case "--scale": scale = CGFloat(try number(try value(flag), flag))
            case "--max-side": maxSide = CGFloat(try number(try value(flag), flag))
            case "--max-pages": maxPages = Int(try number(try value(flag), flag))
            case "--annotations": annotations = true
            case "--jobs": jobs = max(1, Int(try number(try value(flag), flag)))
            case "--timeout": timeout = try number(try value(flag), flag)
            default: throw Failure("unknown argument \(flag)")
            }
        }
        if (corpus == nil) == (pdf == nil) {
            throw Failure("pass exactly one of --corpus <root> and --pdf <file>")
        }
    }

    /// Arguments that make a child render `pdf` the way this process would.
    func child(pdf: URL, relative: String) -> [String] {
        var arguments = [
            "--pdf", pdf.path, "--out", out.path, "--relative", relative,
            "--scale", "\(scale)", "--max-side", "\(maxSide)", "--max-pages", "\(maxPages)",
        ]
        if annotations { arguments.append("--annotations") }
        return arguments
    }
}

struct Failure: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}

func number(_ text: String, _ flag: String) throws -> Double {
    guard let value = Double(text), value.isFinite, value > 0 else {
        throw Failure("\(flag) must be a positive number, not \(text)")
    }
    return value
}

/// Returns the raster size of a page box of `width` x `height` points.
func rasterSize(width: CGFloat, height: CGFloat, options: Options) -> (Int, Int)? {
    guard width > 0, height > 0, width.isFinite, height.isFinite else { return nil }
    let scale = min(options.scale, options.maxSide / max(width, height))
    return (Int(max(1, (width * scale).rounded(.up))), Int(max(1, (height * scale).rounded(.up))))
}

/// Renders the first pages of one document.
func renderDocument(_ options: Options) throws {
    guard let url = options.pdf, let document = PDFDocument(url: url) else {
        throw Failure("PDFKit cannot open the document")
    }
    if document.isLocked && !document.unlock(withPassword: "") {
        throw Failure("the document needs a password")
    }
    let dir = options.out.appendingPathComponent(options.relative.isEmpty ? url.lastPathComponent : options.relative)
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    for index in 0..<min(document.pageCount, options.maxPages) {
        guard let page = document.page(at: index) else { continue }
        page.displaysAnnotations = options.annotations
        let box = page.bounds(for: .cropBox)
        let quarterTurn = (page.rotation / 90) % 2 != 0
        let (pageWidth, pageHeight) = quarterTurn ? (box.height, box.width) : (box.width, box.height)
        guard let size = rasterSize(width: pageWidth, height: pageHeight, options: options) else { continue }
        let (width, height) = size
        guard let context = CGContext(
                  data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                  space: CGColorSpace(name: CGColorSpace.sRGB)!,
                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        else { continue }
        context.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        // Stretches the rotated crop box onto the raster exactly, as the other renderers do.
        context.scaleBy(x: CGFloat(width) / pageWidth, y: CGFloat(height) / pageHeight)
        page.draw(with: .cropBox, to: context)
        guard let image = context.makeImage() else { continue }
        let file = dir.appendingPathComponent("p\(index).png") as CFURL
        guard let destination = CGImageDestinationCreateWithURL(file, UTType.png.identifier as CFString, 1, nil) else {
            throw Failure("cannot write \(file)")
        }
        CGImageDestinationAddImage(destination, image, nil)
        if !CGImageDestinationFinalize(destination) { throw Failure("cannot write \(file)") }
    }
}

/// Returns every PDF under `root` with its path relative to `root`, sorted.
func corpusDocuments(_ root: URL) -> [(URL, String)] {
    let prefix = root.path.hasSuffix("/") ? root.path : root.path + "/"
    let files = FileManager.default.enumerator(at: root, includingPropertiesForKeys: nil)?
        .compactMap { $0 as? URL } ?? []
    return files
        .filter { $0.pathExtension.lowercased() == "pdf" }
        .map { file in
            let path = file.standardizedFileURL.path
            return (file, path.hasPrefix(prefix) ? String(path.dropFirst(prefix.count)) : file.lastPathComponent)
        }
        .sorted { $0.1 < $1.1 }
}

/// Renders every document of the corpus in child processes, `jobs` at a time.
func renderCorpus(_ options: Options, root: URL) {
    let documents = corpusDocuments(root)
    let executable = URL(fileURLWithPath: CommandLine.arguments[0]).standardizedFileURL
    let slots = DispatchSemaphore(value: options.jobs)
    let group = DispatchGroup()
    let lock = NSLock()
    var failed = 0
    for (pdf, relative) in documents {
        slots.wait()
        group.enter()
        DispatchQueue.global().async {
            defer { slots.signal(); group.leave() }
            let process = Process()
            process.executableURL = executable
            process.arguments = options.child(pdf: pdf, relative: relative)
            let finished = DispatchSemaphore(value: 0)
            process.terminationHandler = { _ in finished.signal() }
            var outcome = "ok"
            do {
                try process.run()
                if finished.wait(timeout: .now() + options.timeout) == .timedOut {
                    process.terminate()
                    finished.wait()
                    outcome = "time limit exceeded"
                } else if process.terminationStatus != 0 {
                    outcome = "exit \(process.terminationStatus)"
                }
            } catch {
                outcome = "\(error)"
            }
            if outcome != "ok" {
                lock.lock(); failed += 1; lock.unlock()
                FileHandle.standardError.write("\(relative): \(outcome)\n".data(using: .utf8)!)
            }
        }
    }
    group.wait()
    print("Rendered \(documents.count - failed) of \(documents.count) documents with PDFKit")
}

do {
    let options = try Options(CommandLine.arguments)
    if let root = options.corpus {
        renderCorpus(options, root: root)
    } else {
        try renderDocument(options)
    }
} catch {
    FileHandle.standardError.write("render: \(error)\n".data(using: .utf8)!)
    exit(1)
}
