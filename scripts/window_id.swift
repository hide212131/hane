import CoreGraphics
import Foundation

guard CommandLine.arguments.count == 2,
      let processID = Int32(CommandLine.arguments[1]) else {
    FileHandle.standardError.write(Data("usage: window_id.swift PID\n".utf8))
    exit(2)
}

let options: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
guard let windows = CGWindowListCopyWindowInfo(options, kCGNullWindowID)
        as? [[String: Any]] else {
    exit(1)
}

for window in windows {
    let ownerPID = window[kCGWindowOwnerPID as String] as? Int32
    let layer = window[kCGWindowLayer as String] as? Int
    let windowID = window[kCGWindowNumber as String] as? UInt32
    if ownerPID == processID, layer == 0, let windowID {
        print(windowID)
        exit(0)
    }
}

exit(1)
