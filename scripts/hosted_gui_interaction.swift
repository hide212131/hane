#!/usr/bin/env swift

// Minimal OS-level input helper for the hosted GUI interaction spike
// (scripts/hosted_gui_interaction.py). Deliberately separate from the
// pinned target's scripts/phase0_input.swift: that file belongs to the
// checked-out target commit and is not edited by this procedure. Every
// keystroke here goes through System Events (synthesized OS key events), never
// direct Unicode insertion, so it does not misrepresent what was proven.

import AppKit
import Carbon
import CryptoKit
import Foundation
import Vision

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(2)
}

func runAppleScript(_ source: String) {
    var error: NSDictionary?
    NSAppleScript(source: source)?.executeAndReturnError(&error)
    if let error { fail("System Events automation failed: \(error)") }
}

func inputSources() -> [TISInputSource] {
    let properties = [
        kTISPropertyInputSourceCategory!: kTISCategoryKeyboardInputSource as Any,
    ] as CFDictionary
    return TISCreateInputSourceList(properties, true).takeRetainedValue() as! [TISInputSource]
}

func sourceID(_ source: TISInputSource) -> String? {
    guard let pointer = TISGetInputSourceProperty(source, kTISPropertyInputSourceID) else { return nil }
    return Unmanaged<CFString>.fromOpaque(pointer).takeUnretainedValue() as String
}

func currentSourceID() -> String {
    guard let id = sourceID(TISCopyCurrentKeyboardInputSource().takeRetainedValue()) else {
        fail("current input source has no identifier")
    }
    return id
}

func selectSource(_ id: String) {
    // Apple's TextInputSources.h requires an input mode's parent method
    // to be enabled before the mode can be selected. Hosted images list
    // Japanese modes even though their parent is disabled initially.
    if ProcessInfo.processInfo.environment["GITHUB_ACTIONS"] == "true" {
        for parent in inputSources() {
            guard let parentID = sourceID(parent), id.hasPrefix(parentID + "."),
                  let typePointer = TISGetInputSourceProperty(parent, kTISPropertyInputSourceType)
            else { continue }
            let type = Unmanaged<CFString>.fromOpaque(typePointer).takeUnretainedValue()
            if type as String == kTISTypeKeyboardInputMethodModeEnabled as String {
                let code = TISEnableInputSource(parent)
                guard code == noErr else { fail("could not enable parent \(parentID): \(code)") }
            }
        }
    }
    guard let source = inputSources().first(where: { sourceID($0) == id }) else {
        fail("input source not found: \(id)")
    }
    let enabled = TISEnableInputSource(source)
    guard enabled == noErr else { fail("could not enable input source \(id): \(enabled)") }
    let status = TISSelectInputSource(source)
    guard status == noErr else { fail("could not select input source \(id): \(status)") }
}

func escapeForAppleScript(_ s: String) -> String {
    s.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"")
}

func selectAllTypeSave(_ pid: pid_t, _ text: String) {
    let escaped = escapeForAppleScript(text)
    runAppleScript("""
    tell application "System Events"
        tell first process whose unix id is \(pid)
            set frontmost to true
            delay 0.2
            keystroke "a" using command down
            delay 0.1
            keystroke "\(escaped)"
            delay 0.2
            keystroke "s" using command down
            delay 0.3
        end tell
    end tell
    """)
}

func appendSave(_ pid: pid_t, _ text: String) {
    let escaped = escapeForAppleScript(text)
    runAppleScript("""
    tell application "System Events"
        tell first process whose unix id is \(pid)
            set frontmost to true
            key code 124 using command down
            delay 1.0
            keystroke "\(escaped)"
            delay 0.2
            keystroke "s" using command down
            delay 0.3
        end tell
    end tell
    """)
}

func undoSave(_ pid: pid_t) {
    runAppleScript("""
    tell application "System Events"
        tell first process whose unix id is \(pid)
            set frontmost to true
            delay 0.1
            keystroke "z" using command down
            delay 0.2
            keystroke "s" using command down
            delay 0.3
        end tell
    end tell
    """)
}

func redoSave(_ pid: pid_t) {
    runAppleScript("""
    tell application "System Events"
        tell first process whose unix id is \(pid)
            set frontmost to true
            delay 0.1
            keystroke "z" using {command down, shift down}
            delay 0.2
            keystroke "s" using command down
            delay 0.3
        end tell
    end tell
    """)
}

