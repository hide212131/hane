#!/usr/bin/env swift

import CoreGraphics
import Darwin
import Foundation
import ImageIO

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(2)
}

guard CommandLine.arguments.count == 3,
      let windowID = UInt32(CommandLine.arguments[1]) else {
    fail("usage: window_capture_dlsym.swift WINDOW_ID OUTPUT_PATH")
}

typealias CreateImage = @convention(c) (
    CGRect, CGWindowListOption, CGWindowID, CGWindowImageOption
) -> Unmanaged<CGImage>?

guard let symbol = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "CGWindowListCreateImage") else {
    fail("CGWindowListCreateImage symbol is unavailable")
}
let createImage = unsafeBitCast(symbol, to: CreateImage.self)
guard let image = createImage(
    .null,
    .optionIncludingWindow,
    CGWindowID(windowID),
    [.bestResolution, .boundsIgnoreFraming]
)?.takeRetainedValue() else {
    fail("could not capture window \(windowID)")
}

let outputURL = URL(fileURLWithPath: CommandLine.arguments[2])
guard let destination = CGImageDestinationCreateWithURL(
    outputURL as CFURL,
    "public.png" as CFString,
    1,
    nil
) else {
    fail("could not create PNG destination: \(outputURL.path)")
}
CGImageDestinationAddImage(destination, image, nil)
guard CGImageDestinationFinalize(destination) else {
    fail("could not write PNG: \(outputURL.path)")
}
