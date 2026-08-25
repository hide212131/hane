#!/usr/bin/env swift

import AppKit
import Carbon
import Foundation

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(2)
}

func inputSources() -> [TISInputSource] {
    let properties = [
        kTISPropertyInputSourceCategory!: kTISCategoryKeyboardInputSource as Any,
        kTISPropertyInputSourceIsSelectCapable!: true as CFBoolean,
    ] as CFDictionary
    return TISCreateInputSourceList(properties, false).takeRetainedValue() as! [TISInputSource]
}

func sourceID(_ source: TISInputSource) -> String? {
    guard let pointer = TISGetInputSourceProperty(source, kTISPropertyInputSourceID) else { return nil }
    return Unmanaged<CFString>.fromOpaque(pointer).takeUnretainedValue() as String
}

func selectSource(_ id: String) {
    guard let source = inputSources().first(where: { sourceID($0) == id }) else {
        fail("input source not found: \(id)")
    }
    let status = TISSelectInputSource(source)
    guard status == noErr else { fail("could not select input source \(id): \(status)") }
}

func automate(_ pid: pid_t, count: Int, ime: Bool) {
    let action = ime
        ? "keystroke \"nihongo\"\ndelay 0.08\nkey code 36"
        : "keystroke \"a\""
    let source = """
    tell application "System Events"
        tell first process whose unix id is \(pid)
            set frontmost to true
            repeat \(count) times
                \(action)
                delay 0.022
            end repeat
        end tell
    end tell
    """
    var error: NSDictionary?
    NSAppleScript(source: source)?.executeAndReturnError(&error)
    if let error { fail("System Events automation failed: \(error)") }
}

let arguments = Array(CommandLine.arguments.dropFirst())
guard let command = arguments.first else { fail("usage: phase0_input.swift <current-source|select-source|ascii|ime|scroll|scroll-input> ...") }

switch command {
case "current-source":
    guard let id = sourceID(TISCopyCurrentKeyboardInputSource().takeRetainedValue()) else {
        fail("current input source has no identifier")
    }
    print(id)
case "select-source":
    guard arguments.count == 2 else { fail("select-source requires an input source id") }
    selectSource(arguments[1])
case "ascii", "ime", "scroll", "scroll-input":
    guard arguments.count == 3, let pid = pid_t(arguments[1]), let count = Int(arguments[2]) else {
        fail("\(command) requires PID and count")
    }
    if command == "ascii" || command == "scroll-input" {
        automate(pid, count: count, ime: false)
    } else if command == "ime" {
        automate(pid, count: count, ime: true)
    } else {
        Thread.sleep(forTimeInterval: Double(count) * 0.025)
    }
default:
    fail("unknown command: \(command)")
}
