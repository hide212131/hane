import AppKit
import CoreGraphics
import Foundation

func fail(_ message: String) -> Never {
    FileHandle.standardError.write((message + "\n").data(using: .utf8)!)
    exit(2)
}

func jsonPrint(_ value: [String: Any]) {
    guard JSONSerialization.isValidJSONObject(value),
          let data = try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]),
          let text = String(data: data, encoding: .utf8) else {
        fail("could not encode JSON")
    }
    print(text)
}

func targetWindow(_ pid: pid_t) -> (id: CGWindowID, bounds: CGRect) {
    guard let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]],
          let row = windows.first(where: {
              ($0[kCGWindowOwnerPID as String] as? Int) == Int(pid)
                  && ($0[kCGWindowLayer as String] as? Int) == 0
          }),
          let number = row[kCGWindowNumber as String] as? NSNumber,
          let dictionary = row[kCGWindowBounds as String] as? [String: Any],
          let bounds = CGRect(dictionaryRepresentation: dictionary as CFDictionary)
    else { fail("target window unavailable") }
    return (CGWindowID(number.uint32Value), bounds)
}

func focus(_ pid: pid_t) {
    let source = """
    tell application "System Events"
      set frontmost of first process whose unix id is (pid) to true
    end tell
    """
    var error: NSDictionary?
    NSAppleScript(source: source)?.executeAndReturnError(&error)
    if error != nil { fail("could not focus target process") }
    Thread.sleep(forTimeInterval: 0.15)
}

func captureWindow(_ id: CGWindowID, _ path: String) {
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
    process.arguments = ["-x", "-l\(id)", path]
    do {
        try process.run()
        process.waitUntilExit()
    } catch {
        fail("could not run screencapture")
    }
    if process.terminationStatus != 0 { fail("screencapture failed") }
}

struct Raster {
    let width: Int
    let height: Int
    let bytes: [UInt8]

    func rgb(_ x: Int, _ y: Int) -> UInt32 {
        let index = (y * width + x) * 4
        return (UInt32(bytes[index]) << 16) | (UInt32(bytes[index + 1]) << 8) | UInt32(bytes[index + 2])
    }
}

func raster(_ path: String) -> Raster {
    guard let image = NSImage(contentsOfFile: path) else { fail("could not load screenshot") }
    var rect = CGRect(origin: .zero, size: image.size)
    guard let cg = image.cgImage(forProposedRect: &rect, context: nil, hints: nil) else {
        fail("could not create CGImage")
    }
    let width = cg.width
    let height = cg.height
    var bytes = [UInt8](repeating: 0, count: width * height * 4)
    guard let context = CGContext(
        data: &bytes,
        width: width,
        height: height,
        bitsPerComponent: 8,
        bytesPerRow: width * 4,
        space: CGColorSpaceCreateDeviceRGB(),
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else { fail("could not create bitmap context") }
    context.draw(cg, in: CGRect(x: 0, y: 0, width: width, height: height))
    return Raster(width: width, height: height, bytes: bytes)
}

func dominantColor(_ image: Raster, x0: Int, x1: Int, y0: Int, y1: Int) -> UInt32 {
    var counts: [UInt32: Int] = [:]
    if x0 >= x1 || y0 >= y1 { fail("invalid analysis ROI") }
    for y in y0..<y1 {
        for x in x0..<x1 {
            counts[image.rgb(x, y), default: 0] += 1
        }
    }
    guard let result = counts.max(by: { $0.value < $1.value })?.key else {
        fail("could not find dominant color")
    }
    return result
}

func colorString(_ color: UInt32) -> String {
    String(format: "#%06x", color)
}

func columnDominance(_ image: Raster, x: Int, y0: Int, y1: Int) -> (UInt32, Double) {
    var counts: [UInt32: Int] = [:]
    for y in y0..<y1 {
        counts[image.rgb(x, y), default: 0] += 1
    }
    guard let pair = counts.max(by: { $0.value < $1.value }) else { fail("empty column") }
    return (pair.key, Double(pair.value) / Double(y1 - y0))
}

func longestRunDifferentFrom(_ image: Raster, x: Int, y0: Int, y1: Int, color: UInt32) -> Int {
    var best = 0
    var current = 0
    for y in y0..<y1 {
        if image.rgb(x, y) != color {
            current += 1
            best = max(best, current)
        } else {
            current = 0
        }
    }
    return best
}

func maxConsecutive(_ values: [Int]) -> Int {
    if values.isEmpty { return 0 }
    let sorted = values.sorted()
    var best = 1
    var current = 1
    for index in 1..<sorted.count {
        if sorted[index] == sorted[index - 1] + 1 {
            current += 1
            best = max(best, current)
        } else if sorted[index] != sorted[index - 1] {
            current = 1
        }
    }
    return best
}

func analyze(_ path: String) -> [String: Any] {
    let image = raster(path)
    let width = image.width
    let height = image.height
    if width < 400 || height < 300 { fail("unexpected screenshot size") }

    let y0 = max(30, Int(Double(height) * 0.05))
    let y1 = min(height - 10, Int(Double(height) * 0.98))
    let sidebarBackground = dominantColor(
        image,
        x0: 5,
        x1: max(6, Int(Double(width) * 0.19)),
        y0: max(y0, Int(Double(height) * 0.30)),
        y1: y1
    )
    let mainBackground = dominantColor(
        image,
        x0: Int(Double(width) * 0.32),
        x1: width - 5,
        y0: max(y0, Int(Double(height) * 0.20)),
        y1: y1
    )

    let searchStart = Int(Double(width) * 0.20)
    let searchEnd = Int(Double(width) * 0.26)
    var dividerCandidates: [(x: Int, color: UInt32, ratio: Double)] = []
    for x in searchStart...searchEnd {
        let (color, ratio) = columnDominance(image, x: x, y0: y0, y1: y1)
        if ratio >= 0.97 && color != sidebarBackground && color != mainBackground {
            dividerCandidates.append((x, color, ratio))
        }
    }
    guard let divider = dividerCandidates.max(by: { $0.ratio < $1.ratio }) else {
        fail("could not locate thin sidebar divider")
    }

    var dividerColumns: [Int] = []
    for candidate in dividerCandidates where candidate.color == divider.color {
        dividerColumns.append(candidate.x)
    }
    let dividerWidth = maxConsecutive(dividerColumns)

    let trackStart = max(0, divider.x - 18)
    let trackEnd = max(trackStart, divider.x - 3)
    var activeColumns: [Int] = []
    var maxRun = 0
    if trackStart <= trackEnd {
        for x in trackStart...trackEnd {
            let run = longestRunDifferentFrom(image, x: x, y0: y0, y1: y1, color: sidebarBackground)
            maxRun = max(maxRun, run)
            if run >= 20 {
                activeColumns.append(x)
            }
        }
    }
    let thumbWidth = maxConsecutive(activeColumns)

    return [
        "width": width,
        "height": height,
        "divider_x": divider.x,
        "divider_width": dividerWidth,
        "divider_color": colorString(divider.color),
        "sidebar_background": colorString(sidebarBackground),
        "main_background": colorString(mainBackground),
        "track_start_x": trackStart,
        "track_end_x": trackEnd,
        "thumb_width": thumbWidth,
        "max_non_background_run": maxRun,
        "active_columns": activeColumns,
    ]
}

func wheelCaptureAnalyze(_ pid: pid_t, pixels: Int32, path: String) {
    let info = targetWindow(pid)
    focus(pid)
    let point = CGPoint(
        x: info.bounds.minX + info.bounds.width * 0.12,
        y: info.bounds.minY + info.bounds.height * 0.55
    )
    guard let wheel = CGEvent(
        scrollWheelEvent2Source: nil,
        units: .pixel,
        wheelCount: 1,
        wheel1: pixels,
        wheel2: 0,
        wheel3: 0
    ) else { fail("could not create scroll event") }
    wheel.location = point
    wheel.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.05)
    captureWindow(info.id, path)
    var metrics = analyze(path)
    metrics["event"] = [
        "kind": "wheel",
        "x": Double(point.x),
        "y": Double(point.y),
        "pixels": Int(pixels),
    ]
    jsonPrint(metrics)
}

