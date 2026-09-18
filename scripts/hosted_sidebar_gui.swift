import AppKit
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

func fail(_ message: String) -> Never {
    FileHandle.standardError.write((message + "\n").data(using: .utf8)!)
    exit(2)
}

struct WindowInfo {
    let id: CGWindowID
    let bounds: CGRect
}

func targetWindow(_ pid: pid_t) -> WindowInfo {
    guard let rows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]],
          let row = rows.first(where: {
              ($0[kCGWindowOwnerPID as String] as? Int) == Int(pid) &&
              ($0[kCGWindowLayer as String] as? Int) == 0
          }),
          let number = row[kCGWindowNumber as String] as? NSNumber,
          let dictionary = row[kCGWindowBounds as String] as? [String: Any],
          let bounds = CGRect(dictionaryRepresentation: dictionary as CFDictionary)
    else { fail("target window unavailable") }
    return WindowInfo(id: CGWindowID(number.uint32Value), bounds: bounds)
}

func focus(_ pid: pid_t) {
    let script = "tell application \"System Events\" to set frontmost of first process whose unix id is \(pid) to true"
    var error: NSDictionary?
    NSAppleScript(source: script)?.executeAndReturnError(&error)
    if let error { fail("could not focus target process: \(error)") }
    Thread.sleep(forTimeInterval: 0.15)
}

func capture(_ pid: pid_t, _ path: String) {
    let info = targetWindow(pid)
    guard let image = CGWindowListCreateImage(.null, .optionIncludingWindow, info.id, [.boundsIgnoreFraming, .bestResolution]) else {
        fail("window capture failed")
    }
    let url = URL(fileURLWithPath: path) as CFURL
    guard let dest = CGImageDestinationCreateWithURL(url, UTType.png.identifier as CFString, 1, nil) else {
        fail("could not create PNG destination")
    }
    CGImageDestinationAddImage(dest, image, nil)
    guard CGImageDestinationFinalize(dest) else { fail("could not write PNG") }
}

func wheelCapture(_ pid: pid_t, xNorm: Double, yNorm: Double, pixels: Int32, path: String) {
    let info = targetWindow(pid)
    focus(pid)
    let point = CGPoint(
        x: info.bounds.minX + CGFloat(xNorm) * info.bounds.width,
        y: info.bounds.minY + CGFloat(yNorm) * info.bounds.height
    )
    guard let wheel = CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 1, wheel1: pixels, wheel2: 0, wheel3: 0) else {
        fail("could not create wheel event")
    }
    wheel.location = point
    wheel.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.08)
    capture(pid, path)
}

func dragCapture(_ pid: pid_t, xNorm: Double, yNorm: Double, deltaXNorm: Double, path: String) {
    let info = targetWindow(pid)
    focus(pid)
    let from = CGPoint(
        x: info.bounds.minX + CGFloat(xNorm) * info.bounds.width,
        y: info.bounds.minY + CGFloat(yNorm) * info.bounds.height
    )
    let to = CGPoint(
        x: from.x + CGFloat(deltaXNorm) * info.bounds.width,
        y: from.y
    )
    guard let down = CGEvent(mouseEventSource: nil, mouseType: .leftMouseDown, mouseCursorPosition: from, mouseButton: .left),
          let drag = CGEvent(mouseEventSource: nil, mouseType: .leftMouseDragged, mouseCursorPosition: to, mouseButton: .left),
          let up = CGEvent(mouseEventSource: nil, mouseType: .leftMouseUp, mouseCursorPosition: to, mouseButton: .left)
    else { fail("could not create drag events") }
    down.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.08)
    drag.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.12)
    up.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.2)
    capture(pid, path)
}

struct Bitmap {
    let width: Int
    let height: Int
    let bytes: [UInt8]

    func rgb(_ x: Int, _ y: Int) -> (Int, Int, Int) {
        let i = (y * width + x) * 4
        return (Int(bytes[i]), Int(bytes[i + 1]), Int(bytes[i + 2]))
    }
}