func selectAllTypeRomajiCommitSave(_ pid: pid_t, _ romaji: String, _ inputSource: String) {
    runAppleScript("tell application \"System Events\" to set frontmost of first process whose unix id is \(pid) to true")
    Thread.sleep(forTimeInterval: 0.3)
    selectSource(inputSource)
    guard currentSourceID() == inputSource else { fail("input source did not become active") }
    let escaped = escapeForAppleScript(romaji)
    runAppleScript("""
    tell application "System Events"
        tell first process whose unix id is \(pid)
            set frontmost to true
            delay 0.2
            keystroke "a" using command down
            delay 0.1
            keystroke "\(escaped)"
            delay 0.3
            key code 49
            delay 0.3
            key code 36
            delay 0.3
            keystroke "s" using command down
            delay 0.3
        end tell
    end tell
    """)
}

func typeRomajiAtCaretCommitSave(_ pid: pid_t, _ romaji: String, _ inputSource: String) {
    focus(pid)
    selectSource(inputSource)
    guard currentSourceID() == inputSource else { fail("input source did not become active") }
    let escaped = escapeForAppleScript(romaji)
    runAppleScript("""
    tell application "System Events"
        tell first process whose unix id is \(pid)
            set frontmost to true
            delay 0.2
            keystroke "\(escaped)"
            delay 0.3
            key code 49
            delay 0.3
            key code 36
            delay 0.3
            keystroke "s" using command down
            delay 0.3
        end tell
    end tell
    """)
}

func recognizeText(_ path: String) {
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    request.usesLanguageCorrection = false
    request.recognitionLanguages = ["en-US", "ja-JP"]
    do {
        try VNImageRequestHandler(url: URL(fileURLWithPath: path), options: [:]).perform([request])
        for observation in request.results ?? [] {
            if let candidate = observation.topCandidates(1).first { print(candidate.string) }
        }
    } catch { fail("OCR failed: \(error)") }
}

// Crop a fixed editor-body region from the screenshot, excluding the top 15%
// where Hane renders the dynamic revision/frame header. CGImage crop rectangles
// use the image's top-left pixel origin, unlike Vision's normalized bottom-left
// coordinates, so express this crop directly in CGImage pixel coordinates.
// The body crop is saved as a PNG evidence artifact and its PNG-byte SHA-256 is
// printed. Receipt validation recomputes this same digest from the staged crop.
func imagePixelDigest(_ path: String) {
    guard let image = NSImage(contentsOfFile: path) else { fail("image could not be loaded: \(path)") }
    var proposed = CGRect(origin: .zero, size: image.size)
    guard let cgImage = image.cgImage(forProposedRect: &proposed, context: nil, hints: nil) else {
        fail("image has no CGImage: \(path)")
    }
    let topInset = CGFloat(cgImage.height) * 0.15
    let bodyRect = CGRect(
        x: 0,
        y: topInset,
        width: CGFloat(cgImage.width),
        height: CGFloat(cgImage.height) - topInset
    ).integral
    guard let body = cgImage.cropping(to: bodyRect) else {
        fail("could not crop editor body: \(path)")
    }
    let representation = NSBitmapImageRep(cgImage: body)
    guard let png = representation.representation(using: .png, properties: [:]) else {
        fail("could not encode editor body crop: \(path)")
    }
    let outputPath = (path as NSString).deletingPathExtension + ".body.png"
    do {
        try png.write(to: URL(fileURLWithPath: outputPath), options: .atomic)
    } catch {
        fail("could not write editor body crop \(outputPath): \(error)")
    }
    let digest = SHA256.hash(data: png)
    print(digest.map { String(format: "%02x", $0) }.joined())
}

func findTextMatch(_ path: String, _ pattern: String) -> CGRect {
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
        return box.boundingBox
    }
    fail("no OCR match for pattern: \(pattern)")
}

func windowBounds(_ pid: pid_t) -> CGRect {
    guard let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]],
          let row = windows.first(where: { ($0[kCGWindowOwnerPID as String] as? Int) == Int(pid) && ($0[kCGWindowLayer as String] as? Int) == 0 }),
          let dictionary = row[kCGWindowBounds as String] as? [String: Any],
          let bounds = CGRect(dictionaryRepresentation: dictionary as CFDictionary)
    else { fail("target window bounds unavailable") }
    return bounds
}

func screenPoint(_ bounds: CGRect, _ normalized: CGRect, _ edge: String) -> CGPoint {
    let xNorm: CGFloat = edge == "start" ? normalized.minX : normalized.maxX
    let yNormFromTop = 1 - (normalized.minY + normalized.height / 2)
    return CGPoint(x: bounds.minX + xNorm * bounds.width, y: bounds.minY + yNormFromTop * bounds.height)
}

func focus(_ pid: pid_t) {
    runAppleScript("tell application \"System Events\" to set frontmost of first process whose unix id is \(pid) to true")
    Thread.sleep(forTimeInterval: 0.2)
}

func postClick(_ point: CGPoint) {
    guard let down = CGEvent(mouseEventSource: nil, mouseType: .leftMouseDown, mouseCursorPosition: point, mouseButton: .left),
          let up = CGEvent(mouseEventSource: nil, mouseType: .leftMouseUp, mouseCursorPosition: point, mouseButton: .left)
    else { fail("could not create OS pointer events") }
    down.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.05)
    up.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.3)
}