func dragDivider(_ pid: pid_t, fraction: Double, delta: Double) {
    if !fraction.isFinite || fraction <= 0 || fraction >= 1 || !delta.isFinite || abs(delta) > 100 {
        fail("invalid drag arguments")
    }
    let info = targetWindow(pid)
    focus(pid)
    let start = CGPoint(
        x: info.bounds.minX + info.bounds.width * fraction,
        y: info.bounds.minY + info.bounds.height * 0.55
    )
    let finish = CGPoint(x: start.x + delta, y: start.y)
    guard let down = CGEvent(mouseEventSource: nil, mouseType: .leftMouseDown, mouseCursorPosition: start, mouseButton: .left),
          let drag = CGEvent(mouseEventSource: nil, mouseType: .leftMouseDragged, mouseCursorPosition: finish, mouseButton: .left),
          let up = CGEvent(mouseEventSource: nil, mouseType: .leftMouseUp, mouseCursorPosition: finish, mouseButton: .left)
    else { fail("could not create drag events") }
    down.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.08)
    drag.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.08)
    up.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.2)
    jsonPrint([
        "kind": "drag",
        "start_x": Double(start.x),
        "start_y": Double(start.y),
        "finish_x": Double(finish.x),
        "finish_y": Double(finish.y),
        "delta_x": delta,
    ])
}

let args = Array(CommandLine.arguments.dropFirst())
guard let command = args.first else { fail("missing command") }

switch command {
case "analyze":
    guard args.count == 2 else { fail("analyze requires screenshot path") }
    jsonPrint(analyze(args[1]))
case "wheel-capture-analyze":
    guard args.count == 4,
          let pid = pid_t(args[1]),
          let pixels = Int32(args[2])
    else { fail("wheel-capture-analyze requires PID, pixels, screenshot path") }
    wheelCaptureAnalyze(pid, pixels: pixels, path: args[3])
case "drag-divider":
    guard args.count == 4,
          let pid = pid_t(args[1]),
          let fraction = Double(args[2]),
          let delta = Double(args[3])
    else { fail("drag-divider requires PID, x fraction, delta") }
    dragDivider(pid, fraction: fraction, delta: delta)
default:
    fail("unsupported command")
}
