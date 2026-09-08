#!/usr/bin/env swift

// Minimal OS-level input helper for the hosted GUI interaction spike
// (scripts/hosted_gui_interaction.py). Deliberately separate from the
// pinned target's scripts/phase0_input.swift: that file belongs to the
// checked-out target commit and is not edited by this procedure. Every
// keystroke here goes through System Events (synthesized OS key events), never
// direct Unicode insertion, so it does not misrepresent what was proven.

import AppKit
import Carbon
import Foundation

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
        kTISPropertyInputSourceIsSelectCapable!: true as CFBoolean,
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

// Select-all, type an ASCII string via synthesized OS key events, then Cmd-S.
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

// Select-all, type romaji via synthesized OS key events so the active IME (not this
// script) performs the conversion, confirm the candidate with space, commit
// with return, then Cmd-S. This proves OS-input IME behavior; it does
// not insert Japanese Unicode text directly.
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

let arguments = Array(CommandLine.arguments.dropFirst())
guard let command = arguments.first else {
    fail("usage: hosted_gui_interaction.swift <current-source|list-sources|select-source|select-all-type-save|undo-save|redo-save|type-romaji-commit-save> ...")
}

switch command {
case "current-source":
    print(currentSourceID())
case "list-sources":
    for id in inputSources().compactMap({ sourceID($0) }) { print(id) }
case "select-source":
    guard arguments.count == 2 else { fail("select-source requires an input source id") }
    selectSource(arguments[1])
case "select-all-type-save":
    guard arguments.count == 3, let pid = pid_t(arguments[1]) else {
        fail("select-all-type-save requires PID and text")
    }
    selectAllTypeSave(pid, arguments[2])
case "undo-save":
    guard arguments.count == 2, let pid = pid_t(arguments[1]) else { fail("undo-save requires PID") }
    undoSave(pid)
case "redo-save":
    guard arguments.count == 2, let pid = pid_t(arguments[1]) else { fail("redo-save requires PID") }
    redoSave(pid)
case "type-romaji-commit-save":
    guard arguments.count == 4, let pid = pid_t(arguments[1]) else {
        fail("type-romaji-commit-save requires PID, romaji text and source ID")
    }
    selectAllTypeRomajiCommitSave(pid, arguments[2], arguments[3])
default:
    fail("unknown command: \(command)")
}
