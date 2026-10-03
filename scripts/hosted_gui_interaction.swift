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
import Darwin
import Foundation
import ScreenCaptureKit
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

func forceSave(_ pid: pid_t) {
    // Flushes any in-memory edit left over from a mutating AppleScript that
    // failed or timed out between its edit keystroke and its own save
    // keystroke, so baseline restoration compares against disk bytes that
    // actually reflect the app's current document state.
    runAppleScript("""
    tell application "System Events"
        tell first process whose unix id is \(pid)
            set frontmost to true
            delay 0.1
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

func typeRomajiAtCaret(_ pid: pid_t, _ romaji: String, _ inputSource: String, commit: Bool, save: Bool) {
    focus(pid)
    // The caller selects the source while the editor is unfocused. Selecting it
    // once more after focus is intentional: it binds the newly focused
    // NSTextInputContext to Kotoeri so a cancel/commit operation is a real IME
    // composition rather than a plain key sequence. The focused validator does
    // this only after its explicit deactivate/select evidence step.
    selectSource(inputSource)
    // A successful TISSelectInputSource/currentSourceID match does not by
    // itself guarantee the app's IME session is ready to convert keystrokes;
    // on a hosted runner the switch can still be settling. Re-check with a
    // bounded, deterministic poll instead of a single immediate check, and
    // require the match to still hold after an explicit settle delay before
    // typing (Issue #126 post-merge GUI run 35410838091: currentSourceID()
    // already reported the Japanese source, yet the romaji was saved
    // unconverted).
    let settleDeadline = Date().addingTimeInterval(3.0)
    while currentSourceID() != inputSource && Date() < settleDeadline {
        Thread.sleep(forTimeInterval: 0.1)
    }
    guard currentSourceID() == inputSource else {
        fail("input source did not become active in time: \(inputSource)")
    }
    Thread.sleep(forTimeInterval: 0.5)
    guard currentSourceID() == inputSource else {
        fail("input source became inactive again before typing: \(inputSource)")
    }
    let escaped = escapeForAppleScript(romaji)
    let finishAction = commit
        ? "key code 49\ndelay 0.5\nkey code 36"
        : "key code 53"
    let saveAction = save ? "keystroke \"s\" using command down\ndelay 0.3" : ""
    runAppleScript("""
    tell application "System Events"
        tell first process whose unix id is \(pid)
            set frontmost to true
            delay 0.3
            keystroke "\(escaped)"
            delay 0.5
            \(finishAction)
            delay 0.3
            \(saveAction)
        end tell
    end tell
    """)
}

func typeRomajiAtCaretCommitSave(_ pid: pid_t, _ romaji: String, _ inputSource: String) {
    typeRomajiAtCaret(pid, romaji, inputSource, commit: true, save: true)
}

func typeRomajiAtCaretCommit(_ pid: pid_t, _ romaji: String, _ inputSource: String) {
    typeRomajiAtCaret(pid, romaji, inputSource, commit: true, save: false)
}

func typeRomajiAtCaretCancelSave(_ pid: pid_t, _ romaji: String, _ inputSource: String) {
    typeRomajiAtCaret(pid, romaji, inputSource, commit: false, save: true)
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

// matchedText は正規表現が一致した部分文字列そのもの(bounding box の由来を追跡できる
// ように、行全体ではなく一致範囲のテキストを返す)。呼び出し側はこの bounding box の
// 端をそのまま source 境界の真値として扱わず、evidence として残すためだけに使う
// (Issue #137)。
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

func activateApplication(_ pid: pid_t) {
    guard let application = NSRunningApplication(processIdentifier: pid) else {
        fail("target application is not running: \(pid)")
    }
    _ = application.activate(options: [.activateAllWindows])
    runAppleScript("tell application \"System Events\" to set frontmost of first process whose unix id is \(pid) to true")
    Thread.sleep(forTimeInterval: 0.3)
}

func deactivateApplication() {
    // Change the global input source while the target editor is not the key
    // application. GPUI reactivates the key window's NSTextInputContext when
    // macOS publishes a keyboard-source change; doing that while Kotoeri is
    // being selected can block the target editor in AppKit's IME XPC path.
    runAppleScript("tell application \"Finder\" to activate")
    Thread.sleep(forTimeInterval: 0.3)
}

func focus(_ pid: pid_t) {
    activateApplication(pid)
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

// click-text の stdout は「OCR bounding box の端をそのまま source 境界の真値として使った」
// のか「そこから選んだ実 OS click 座標」なのかを Python 側の evidence として残せるよう、
// 1行の JSON で OCR 一致文字列・正規化 bounding box・window bounds・実クリック座標を返す
// (Issue #137)。この座標自体が意図した visual boundary を正しく指したかどうかは、この
// stdout だけでは確定できない。呼び出し側が probe 文字入力後の保存 byte という独立した
// 観測と突き合わせて初めて判定できる。
func clickText(_ pid: pid_t, _ screenshotPath: String, _ pattern: String, _ edge: String) {
    guard edge == "start" || edge == "end" else { fail("edge must be start or end") }
    let match = findTextMatch(screenshotPath, pattern)
    let bounds = windowBounds(pid)
    let point = screenPoint(bounds, match.box, edge)
    focus(pid)
    postClick(point)
    let evidence: [String: Any] = [
        "matched_text": match.matchedText,
        "bounding_box": [
            "minX": Double(match.box.minX), "maxX": Double(match.box.maxX),
            "minY": Double(match.box.minY), "maxY": Double(match.box.maxY),
        ],
        "window_bounds": [
            "x": Double(bounds.minX), "y": Double(bounds.minY),
            "width": Double(bounds.width), "height": Double(bounds.height),
        ],
        "click_point": ["x": Double(point.x), "y": Double(point.y)],
        "edge": edge,
        "pattern": pattern,
    ]
    guard let data = try? JSONSerialization.data(withJSONObject: evidence),
          let json = String(data: data, encoding: .utf8) else {
        fail("could not encode click-text evidence as JSON")
    }
    print(json)
}

func dragSelectText(_ pid: pid_t, _ screenshotPath: String, _ pattern1: String, _ edge1: String, _ pattern2: String, _ edge2: String) {
    guard edge1 == "start" || edge1 == "end", edge2 == "start" || edge2 == "end" else { fail("edge must be start or end") }
    let bounds = windowBounds(pid)
    let from = screenPoint(bounds, findTextMatch(screenshotPath, pattern1).box, edge1)
    let to = screenPoint(bounds, findTextMatch(screenshotPath, pattern2).box, edge2)
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

func pressKey(_ pid: pid_t, _ key: String, shift: Bool, save: Bool) {
    let keyCode: Int
    switch key {
    case "enter": keyCode = 36
    case "backspace": keyCode = 51
    default: fail("key must be enter or backspace")
    }
    let keyAction = shift ? "key code \(keyCode) using shift down" : "key code \(keyCode)"
    let saveAction = save ? "keystroke \"s\" using command down\ndelay 0.3" : ""
    runAppleScript("""
    tell application "System Events"
        tell first process whose unix id is \(pid)
            set frontmost to true
            delay 0.1
            \(keyAction)
            delay 0.2
            \(saveAction)
        end tell
    end tell
    """)
}

func focusEditor(_ pid: pid_t) {
    let bounds = windowBounds(pid)
    // Hane's input capture is focused by an editor-body mouse event. The
    // point is derived from the live window bounds rather than a fixed screen
    // coordinate; moveDocStart immediately relocates the caret by source
    // offset afterward, so this click is only a focus operation.
    let point = CGPoint(
        x: bounds.midX,
        y: bounds.minY + bounds.height * 0.5
    )
    focus(pid)
    postClick(point)
}

func moveDocStart(_ pid: pid_t) {
    focusEditor(pid)
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

func scrollUnit(_ name: String) -> CGScrollEventUnit {
    switch name {
    case "lines": return .line
    case "pixels": return .pixel
    default: fail("scroll unit must be lines or pixels")
    }
}

@discardableResult
func postScroll(_ pid: pid_t, _ unit: CGScrollEventUnit, _ delta: Int32) -> TimeInterval {
    let bounds = windowBounds(pid)
    guard let event = CGEvent(
        scrollWheelEvent2Source: nil,
        units: unit,
        wheelCount: 1,
        wheel1: delta,
        wheel2: 0,
        wheel3: 0
    ) else { fail("could not create OS scroll event") }
    event.location = CGPoint(x: bounds.midX, y: bounds.midY)
    let postedAt = monotonicSeconds()
    event.post(tap: .cghidEventTap)
    return postedAt
}

// Separate from `postScroll` above: that function's seconds-based return value
// is relied on by the existing wheel-capture/wheel-reversal scenarios, so this
// adds a raw-ticks variant for the measurement-only wheel-measure command
// instead of changing what postScroll reports (Issue #427).
@discardableResult
func postScrollTicks(_ pid: pid_t, _ unit: CGScrollEventUnit, _ delta: Int32) -> UInt64 {
    let bounds = windowBounds(pid)
    guard let event = CGEvent(
        scrollWheelEvent2Source: nil,
        units: unit,
        wheelCount: 1,
        wheel1: delta,
        wheel2: 0,
        wheel3: 0
    ) else { fail("could not create OS scroll event") }
    event.location = CGPoint(x: bounds.midX, y: bounds.midY)
    let postedTicks = monotonicTicks()
    event.post(tap: .cghidEventTap)
    return postedTicks
}

let machTimebaseInfo: mach_timebase_info_data_t = {
    var timebase = mach_timebase_info_data_t()
    guard mach_timebase_info(&timebase) == KERN_SUCCESS, timebase.denom != 0 else {
        fail("could not read mach clock timebase")
    }
    return timebase
}()

let machSecondsPerTick: Double = Double(machTimebaseInfo.numer) / Double(machTimebaseInfo.denom) / 1_000_000_000

func monotonicSeconds() -> TimeInterval {
    Double(mach_absolute_time()) * machSecondsPerTick
}

func monotonicTicks() -> UInt64 {
    mach_absolute_time()
}

final class WindowCapture: @unchecked Sendable {
    let filter: SCContentFilter
    let configuration: SCStreamConfiguration

    init(filter: SCContentFilter, configuration: SCStreamConfiguration) {
        self.filter = filter
        self.configuration = configuration
    }
}

final class WindowCaptureResult: @unchecked Sendable {
    private let lock = NSLock()
    private var capture: WindowCapture?
    private var image: CGImage?
    private var errorMessage: String?

    func finish(capture: WindowCapture? = nil, image: CGImage? = nil, error: String? = nil) {
        lock.lock()
        self.capture = capture
        self.image = image
        errorMessage = error
        lock.unlock()
    }

    func values() -> (WindowCapture?, CGImage?, String?) {
        lock.lock()
        defer { lock.unlock() }
        return (capture, image, errorMessage)
    }
}

func prepareWindowCapture(_ windowID: CGWindowID) -> (WindowCapture?, String?) {
    let result = WindowCaptureResult()
    let completed = DispatchSemaphore(value: 0)
    SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: true) { content, error in
        guard let content else {
            result.finish(error: error.map { String(describing: $0) } ?? "shareable window list unavailable")
            completed.signal()
            return
        }
        guard let window = content.windows.first(where: { $0.windowID == windowID }) else {
            result.finish(error: "target window \(windowID) is not available to ScreenCaptureKit")
            completed.signal()
            return
        }
        let filter = SCContentFilter(desktopIndependentWindow: window)
        let configuration = SCStreamConfiguration()
        configuration.width = max(1, Int(filter.contentRect.width * CGFloat(filter.pointPixelScale)))
        configuration.height = max(1, Int(filter.contentRect.height * CGFloat(filter.pointPixelScale)))
        result.finish(capture: WindowCapture(filter: filter, configuration: configuration))
        completed.signal()
    }
    guard completed.wait(timeout: .now() + 10) == .success else {
        return (nil, "timed out while preparing ScreenCaptureKit window capture")
    }
    let (capture, _, error) = result.values()
    return (capture, error)
}

struct CapturedWindowFrame {
    let image: CGImage
    let started: TimeInterval
    let completed: TimeInterval
}

func captureWindowImage(_ capture: WindowCapture) -> (CGImage?, String?) {
    let result = WindowCaptureResult()
    let completed = DispatchSemaphore(value: 0)
    SCScreenshotManager.captureImage(contentFilter: capture.filter,
                                     configuration: capture.configuration) { image, error in
        result.finish(image: image, error: error.map { String(describing: $0) })
        completed.signal()
    }
    guard completed.wait(timeout: .now() + 10) == .success else {
        return (nil, "timed out while capturing the target window")
    }
    let (_, image, error) = result.values()
    return (image, error)
}

func writeWindowImage(_ image: CGImage, path: String) {
    let representation = NSBitmapImageRep(cgImage: image)
    guard let png = representation.representation(using: .png, properties: [:]) else {
        fail("could not encode window screenshot: \(path)")
    }
    do {
        try png.write(to: URL(fileURLWithPath: path), options: .atomic)
    } catch {
        fail("could not write window screenshot \(path): \(error)")
    }
}

func milliseconds(_ seconds: TimeInterval) -> String {
    String(format: "%.3f", locale: Locale(identifier: "en_US_POSIX"), seconds * 1000)
}

func captureImageWithTimes(_ capture: WindowCapture) -> (CapturedWindowFrame?, String?) {
    let started = monotonicSeconds()
    let (image, error) = captureWindowImage(capture)
    let completed = monotonicSeconds()
    guard let image else { return (nil, error ?? "screenshot unavailable") }
    return (CapturedWindowFrame(image: image, started: started, completed: completed), nil)
}

struct CapturedWindowFrameTicks {
    let image: CGImage
    let startedTicks: UInt64
    let completedTicks: UInt64
}

// Raw-ticks counterpart of captureImageWithTimes, used only by wheel-measure
// so that its frame times are directly comparable (same mach clock, no
// seconds round-trip) with the product-side ticks it polls for (Issue #427).
func captureImageWithTimesTicks(_ capture: WindowCapture) -> (CapturedWindowFrameTicks?, String?) {
    let started = monotonicTicks()
    let (image, error) = captureWindowImage(capture)
    let completed = monotonicTicks()
    guard let image else { return (nil, error ?? "screenshot unavailable") }
    return (CapturedWindowFrameTicks(image: image, startedTicks: started, completedTicks: completed), nil)
}

func visibleLineNumbers(_ image: CGImage) -> [Int] {
    // `.fast` recognition with `["en-US"]` reproduced a Vision crash during
    // ScreenCaptureKit warm-up on a local macOS 26.6.2 run (Issue #427); a
    // separate offline probe against that one saved baseline image showed
    // `.accurate` with `["en-US", "ja-JP"]` does not crash there. That probe
    // ran outside this hosted GUI path, so it is evidence of a plausible
    // mitigation, not proof this configuration is crash-free during an
    // actual run — it is also already the configuration `recognizeText` and
    // `findTextMatch` use below, so this reuses their proven request instead
    // of a separately tuned one. If Vision still throws against a live
    // capture, the `catch` below must keep surfacing the raw error through
    // `fail` (exit 2) so the Python harness observes this as
    // measurement-unavailable/blocked, never as a product failure or a
    // silently empty/synthesized result.
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    request.usesLanguageCorrection = false
    request.recognitionLanguages = ["en-US", "ja-JP"]
    do {
        try VNImageRequestHandler(cgImage: image, options: [:]).perform([request])
    } catch {
        fail("could not inspect captured scroll frame: \(error)")
    }
    let pattern = try! NSRegularExpression(pattern: #"\bLINE\s+(\d+)\b"#, options: [.caseInsensitive])
    return (request.results ?? []).compactMap { observation in
        guard let candidate = observation.topCandidates(1).first else { return nil }
        let text = candidate.string as NSString
        let range = NSRange(location: 0, length: text.length)
        guard let match = pattern.firstMatch(in: candidate.string, range: range),
              let numberRange = Range(match.range(at: 1), in: candidate.string) else { return nil }
        return Int(candidate.string[numberRange])
    }.sorted()
}

func prepareWindowCaptureContext(_ windowID: CGWindowID) -> WindowCapture {
    // ScreenCaptureKit setup and first-use framework latency must stay outside
    // the timed input-to-frame interval.
    _ = NSApplication.shared
    let (preparedCapture, captureError) = prepareWindowCapture(windowID)
    guard captureError == nil else {
        fail("could not prepare ScreenCaptureKit: \(captureError ?? "unknown error")")
    }
    guard let capture = preparedCapture else { fail("ScreenCaptureKit returned no capture context") }
    let (warmFrame, warmError) = captureImageWithTimes(capture)
    guard let warmFrame else { fail("ScreenCaptureKit preflight failed: \(warmError ?? "unknown error")") }
    guard warmFrame.completed >= warmFrame.started else { fail("ScreenCaptureKit warmup timing was invalid") }
    // Warm Vision before any timed scroll probe so the first OCR request does
    // not consume the inertia window while analyzing a frame already captured.
    _ = visibleLineNumbers(warmFrame.image)
    return capture
}

func wheelCapture(_ pid: pid_t, _ unit: CGScrollEventUnit, _ delta: Int32,
                  _ windowID: CGWindowID, _ frameDirectory: String, _ frameDelays: [Int]) {
    let capture = prepareWindowCaptureContext(windowID)
    let commandStarted = monotonicSeconds()
    let eventPosted = postScroll(pid, unit, delta)
    guard eventPosted >= commandStarted else { fail("scroll event timing was invalid") }

    var frames: [(Int, CapturedWindowFrame)] = []
    for (index, delayMs) in frameDelays.enumerated() {
        let deadline = eventPosted + Double(delayMs) / 1000
        let remaining = deadline - monotonicSeconds()
        if remaining > 0 { Thread.sleep(forTimeInterval: remaining) }
        let (frame, error) = captureImageWithTimes(capture)
        guard let frame else {
            fail("could not capture scroll frame \(index): \(error ?? "unknown error")")
        }
        frames.append((index, frame))
    }

    try? FileManager.default.createDirectory(
        at: URL(fileURLWithPath: frameDirectory, isDirectory: true),
        withIntermediateDirectories: true
    )
    for (index, frame) in frames {
        let path = URL(fileURLWithPath: frameDirectory, isDirectory: true)
            .appendingPathComponent(String(format: "frame-%02d.png", index)).path
        writeWindowImage(frame.image, path: path)
    }
    print("event_route=cghidEventTap")
    print("event_post_elapsed_ms=\(milliseconds(eventPosted - commandStarted))")
    for (index, frame) in frames {
        print(String(format: "frame_%02d_capture_started_ms=", index) + milliseconds(frame.started - eventPosted))
        print(String(format: "frame_%02d_capture_completed_ms=", index) + milliseconds(frame.completed - eventPosted))
    }
}

/// Reads whatever Hane appended to the product scroll-timing file at `path`
/// since byte `offset`, and parses only the first full line found there.
/// A line only counts as "full" once it is non-empty and terminated by its
/// own `\n`; a partial append (no trailing newline yet) is treated the same
/// as nothing written yet, so the caller polls again instead of parsing a
/// truncated line. Anything not written yet, or written in a form this does
/// not recognize, is the caller's cue to treat the product side as
/// measurement-unavailable rather than guess at a value (Issue #427).
func readScrollEventTimingLine(_ path: String, since offset: UInt64) -> String? {
    guard let handle = FileHandle(forReadingAtPath: path) else { return nil }
    defer { try? handle.close() }
    do {
        try handle.seek(toOffset: offset)
    } catch {
        return nil
    }
    let data = handle.readDataToEndOfFile()
    guard !data.isEmpty, let text = String(data: data, encoding: .utf8) else { return nil }
    guard let newlineIndex = text.firstIndex(of: "\n") else { return nil }
    let line = text[text.startIndex..<newlineIndex]
    guard !line.isEmpty else { return nil }
    return String(line)
}

struct ProductScrollTiming {
    let receiptTicks: UInt64
    // Hane's own frame paint/submission time (`InputCapture::paint`), not
    // compositor presentation: the product process has no trustworthy
    // presentation timestamp on this clock, so it always writes
    // `scroll_frame_presented_ticks=unavailable` and this helper never reads
    // that field as a ticks value.
    let paintTicks: UInt64
    let timebaseNumer: UInt32
    let timebaseDenom: UInt32
}

func parseScrollEventTimingLine(_ line: String) -> ProductScrollTiming? {
    var fields: [String: String] = [:]
    for pair in line.split(separator: " ") {
        let parts = pair.split(separator: "=", maxSplits: 1)
        guard parts.count == 2 else { continue }
        fields[String(parts[0])] = String(parts[1])
    }
    guard let receiptTicks = fields["scroll_receipt_ticks"].flatMap(UInt64.init),
          let paintTicks = fields["scroll_frame_paint_ticks"].flatMap(UInt64.init),
          let numer = fields["mach_timebase_numer"].flatMap(UInt32.init),
          let denom = fields["mach_timebase_denom"].flatMap(UInt32.init) else {
        return nil
    }
    return ProductScrollTiming(receiptTicks: receiptTicks, paintTicks: paintTicks,
                               timebaseNumer: numer, timebaseDenom: denom)
}

// Measurement-only path (Issue #427): distinguishes the OS event post, Hane's
// ScrollWheelEvent receipt, the frame paint/submission Hane committed in
// response, and this helper's own screenshot capture start/end, all read
// from the one mach clock both processes share. Hane's own paint timestamp
// is not compositor presentation (see `ProductScrollTiming`/
// `instrument.rs::ScrollEventTimingOutput`), so true presentation is always
// reported unavailable rather than inferred from paint. It does not evaluate
// Issue #389's product thresholds (80ms/55ms/135ms) and must not be read as
// proof of their pass/fail; a separate observer judges only what this
// command actually measured.
func fileSizeOrZero(_ path: String) -> UInt64 {
    guard let attributes = try? FileManager.default.attributesOfItem(atPath: path),
          let size = attributes[.size] as? UInt64 else {
        return 0
    }
    return size
}

func wheelMeasure(_ pid: pid_t, _ unit: CGScrollEventUnit, _ delta: Int32,
                  _ windowID: CGWindowID, _ frameDirectory: String, _ frameDelays: [Int],
                  _ timingPath: String, _ pollTimeoutMs: Int) {
    let capture = prepareWindowCaptureContext(windowID)
    let timingOffsetBefore = fileSizeOrZero(timingPath)

    let eventPostedTicks = postScrollTicks(pid, unit, delta)
    let eventPostedSeconds = Double(eventPostedTicks) * machSecondsPerTick

    var frames: [(Int, CapturedWindowFrameTicks)] = []
    for (index, delayMs) in frameDelays.enumerated() {
        let deadline = eventPostedSeconds + Double(delayMs) / 1000
        let remaining = deadline - monotonicSeconds()
        if remaining > 0 { Thread.sleep(forTimeInterval: remaining) }
        let (frame, error) = captureImageWithTimesTicks(capture)
        guard let frame else {
            fail("could not capture scroll frame \(index): \(error ?? "unknown error")")
        }
        frames.append((index, frame))
    }

    // Polled only after every timed frame capture above, so waiting for the
    // product's record never itself delays or displaces a capture inside the
    // inertia window; a slow or missing product record only costs this final
    // poll budget.
    let pollDeadline = Date().addingTimeInterval(Double(pollTimeoutMs) / 1000)
    var productTiming: ProductScrollTiming?
    while productTiming == nil && Date() < pollDeadline {
        if let line = readScrollEventTimingLine(timingPath, since: timingOffsetBefore) {
            productTiming = parseScrollEventTimingLine(line)
        }
        if productTiming == nil { Thread.sleep(forTimeInterval: 0.01) }
    }

    try? FileManager.default.createDirectory(
        at: URL(fileURLWithPath: frameDirectory, isDirectory: true),
        withIntermediateDirectories: true
    )
    for (index, frame) in frames {
        let path = URL(fileURLWithPath: frameDirectory, isDirectory: true)
            .appendingPathComponent(String(format: "frame-%02d.png", index)).path
        writeWindowImage(frame.image, path: path)
    }

    print("event_route=cghidEventTap")
    print("event_post_ticks=\(eventPostedTicks)")
    print("mach_timebase_numer=\(machTimebaseInfo.numer)")
    print("mach_timebase_denom=\(machTimebaseInfo.denom)")
    if let productTiming {
        print("product_scroll_receipt_ticks=\(productTiming.receiptTicks)")
        print("product_frame_paint_ticks=\(productTiming.paintTicks)")
        // Hane never observes true compositor presentation on this clock;
        // always report it unavailable rather than inferring it from paint.
        print("product_frame_presented_ticks=unavailable")
        print("product_mach_timebase_numer=\(productTiming.timebaseNumer)")
        print("product_mach_timebase_denom=\(productTiming.timebaseDenom)")
    } else {
        print("product_scroll_receipt_ticks=unavailable")
        print("product_frame_paint_ticks=unavailable")
        print("product_frame_presented_ticks=unavailable")
        print("product_mach_timebase_numer=unavailable")
        print("product_mach_timebase_denom=unavailable")
    }
    for (index, frame) in frames {
        print(String(format: "frame_%02d_capture_started_ticks=", index) + String(frame.startedTicks))
        print(String(format: "frame_%02d_capture_completed_ticks=", index) + String(frame.completedTicks))
    }
}

func wheelReversal(_ pid: pid_t, _ unit: CGScrollEventUnit, _ delta: Int32,
                   _ reverseDelta: Int32, _ preProbeDelaysMs: [Int], _ windowID: CGWindowID,
                   _ preFrameDirectory: String, _ frameDirectory: String, _ frameDelays: [Int]) {
    let capture = prepareWindowCaptureContext(windowID)

    let commandStarted = monotonicSeconds()
    // Keep both reversal-probe inputs on the same global Quartz event path as
    // the other GUI scenarios, including normal window-server target routing.
    let firstPosted = postScroll(pid, unit, delta)

    // Capture every pre-reversal candidate frame before sending the reverse
    // input. A single candidate can miss the old-direction motion depending
    // on exactly when it lands, so multiple candidates are captured and the
    // reverse input is sent right after the latest one. Vision OCR does not
    // run in this loop: it can take longer than the remaining Lines coast
    // and would postpone the reverse input itself.
    var preFrames: [(Int, CapturedWindowFrame)] = []
    for (index, delayMs) in preProbeDelaysMs.enumerated() {
        let deadline = firstPosted + Double(delayMs) / 1000
        let remaining = deadline - monotonicSeconds()
        if remaining > 0 { Thread.sleep(forTimeInterval: remaining) }
        let (frame, error) = captureImageWithTimes(capture)
        guard let frame else {
            fail("could not capture pre-reversal window frame \(index): \(error ?? "unknown error")")
        }
        let startedMs = milliseconds(frame.started - firstPosted)
        let completedMs = milliseconds(frame.completed - firstPosted)
        guard frame.completed < firstPosted + 0.135 else {
            fail("pre-reversal frame \(index) started at \(startedMs)ms and completed at \(completedMs)ms; the Lines inertia deadline is 135ms")
        }
        preFrames.append((index, frame))
    }
    let reversePosted = postScroll(pid, unit, reverseDelta)

    var frames: [(Int, CapturedWindowFrame)] = []
    for (index, delayMs) in frameDelays.enumerated() {
        let deadline = reversePosted + Double(delayMs) / 1000
        let remaining = deadline - monotonicSeconds()
        if remaining > 0 { Thread.sleep(forTimeInterval: remaining) }
        let (frame, error) = captureImageWithTimes(capture)
        guard let frame else {
            fail("could not capture post-reversal window frame \(index): \(error ?? "unknown error")")
        }
        frames.append((index, frame))
    }

    // OCR is intentionally after the reverse event and every timed capture
    // (pre- and post-reversal); all frames were captured before this point,
    // so this preserves the evidence ordering without spending the inertia
    // window on recognition. The caller runs OCR on the written images.
    try? FileManager.default.createDirectory(
        at: URL(fileURLWithPath: preFrameDirectory, isDirectory: true),
        withIntermediateDirectories: true
    )
    for (index, frame) in preFrames {
        let path = URL(fileURLWithPath: preFrameDirectory, isDirectory: true)
            .appendingPathComponent(String(format: "pre-frame-%02d.png", index)).path
        writeWindowImage(frame.image, path: path)
    }
    try? FileManager.default.createDirectory(
        at: URL(fileURLWithPath: frameDirectory, isDirectory: true),
        withIntermediateDirectories: true
    )
    for (index, frame) in frames {
        let path = URL(fileURLWithPath: frameDirectory, isDirectory: true)
            .appendingPathComponent(String(format: "frame-%02d.png", index)).path
        writeWindowImage(frame.image, path: path)
    }

    print("initial_event_elapsed_ms=\(milliseconds(firstPosted - commandStarted))")
    print("reversal_event_route=cghidEventTap")
    print("reverse_event_elapsed_ms=\(milliseconds(reversePosted - commandStarted))")
    for (index, frame) in preFrames {
        print(String(format: "pre_frame_%02d_capture_started_ms=", index) + milliseconds(frame.started - commandStarted))
        print(String(format: "pre_frame_%02d_capture_completed_ms=", index) + milliseconds(frame.completed - commandStarted))
    }
    for (index, frame) in frames {
        print(String(format: "frame_%02d_capture_started_ms=", index) + milliseconds(frame.started - reversePosted))
        print(String(format: "frame_%02d_capture_completed_ms=", index) + milliseconds(frame.completed - reversePosted))
    }
}

let arguments = Array(CommandLine.arguments.dropFirst())
guard let command = arguments.first else {
    fail("usage: hosted_gui_interaction.swift <ocr|image-digest|wheel|wheel-event|wheel-capture|wheel-measure|wheel-reversal|focus-editor|current-source|list-sources|select-source|activate|deactivate|select-all-type-save|undo-save|redo-save|force-save|type-romaji-commit-save|type-romaji-at-caret-commit-save|type-romaji-at-caret-commit|type-romaji-at-caret-cancel-save|click-text|drag-select-text|type-save|press-key|move-doc-start|move-caret|shift-select|delete-selection-save|end-doc-type-save> ...")
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
case "focus-editor":
    guard arguments.count == 2, let pid = pid_t(arguments[1]) else { fail("focus-editor requires PID") }
    focusEditor(pid)
case "wheel-event":
    guard arguments.count == 4,
          let pid = pid_t(arguments[1]),
          let delta = Int32(arguments[3]) else { fail("wheel-event requires PID, lines|pixels and delta") }
    postScroll(pid, scrollUnit(arguments[2]), delta)
case "wheel-capture":
    guard arguments.count == 7,
          let pid = pid_t(arguments[1]),
          let delta = Int32(arguments[3]),
          let windowID = UInt32(arguments[4]) else {
        fail("wheel-capture requires PID, lines|pixels, delta, window ID, frame directory and comma-separated delays")
    }
    let frameDelays = arguments[6].split(separator: ",").compactMap { Int($0) }
    guard !frameDelays.isEmpty,
          frameDelays.first == 0,
          frameDelays == frameDelays.sorted(),
          frameDelays.allSatisfy({ $0 >= 0 && $0 <= 1000 }) else {
        fail("wheel-capture frame delays must be ascending milliseconds beginning at 0")
    }
    wheelCapture(pid, scrollUnit(arguments[2]), delta, windowID, arguments[5], frameDelays)
case "wheel-measure":
    guard arguments.count == 9,
          let pid = pid_t(arguments[1]),
          let delta = Int32(arguments[3]),
          let windowID = UInt32(arguments[4]),
          let pollTimeoutMs = Int(arguments[8]) else {
        fail("wheel-measure requires PID, lines|pixels, delta, window ID, frame directory, comma-separated delays, product timing path and poll timeout ms")
    }
    let measureFrameDelays = arguments[6].split(separator: ",").compactMap { Int($0) }
    guard !measureFrameDelays.isEmpty,
          measureFrameDelays.first == 0,
          measureFrameDelays == measureFrameDelays.sorted(),
          measureFrameDelays.allSatisfy({ $0 >= 0 && $0 <= 1000 }) else {
        fail("wheel-measure frame delays must be ascending milliseconds beginning at 0")
    }
    wheelMeasure(pid, scrollUnit(arguments[2]), delta, windowID, arguments[5], measureFrameDelays,
                 arguments[7], pollTimeoutMs)
case "wheel-reversal":
    guard arguments.count == 10,
          let pid = pid_t(arguments[1]),
          let delta = Int32(arguments[3]),
          let reverseDelta = Int32(arguments[4]),
          let windowID = UInt32(arguments[6]) else {
        fail("wheel-reversal requires PID, lines|pixels, delta, reverse delta, comma-separated pre-reverse probe delays ms, window ID, pre-frame directory, frame directory and comma-separated delays")
    }
    let preProbeDelays = arguments[5].split(separator: ",").compactMap { Int($0) }
    guard !preProbeDelays.isEmpty,
          preProbeDelays == preProbeDelays.sorted(),
          preProbeDelays.allSatisfy({ $0 > 0 && $0 <= 130 }) else {
        fail("wheel-reversal pre-reverse probe delays must be ascending milliseconds within the Lines inertia window")
    }
    let frameDelays = arguments[9].split(separator: ",").compactMap { Int($0) }
    guard !frameDelays.isEmpty,
          frameDelays.first == 0,
          frameDelays == frameDelays.sorted(),
          frameDelays.allSatisfy({ $0 >= 0 && $0 <= 1000 }) else {
        fail("wheel-reversal frame delays must be ascending milliseconds beginning at 0")
    }
    wheelReversal(pid, scrollUnit(arguments[2]), delta, reverseDelta, preProbeDelays,
                  windowID, arguments[7], arguments[8], frameDelays)
case "current-source":
    print(currentSourceID())
case "activate":
    guard arguments.count == 2, let pid = pid_t(arguments[1]) else { fail("activate requires PID") }
    activateApplication(pid)
case "deactivate":
    guard arguments.count == 1 else { fail("deactivate takes no arguments") }
    deactivateApplication()
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
case "force-save":
    guard arguments.count == 2, let pid = pid_t(arguments[1]) else { fail("force-save requires PID") }
    forceSave(pid)
case "type-romaji-commit-save":
    guard arguments.count == 4, let pid = pid_t(arguments[1]) else { fail("type-romaji-commit-save requires PID, romaji text and source ID") }
    selectAllTypeRomajiCommitSave(pid, arguments[2], arguments[3])
case "type-romaji-at-caret-commit-save":
    guard arguments.count == 4, let pid = pid_t(arguments[1]) else { fail("type-romaji-at-caret-commit-save requires PID, romaji text and source ID") }
    typeRomajiAtCaretCommitSave(pid, arguments[2], arguments[3])
case "type-romaji-at-caret-commit":
    guard arguments.count == 4, let pid = pid_t(arguments[1]) else { fail("type-romaji-at-caret-commit requires PID, romaji text and source ID") }
    typeRomajiAtCaretCommit(pid, arguments[2], arguments[3])
case "type-romaji-at-caret-cancel-save":
    guard arguments.count == 4, let pid = pid_t(arguments[1]) else { fail("type-romaji-at-caret-cancel-save requires PID, romaji text and source ID") }
    typeRomajiAtCaretCancelSave(pid, arguments[2], arguments[3])
case "click-text":
    guard arguments.count == 5, let pid = pid_t(arguments[1]) else { fail("click-text requires PID, screenshot path, regex pattern and edge") }
    clickText(pid, arguments[2], arguments[3], arguments[4])
case "drag-select-text":
    guard arguments.count == 7, let pid = pid_t(arguments[1]) else { fail("drag-select-text requires PID, screenshot path, pattern1, edge1, pattern2, edge2") }
    dragSelectText(pid, arguments[2], arguments[3], arguments[4], arguments[5], arguments[6])
case "type-save":
    guard arguments.count == 3, let pid = pid_t(arguments[1]) else { fail("type-save requires PID and text") }
    typeSave(pid, arguments[2])
case "press-key":
    guard arguments.count == 4, let pid = pid_t(arguments[1]) else { fail("press-key requires PID, key and save flag") }
    let save = arguments[3] == "save"
    guard save || arguments[3] == "nosave" else { fail("press-key save flag must be save or nosave") }
    pressKey(pid, arguments[2], shift: false, save: save)
case "press-shift-enter":
    guard arguments.count == 3, let pid = pid_t(arguments[1]) else { fail("press-shift-enter requires PID and save flag") }
    let save = arguments[2] == "save"
    guard save || arguments[2] == "nosave" else { fail("press-shift-enter save flag must be save or nosave") }
    pressKey(pid, "enter", shift: true, save: save)
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
