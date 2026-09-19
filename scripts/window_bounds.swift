import CoreGraphics
import Foundation

guard CommandLine.arguments.count == 2,
      let windowID = UInt32(CommandLine.arguments[1]) else {
    FileHandle.standardError.write(Data("usage: window_bounds.swift WINDOW_ID\n".utf8))
    exit(2)
}

let options: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
guard let windows = CGWindowListCopyWindowInfo(options, kCGNullWindowID)
        as? [[String: Any]],
      let row = windows.first(where: { ($0[kCGWindowNumber as String] as? UInt32) == windowID }),
      let dictionary = row[kCGWindowBounds as String] as? [String: Any],
      let bounds = CGRect(dictionaryRepresentation: dictionary as CFDictionary) else {
    exit(1)
}

print("\(Int(bounds.origin.x)),\(Int(bounds.origin.y)),\(Int(bounds.width)),\(Int(bounds.height))")
