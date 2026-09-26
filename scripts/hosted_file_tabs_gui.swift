#!/usr/bin/env swift

// Minimal OS-level input/analysis helper for the focused file-tab hover
// path / copy icon GUI validation (Issue #354,
// scripts/hosted_file_tabs_gui.py). Deliberately separate from
// scripts/hosted_gui_interaction.swift: every command here is specific to
// locating and clicking on-screen affordances (a theme label, a tab label,
// an unlabeled copy icon) by real OCR text location and real pixel
// contrast, never by trusting the target binary's own internal state as an
// oracle. Every interaction goes through real CGEvent/AppleScript OS input,
// never direct injection into the target process.

import AppKit
import CoreGraphics
import Foundation
import Vision

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
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

func runAppleScript(_ source: String) {
    var error: NSDictionary?
    NSAppleScript(source: source)?.executeAndReturnError(&error)
    if let error { fail("System Events automation failed: \(error)") }
}

func focus(_ pid: pid_t) {
    guard let application = NSRunningApplication(processIdentifier: pid) else {
        fail("target application is not running: \(pid)")
    }
    _ = application.activate(options: [.activateAllWindows])
    runAppleScript("tell application \"System Events\" to set frontmost of first process whose unix id is \(pid) to true")
    Thread.sleep(forTimeInterval: 0.2)
}

func windowBounds(_ pid: pid_t) -> CGRect {
    guard let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]],
          let row = windows.first(where: { ($0[kCGWindowOwnerPID as String] as? Int) == Int(pid) && ($0[kCGWindowLayer as String] as? Int) == 0 }),
          let dictionary = row[kCGWindowBounds as String] as? [String: Any],
          let bounds = CGRect(dictionaryRepresentation: dictionary as CFDictionary)
    else { fail("target window bounds unavailable") }
    return bounds
}

// `fractionYFromTop` is 0 at the window's top edge and 1 at its bottom edge,
// matching the pixel-space (top-left origin) coordinates the analysis
// commands below already work in, so callers never have to re-derive a
// Vision-style bottom-left fraction themselves.
func screenPoint(_ bounds: CGRect, fractionX: Double, fractionYFromTop: Double) -> CGPoint {
    CGPoint(
        x: bounds.minX + CGFloat(fractionX) * bounds.width,
        y: bounds.minY + CGFloat(fractionYFromTop) * bounds.height
    )
}

func postClick(_ point: CGPoint) {
    guard let down = CGEvent(mouseEventSource: nil, mouseType: .leftMouseDown, mouseCursorPosition: point, mouseButton: .left),
          let up = CGEvent(mouseEventSource: nil, mouseType: .leftMouseUp, mouseCursorPosition: point, mouseButton: .left)
    else { fail("could not create OS click events") }
    down.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.05)
    up.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.3)
}

func postMouseMove(_ point: CGPoint) {
    guard let move = CGEvent(mouseEventSource: nil, mouseType: .mouseMoved, mouseCursorPosition: point, mouseButton: .left)
    else { fail("could not create OS mouse-move event") }
    move.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.4)
}

// matchedText/box mirror hosted_gui_interaction.swift's click-text contract:
// the bounding box is OCR evidence, never treated as ground truth about the
// product's own layout by the caller.
func findTextMatch(_ path: String, _ pattern: String) -> (matchedText: String, box: CGRect) {
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    request.usesLanguageCorrection = false
    request.recognitionLanguages = ["en-US", "ja-JP"]
    do {
        try VNImageRequestHandler(url: URL(fileURLWithPath: path), options: [:]).perform([request])
    } catch { fail("OCR failed: \(error)") }
    guard let regex = try? NSRegularExpression(pattern: pattern) else {
        fail("invalid regex pattern: \(pattern)")
    }
    for observation in request.results ?? [] {
        guard let candidate = observation.topCandidates(1).first else { continue }
        let text = candidate.string
        let fullRange = NSRange(text.startIndex..<text.endIndex, in: text)
        guard let match = regex.firstMatch(in: text, range: fullRange), let range = Range(match.range, in: text) else { continue }
        guard let box = try? candidate.boundingBox(for: range) else { continue }
        return (String(text[range]), box.boundingBox)
    }
    fail("no OCR match for pattern: \(pattern)")
}