func clickText(_ pid: pid_t, _ screenshotPath: String, _ pattern: String, _ edge: String) {
    guard edge == "start" || edge == "end" else { fail("edge must be start or end") }
    let box = findTextMatch(screenshotPath, pattern)
    let point = screenPoint(windowBounds(pid), box, edge)
    focus(pid)
    postClick(point)
    print("clicked at \(point.x),\(point.y) for pattern \(pattern) edge=\(edge)")
}

func dragSelectText(_ pid: pid_t, _ screenshotPath: String, _ pattern1: String, _ edge1: String, _ pattern2: String, _ edge2: String) {
    guard edge1 == "start" || edge1 == "end", edge2 == "start" || edge2 == "end" else { fail("edge must be start or end") }
    let bounds = windowBounds(pid)
    let from = screenPoint(bounds, findTextMatch(screenshotPath, pattern1), edge1)
    let to = screenPoint(bounds, findTextMatch(screenshotPath, pattern2), edge2)
    focus(pid)
    guard let down = CGEvent(mouseEventSource: nil, mouseType: .leftMouseDown, mouseCursorPosition: from, mouseButton: .left),
          let drag = CGEvent(mouseEventSource: nil, mouseType: .leftMouseDragged, mouseCursorPosition: to, mouseButton: .left),
          let up = CGEvent(mouseEventSource: nil, mouseType: .leftMouseUp, mouseCursorPosition: to, mouseButton: .left)
    else { fail("could not create OS pointer events") }
    down.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.1)
    drag.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.1)
    up.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.3)
    print("dragged from \(from.x),\(from.y) to \(to.x),\(to.y)")
}

func typeSave(_ pid: pid_t, _ text: String) {
    let escaped = escapeForAppleScript(text)
    runAppleScript("""
    tell application "System Events"
        tell first process whose unix id is \(pid)
            set frontmost to true
            delay 0.1
            keystroke "\(escaped)"
            delay 0.2
            keystroke "s" using command down
            delay 0.3
        end tell
    end tell
    """)
}

func moveDocStart(_ pid: pid_t) {
    runAppleScript("""
    tell application "System Events"
        tell first process whose unix id is \(pid)
            set frontmost to true
            delay 0.1
            key code 126 using command down
            delay 0.2
        end tell
    end tell
    """)
}

func moveCaret(_ pid: pid_t, _ direction: String, _ count: Int) {
    let codes = ["left": 123, "right": 124, "down": 125, "up": 126]
    guard let code = codes[direction] else { fail("direction must be left, right, up, or down") }
    guard count > 0 else { fail("move-caret count must be positive") }
    runAppleScript("""
    tell application "System Events"
        tell first process whose unix id is \(pid)
            set frontmost to true
            delay 0.1
            repeat \(count) times
                key code \(code)
                delay 0.1
            end repeat
        end tell
    end tell
    """)
}

func shiftSelect(_ pid: pid_t, _ direction: String, _ count: Int) {
    guard direction == "left" || direction == "right" else { fail("direction must be left or right") }
    guard count > 0 else { fail("shift-select count must be positive") }
    let code = direction == "right" ? 124 : 123
    runAppleScript("""
    tell application "System Events"
        tell first process whose unix id is \(pid)
            set frontmost to true
            delay 0.1
            repeat \(count) times
                key code \(code) using shift down
                delay 0.1
            end repeat
        end tell
    end tell
    """)
}

func deleteSelectionSave(_ pid: pid_t) {
    runAppleScript("""
    tell application "System Events"
        tell first process whose unix id is \(pid)
            set frontmost to true
            delay 0.1
            key code 51
            delay 0.2
            keystroke "s" using command down
            delay 0.3
        end tell
    end tell
    """)
}

func endDocTypeSave(_ pid: pid_t, _ text: String) {
    let escaped = escapeForAppleScript(text)
    runAppleScript("""
    tell application "System Events"
        tell first process whose unix id is \(pid)
            set frontmost to true
            delay 0.1
            key code 125 using command down
            delay 0.2
            keystroke "\(escaped)"
            delay 0.2
            keystroke "s" using command down
            delay 0.3
        end tell
    end tell
    """)
}