func loadBitmap(_ path: String) -> Bitmap {
    let url = URL(fileURLWithPath: path) as CFURL
    guard let src = CGImageSourceCreateWithURL(url, nil),
          let image = CGImageSourceCreateImageAtIndex(src, 0, nil)
    else { fail("could not load image: \(path)") }
    let width = image.width
    let height = image.height
    var bytes = [UInt8](repeating: 0, count: width * height * 4)
    guard let ctx = CGContext(
        data: &bytes,
        width: width,
        height: height,
        bitsPerComponent: 8,
        bytesPerRow: width * 4,
        space: CGColorSpaceCreateDeviceRGB(),
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else { fail("could not create bitmap context") }
    ctx.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
    return Bitmap(width: width, height: height, bytes: bytes)
}

func distance(_ a: (Int, Int, Int), _ b: (Int, Int, Int)) -> Int {
    abs(a.0 - b.0) + abs(a.1 - b.1) + abs(a.2 - b.2)
}

func locateBoundary(_ image: Bitmap) -> (Int, Double) {
    let minX = max(20, Int(Double(image.width) * 0.10))
    let maxX = min(image.width - 20, Int(Double(image.width) * 0.48))
    let minY = max(40, Int(Double(image.height) * 0.12))
    let maxY = min(image.height - 40, Int(Double(image.height) * 0.92))
    var bestX = minX
    var bestScore = -1.0

    for x in minX..<maxX {
        var sum = 0
        var count = 0
        for y in stride(from: minY, to: maxY, by: 6) {
            sum += distance(image.rgb(max(0, x - 4), y), image.rgb(min(image.width - 1, x + 4), y))
            count += 1
        }
        let score = count > 0 ? Double(sum) / Double(count) : 0
        if score > bestScore {
            bestScore = score
            bestX = x
        }
    }
    return (bestX, bestScore)
}

func dominantColor(_ image: Bitmap, x: Int, minY: Int, maxY: Int) -> (Int, Int, Int) {
    var rs = [Int](), gs = [Int](), bs = [Int]()
    for y in stride(from: minY, to: maxY, by: 8) {
        let c = image.rgb(max(0, min(image.width - 1, x)), y)
        rs.append(c.0); gs.append(c.1); bs.append(c.2)
    }
    rs.sort(); gs.sort(); bs.sort()
    let m = rs.count / 2
    return (rs[m], gs[m], bs[m])
}

func analyze(_ path: String) {
    let image = loadBitmap(path)
    let (boundary, score) = locateBoundary(image)
    let minY = max(40, Int(Double(image.height) * 0.12))
    let maxY = min(image.height - 40, Int(Double(image.height) * 0.92))
    let left = dominantColor(image, x: max(0, boundary - 16), minY: minY, maxY: maxY)
    let right = dominantColor(image, x: min(image.width - 1, boundary + 16), minY: minY, maxY: maxY)
    var dividerColumns = 0
    for x in max(0, boundary - 8)...min(image.width - 1, boundary + 8) {
        var other = 0
        var total = 0
        for y in stride(from: minY, to: maxY, by: 8) {
            let c = image.rgb(x, y)
            if distance(c, left) > 24 && distance(c, right) > 24 { other += 1 }
            total += 1
        }
        if total > 0 && Double(other) / Double(total) > 0.65 { dividerColumns += 1 }
    }
    let result: [String: Any] = [
        "width": image.width,
        "height": image.height,
        "boundary_x": boundary,
        "boundary_normalized_x": Double(boundary) / Double(image.width),
        "boundary_score": score,
        "divider_columns": dividerColumns
    ]
    let data = try! JSONSerialization.data(withJSONObject: result)
    print(String(data: data, encoding: .utf8)!)
}

func compareStrip(_ baselinePath: String, _ candidatePath: String, boundaryX: Int) {
    let a = loadBitmap(baselinePath)
    let b = loadBitmap(candidatePath)
    guard a.width == b.width && a.height == b.height else { fail("image size mismatch") }
    let x0 = max(0, boundaryX - 18)
    let x1 = min(a.width - 1, boundaryX + 2)
    let y0 = max(20, Int(Double(a.height) * 0.10))
    let y1 = min(a.height - 20, Int(Double(a.height) * 0.95))
    var maxRun = 0
    var currentRun = 0
    var maxWidth = 0
    var changedRows = 0
    for y in y0..<y1 {
        var changed = 0
        for x in x0...x1 {
            if distance(a.rgb(x, y), b.rgb(x, y)) > 36 { changed += 1 }
        }
        maxWidth = max(maxWidth, changed)
        if changed >= 2 {
            changedRows += 1
            currentRun += 1
            maxRun = max(maxRun, currentRun)
        } else {
            currentRun = 0
        }
    }
    let result: [String: Any] = [
        "strip_x0": x0,
        "strip_x1": x1,
        "max_changed_run": maxRun,
        "max_changed_width": maxWidth,
        "changed_rows": changedRows
    ]
    let data = try! JSONSerialization.data(withJSONObject: result)
    print(String(data: data, encoding: .utf8)!)
}

let args = Array(CommandLine.arguments.dropFirst())
guard let command = args.first else {
    fail("usage: hosted_sidebar_gui.swift <capture|wheel-capture|drag-capture|analyze|compare-strip> ...")
}

switch command {
case "capture":
    guard args.count == 3, let pid = pid_t(args[1]) else { fail("capture requires PID path") }
    capture(pid, args[2])
case "wheel-capture":
    guard args.count == 7,
          let pid = pid_t(args[1]),
          let x = Double(args[2]), let y = Double(args[3]),
          let pixels = Int32(args[4])
    else { fail("wheel-capture requires PID xNorm yNorm pixels path") }
    wheelCapture(pid, xNorm: x, yNorm: y, pixels: pixels, path: args[5])
case "drag-capture":
    guard args.count == 7,
          let pid = pid_t(args[1]),
          let x = Double(args[2]), let y = Double(args[3]),
          let dx = Double(args[4])
    else { fail("drag-capture requires PID xNorm yNorm deltaXNorm path") }
    dragCapture(pid, xNorm: x, yNorm: y, deltaXNorm: dx, path: args[5])
case "analyze":
    guard args.count == 2 else { fail("analyze requires image path") }
    analyze(args[1])
case "compare-strip":
    guard args.count == 4, let boundary = Int(args[3]) else { fail("compare-strip requires baseline candidate boundaryX") }
    compareStrip(args[1], args[2], boundaryX: boundary)
default:
    fail("unknown command: \(command)")
}