// Vision's normalized box is bottom-left origin, y-up; every command below
// that needs a single interaction point converts it to a from-top fraction
// this way, matching screenPoint's convention.
func centerFraction(_ box: CGRect) -> (x: Double, yFromTop: Double) {
    (Double(box.midX), 1 - Double(box.midY))
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
    guard let image = NSImage(contentsOfFile: path) else { fail("could not load screenshot: \(path)") }
    var rect = CGRect(origin: .zero, size: image.size)
    guard let cg = image.cgImage(forProposedRect: &rect, context: nil, hints: nil) else {
        fail("could not create CGImage: \(path)")
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

func colorString(_ color: UInt32) -> String {
    String(format: "#%06x", color)
}

func colorComponents(_ color: UInt32) -> (r: Int, g: Int, b: Int) {
    (Int((color >> 16) & 0xff), Int((color >> 8) & 0xff), Int(color & 0xff))
}

func maxChannelDistance(_ a: UInt32, _ b: UInt32) -> Int {
    let x = colorComponents(a)
    let y = colorComponents(b)
    return max(abs(x.r - y.r), abs(x.g - y.g), abs(x.b - y.b))
}

func dominantColor(_ image: Raster, x0: Int, x1: Int, y0: Int, y1: Int) -> UInt32 {
    if x0 >= x1 || y0 >= y1 { fail("invalid analysis ROI") }
    var counts: [UInt32: Int] = [:]
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

// Pixel-space (top-left origin, y-down) rect derived from a Vision
// normalized (bottom-left origin, y-up) bounding box, using the same
// `1 - y` convention as centerFraction/screenPoint above.
func pixelRect(_ box: CGRect, width: Int, height: Int) -> (x0: Int, y0: Int, x1: Int, y1: Int) {
    (
        Int(Double(box.minX) * Double(width)),
        Int((1 - Double(box.maxY)) * Double(height)),
        Int(Double(box.maxX) * Double(width)),
        Int((1 - Double(box.minY)) * Double(height))
    )
}

func boundingBoxJSON(_ box: CGRect) -> [String: Any] {
    [
        "minX": Double(box.minX), "maxX": Double(box.maxX),
        "minY": Double(box.minY), "maxY": Double(box.maxY),
    ]
}

func windowBoundsJSON(_ bounds: CGRect) -> [String: Any] {
    [
        "x": Double(bounds.minX), "y": Double(bounds.minY),
        "width": Double(bounds.width), "height": Double(bounds.height),
    ]
}

func activate(_ pid: pid_t) {
    focus(pid)
}

func findText(_ path: String, _ pattern: String) {
    let match = findTextMatch(path, pattern)
    let image = raster(path)
    jsonPrint([
        "matched_text": match.matchedText,
        "bounding_box": boundingBoxJSON(match.box),
        "image_size": ["width": image.width, "height": image.height],
    ])
}

func clickTextCenter(_ pid: pid_t, _ path: String, _ pattern: String) {
    let match = findTextMatch(path, pattern)
    let bounds = windowBounds(pid)
    let fraction = centerFraction(match.box)
    let point = screenPoint(bounds, fractionX: fraction.x, fractionYFromTop: fraction.yFromTop)
    focus(pid)
    postClick(point)
    jsonPrint([
        "matched_text": match.matchedText,
        "bounding_box": boundingBoxJSON(match.box),
        "window_bounds": windowBoundsJSON(bounds),
        "click_point": ["x": Double(point.x), "y": Double(point.y)],
    ])
}

func hoverTextCenter(_ pid: pid_t, _ path: String, _ pattern: String) {
    let match = findTextMatch(path, pattern)
    let bounds = windowBounds(pid)
    let fraction = centerFraction(match.box)
    let point = screenPoint(bounds, fractionX: fraction.x, fractionYFromTop: fraction.yFromTop)
    focus(pid)
    postMouseMove(point)
    jsonPrint([
        "matched_text": match.matchedText,
        "bounding_box": boundingBoxJSON(match.box),
        "window_bounds": windowBoundsJSON(bounds),
        "hover_point": ["x": Double(point.x), "y": Double(point.y)],
    ])
}

// Samples the tab's own background just outside the label glyphs (above
// them, or below if the label sits too close to the screenshot's top edge)
// rather than trusting any fixed proportion of the window, since the tab's
// on-screen position depends on how many tabs are open.
func tabBackground(_ path: String, _ pattern: String) {
    let match = findTextMatch(path, pattern)
    let image = raster(path)
    let text = pixelRect(match.box, width: image.width, height: image.height)
    var sampleY0 = text.y0 - 14
    var sampleY1 = text.y0 - 2
    if sampleY0 < 0 {
        sampleY0 = text.y1 + 2
        sampleY1 = min(image.height, text.y1 + 14)
    }
    let sampleX0 = max(0, text.x0 - 4)
    let sampleX1 = min(image.width, text.x1 + 4)
    guard sampleX0 < sampleX1, sampleY0 < sampleY1, sampleY1 <= image.height, sampleY0 >= 0 else {
        fail("invalid tab background sample rect")
    }
    let color = dominantColor(image, x0: sampleX0, x1: sampleX1, y0: sampleY0, y1: sampleY1)
    jsonPrint([
        "matched_text": match.matchedText,
        "bounding_box": boundingBoxJSON(match.box),
        "sample_rect": ["x0": sampleX0, "y0": sampleY0, "x1": sampleX1, "y1": sampleY1],
        "background_color": colorString(color),
    ])
}

// The HoverCard's absolute-path text can wrap across more than one visual
// line (it is a whitespace_normal flex_1 div under a 560px max width), so a
// single-observation regex match (as findTextMatch uses) cannot locate it
// reliably. This instead accepts every OCR observation whose
// whitespace-stripped text is itself a substring of the expected
// whitespace-stripped absolute path, and returns the pixel envelope of all
// such fragments together, independent of how many lines the path wrapped
// into.
func findPathFragments(_ path: String, _ expectedNoWhitespace: String) {
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    request.usesLanguageCorrection = false
    do {
        try VNImageRequestHandler(url: URL(fileURLWithPath: path), options: [:]).perform([request])
    } catch { fail("OCR failed: \(error)") }

    var fragments: [(text: String, box: CGRect)] = []
    for observation in request.results ?? [] {
        guard let candidate = observation.topCandidates(1).first else { continue }
        let stripped = String(candidate.string.filter { !$0.isWhitespace })
        if stripped.isEmpty { continue }

        // HoverCard text is intentionally constrained by its max width, so a
        // long absolute path may be rendered with a trailing ellipsis. OCR
        // then sees a visible prefix such as ".../file-tabs-focu..." rather
        // than the complete path. Strip only a trailing ellipsis for matching;
        // the later clipboard check still proves the exact full path.
        let visible = stripped
            .replacingOccurrences(of: "…", with: "")
            .replacingOccurrences(of: "...", with: "")
        if visible.isEmpty { continue }
        guard expectedNoWhitespace.contains(visible) else { continue }

        let fullRange = candidate.string.startIndex..<candidate.string.endIndex
        guard let box = try? candidate.boundingBox(for: fullRange) else { continue }
        fragments.append((visible, box.boundingBox))
    }
    guard !fragments.isEmpty else { fail("no OCR fragment matched the expected path") }

    let minX = fragments.map { $0.box.minX }.min()!
    let maxX = fragments.map { $0.box.maxX }.max()!
    let minY = fragments.map { $0.box.minY }.min()!
    let maxY = fragments.map { $0.box.maxY }.max()!
    let image = raster(path)
    let envelope = pixelRect(
        CGRect(x: minX, y: minY, width: maxX - minX, height: maxY - minY),
        width: image.width, height: image.height
    )
    jsonPrint([
        "fragments": fragments.map { ["text": $0.text] },
        "envelope_pixel_rect": ["x0": envelope.x0, "y0": envelope.y0, "x1": envelope.x1, "y1": envelope.y1],
        "image_size": ["width": image.width, "height": image.height],
    ])
}

// Locates the copy icon by pixel contrast alone (never by asking the
// target's own layout), immediately to the right of the absolute-path
// text's pixel envelope: it samples the dominant background color of the
// search band, then finds the contiguous run of columns whose pixels
// depart from that background across a large share of the band's height.
func copyIconProbe(_ path: String, _ envX0: Int, _ envY0: Int, _ envX1: Int, _ envY1: Int) {
    let image = raster(path)
    let searchMargin = 220
    let verticalPad = 8
    let contrastThreshold = 40
    let searchX0 = max(0, min(image.width, envX1))
    let searchX1 = max(searchX0, min(image.width, envX1 + searchMargin))
    let searchY0 = max(0, envY0 - verticalPad)
    let searchY1 = min(image.height, envY1 + verticalPad)
    guard searchX0 < searchX1, searchY0 < searchY1 else { fail("invalid copy icon search rect") }

    let background = dominantColor(image, x0: searchX0, x1: searchX1, y0: searchY0, y1: searchY1)
    let bandHeight = searchY1 - searchY0
    let minColumnCount = max(1, Int(Double(bandHeight) * 0.3))

    var columnCounts: [Int: Int] = [:]
    for x in searchX0..<searchX1 {
        var count = 0
        for y in searchY0..<searchY1 where maxChannelDistance(image.rgb(x, y), background) > contrastThreshold {
            count += 1
        }
        if count >= minColumnCount { columnCounts[x] = count }
    }
    let qualifying = columnCounts.keys.sorted()
    guard !qualifying.isEmpty else { fail("copy icon not found: no contrasting pixel run in search band") }

    var bestRun: [Int] = []
    var currentRun: [Int] = []
    for x in qualifying {
        if let last = currentRun.last, x == last + 1 {
            currentRun.append(x)
        } else {
            currentRun = [x]
        }
        if currentRun.count > bestRun.count { bestRun = currentRun }
    }
    let iconX0 = bestRun.first!
    let iconX1 = bestRun.last! + 1

    var iconY0 = searchY1
    var iconY1 = searchY0
    for x in iconX0..<iconX1 {
        for y in searchY0..<searchY1 where maxChannelDistance(image.rgb(x, y), background) > contrastThreshold {
            iconY0 = min(iconY0, y)
            iconY1 = max(iconY1, y + 1)
        }
    }
    if iconY0 >= iconY1 {
        iconY0 = searchY0
        iconY1 = searchY1
    }

    let centerX = Double(iconX0 + iconX1) / 2.0
    let centerY = Double(iconY0 + iconY1) / 2.0
    jsonPrint([
        "background_color": colorString(background),
        "icon_pixel_rect": ["x0": iconX0, "y0": iconY0, "x1": iconX1, "y1": iconY1],
        "icon_center_fraction": ["x": centerX / Double(image.width), "y_from_top": centerY / Double(image.height)],
        "contrast_run": bestRun.count,
        "search_rect": ["x0": searchX0, "y0": searchY0, "x1": searchX1, "y1": searchY1],
    ])
}

func clickFraction(_ pid: pid_t, _ fractionX: Double, _ fractionYFromTop: Double) {
    guard fractionX.isFinite, fractionYFromTop.isFinite,
          fractionX > 0, fractionX < 1, fractionYFromTop > 0, fractionYFromTop < 1
    else { fail("fraction must be within (0, 1)") }
    let bounds = windowBounds(pid)
    let point = screenPoint(bounds, fractionX: fractionX, fractionYFromTop: fractionYFromTop)
    focus(pid)
    postClick(point)
    jsonPrint([
        "window_bounds": windowBoundsJSON(bounds),
        "fraction": ["x": fractionX, "y_from_top": fractionYFromTop],
        "click_point": ["x": Double(point.x), "y": Double(point.y)],
    ])
}

func readClipboard() {
    print(NSPasteboard.general.string(forType: .string) ?? "")
}

let arguments = Array(CommandLine.arguments.dropFirst())
guard let command = arguments.first else {
    fail("usage: hosted_file_tabs_gui.swift <activate|find-text|click-text|hover-text|tab-background|find-path-fragments|copy-icon-probe|click-fraction|read-clipboard> ...")
}

switch command {
case "activate":
    guard arguments.count == 2, let pid = pid_t(arguments[1]) else { fail("activate requires PID") }
    activate(pid)
case "find-text":
    guard arguments.count == 3 else { fail("find-text requires screenshot path and pattern") }
    findText(arguments[1], arguments[2])
case "click-text":
    guard arguments.count == 4, let pid = pid_t(arguments[1]) else { fail("click-text requires PID, screenshot path and pattern") }
    clickTextCenter(pid, arguments[2], arguments[3])
case "hover-text":
    guard arguments.count == 4, let pid = pid_t(arguments[1]) else { fail("hover-text requires PID, screenshot path and pattern") }
    hoverTextCenter(pid, arguments[2], arguments[3])
case "tab-background":
    guard arguments.count == 3 else { fail("tab-background requires screenshot path and pattern") }
    tabBackground(arguments[1], arguments[2])
case "find-path-fragments":
    guard arguments.count == 3 else { fail("find-path-fragments requires screenshot path and expected whitespace-stripped path") }
    findPathFragments(arguments[1], arguments[2])
case "copy-icon-probe":
    guard arguments.count == 6,
          let x0 = Int(arguments[2]), let y0 = Int(arguments[3]),
          let x1 = Int(arguments[4]), let y1 = Int(arguments[5])
    else { fail("copy-icon-probe requires screenshot path and envelope x0 y0 x1 y1") }
    copyIconProbe(arguments[1], x0, y0, x1, y1)
case "click-fraction":
    guard arguments.count == 4, let pid = pid_t(arguments[1]),
          let fractionX = Double(arguments[2]), let fractionYFromTop = Double(arguments[3])
    else { fail("click-fraction requires PID, x fraction and y-from-top fraction") }
    clickFraction(pid, fractionX, fractionYFromTop)
case "read-clipboard":
    guard arguments.count == 1 else { fail("read-clipboard takes no arguments") }
    readClipboard()
default:
    fail("unknown command: \(command)")
}