func scrollEditor(_ pid: pid_t, _ pixels: Int32) {
    let bounds = windowBounds(pid)
    runAppleScript("tell application \"System Events\" to set frontmost of first process whose unix id is \(pid) to true")
    Thread.sleep(forTimeInterval: 0.3)
    let point = CGPoint(x: bounds.midX, y: bounds.midY)
    guard let down = CGEvent(mouseEventSource: nil, mouseType: .leftMouseDown, mouseCursorPosition: point, mouseButton: .left),
          let up = CGEvent(mouseEventSource: nil, mouseType: .leftMouseUp, mouseCursorPosition: point, mouseButton: .left),
          let wheel = CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 1, wheel1: pixels, wheel2: 0, wheel3: 0)
    else { fail("could not create OS pointer events") }
    down.post(tap: .cghidEventTap)
    up.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.2)
    wheel.location = point
    wheel.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.7)
    print("OS wheel at \(point.x),\(point.y), pixels=\(pixels)")
}

let arguments = Array(CommandLine.arguments.dropFirst())
guard let command = arguments.first else {
    fail("usage: hosted_gui_interaction.swift <ocr|image-digest|wheel|current-source|list-sources|select-source|select-all-type-save|undo-save|redo-save|type-romaji-commit-save|type-romaji-at-caret-commit-save|click-text|drag-select-text|type-save|move-doc-start|move-caret|shift-select|delete-selection-save|end-doc-type-save> ...")
}

switch command {
case "ocr":
    guard arguments.count == 2 else { fail("ocr requires screenshot path") }
    recognizeText(arguments[1])
case "image-digest":
    guard arguments.count == 2 else { fail("image-digest requires screenshot path") }
    imagePixelDigest(arguments[1])
case "wheel":
    guard arguments.count == 3, let pid = pid_t(arguments[1]), let pixels = Int32(arguments[2]) else { fail("wheel requires PID and pixels") }
    scrollEditor(pid, pixels)
case "current-source":
    print(currentSourceID())
case "list-sources":
    for id in inputSources().compactMap({ sourceID($0) }) { print(id) }
case "select-source":
    guard arguments.count == 2 else { fail("select-source requires an input source id") }
    selectSource(arguments[1])
case "select-all-type-save":
    guard arguments.count == 3, let pid = pid_t(arguments[1]) else { fail("select-all-type-save requires PID and text") }
    selectAllTypeSave(pid, arguments[2])
case "append-save":
    guard arguments.count == 3, let pid = pid_t(arguments[1]) else { fail("append-save requires PID and text") }
    appendSave(pid, arguments[2])
case "undo-save":
    guard arguments.count == 2, let pid = pid_t(arguments[1]) else { fail("undo-save requires PID") }
    undoSave(pid)
case "redo-save":
    guard arguments.count == 2, let pid = pid_t(arguments[1]) else { fail("redo-save requires PID") }
    redoSave(pid)
case "type-romaji-commit-save":
    guard arguments.count == 4, let pid = pid_t(arguments[1]) else { fail("type-romaji-commit-save requires PID, romaji text and source ID") }
    selectAllTypeRomajiCommitSave(pid, arguments[2], arguments[3])
case "type-romaji-at-caret-commit-save":
    guard arguments.count == 4, let pid = pid_t(arguments[1]) else { fail("type-romaji-at-caret-commit-save requires PID, romaji text and source ID") }
    typeRomajiAtCaretCommitSave(pid, arguments[2], arguments[3])
case "click-text":
    guard arguments.count == 5, let pid = pid_t(arguments[1]) else { fail("click-text requires PID, screenshot path, regex pattern and edge") }
    clickText(pid, arguments[2], arguments[3], arguments[4])
case "drag-select-text":
    guard arguments.count == 7, let pid = pid_t(arguments[1]) else { fail("drag-select-text requires PID, screenshot path, pattern1, edge1, pattern2, edge2") }
    dragSelectText(pid, arguments[2], arguments[3], arguments[4], arguments[5], arguments[6])
case "type-save":
    guard arguments.count == 3, let pid = pid_t(arguments[1]) else { fail("type-save requires PID and text") }
    typeSave(pid, arguments[2])
case "move-doc-start":
    guard arguments.count == 2, let pid = pid_t(arguments[1]) else { fail("move-doc-start requires PID") }
    moveDocStart(pid)
case "move-caret":
    guard arguments.count == 4, let pid = pid_t(arguments[1]), let count = Int(arguments[3]) else { fail("move-caret requires PID, direction and count") }
    moveCaret(pid, arguments[2], count)
case "shift-select":
    guard arguments.count == 4, let pid = pid_t(arguments[1]), let count = Int(arguments[3]) else { fail("shift-select requires PID, direction and count") }
    shiftSelect(pid, arguments[2], count)
case "delete-selection-save":
    guard arguments.count == 2, let pid = pid_t(arguments[1]) else { fail("delete-selection-save requires PID") }
    deleteSelectionSave(pid)
case "end-doc-type-save":
    guard arguments.count == 3, let pid = pid_t(arguments[1]) else { fail("end-doc-type-save requires PID and text") }
    endDocTypeSave(pid, arguments[2])
default:
    fail("unknown command: \(command)")
}
