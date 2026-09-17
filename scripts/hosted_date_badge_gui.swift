#!/usr/bin/env swift

// Trusted, read-only Vision helper for the focused sidebar date-badge GUI
// procedure. It only observes screenshot text geometry; it never posts input.

import CoreGraphics
import Foundation
import ImageIO
import Vision

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(2)
}

func rectDictionary(_ rect: CGRect) -> [String: Double] {
    [
        "minX": Double(rect.minX),
        "maxX": Double(rect.maxX),
        "minY": Double(rect.minY),
        "maxY": Double(rect.maxY),
    ]
}

// Vision's own top-1 string (`topCandidates(1).first`) can misread a single
// tiny sidebar glyph (Issue #187: a weekday kanji, or a whole short filename)
// even when the rendered GUI is correct. Reading a bounded number of
// lower-ranked alternates per observation lets the validator recover that
// evidence without unbounded trust in OCR: each alternate is still only used
// through the same fail-closed, whole-line/geometry checks as before, and
// only a handful of alternates are considered per observation.
let maxCandidatesPerObservation = 3

// The hosted sidebar renders very small text (e.g. a single weekday kanji
// glyph) into a modest full-window screenshot. At that raw resolution
// Vision's accurate recognizer can fail to surface the correct glyph in any
// bounded candidate at all -- not just top-1 (Issue #190) -- so no amount of
// downstream candidate/geometry logic can recover it. Feeding Vision a
// deterministic, uniformly-upscaled copy of the *entire* screenshot gives it
// more source pixels per glyph while changing nothing else: no cropping, no
// offset, no aspect-ratio change. Every normalized (0...1) bounding box
// Vision reports is already a fraction of whichever image it analyzed, so
// scaling both image dimensions by the same integer factor leaves every
// candidate's normalized coordinates numerically identical to what they
// would be against the raw screenshot -- no coordinate re-projection back to
// "original" space is needed.
let ocrUpscaleFactor = 4

// Pure geometry, independent of Vision/CoreGraphics image decoding so it can
// be exercised by a deterministic test: draws `image` into a new bitmap
// scaled by `factor` in both dimensions with no cropping or offset, so every
// point's fractional position within the frame -- and therefore the
// normalized coordinates Vision will report for it -- is unchanged. Returns
// `nil` only on bitmap allocation failure.
func uniformlyUpscaled(_ image: CGImage, factor: Int) -> CGImage? {
    precondition(factor >= 1, "OCR upscale factor must be a positive integer")
    let width = image.width * factor
    let height = image.height * factor
    guard let context = CGContext(
        data: nil,
        width: width,
        height: height,
        bitsPerComponent: 8,
        bytesPerRow: 0,
        space: CGColorSpaceCreateDeviceRGB(),
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else {
        return nil
    }
    context.interpolationQuality = .high
    context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
    return context.makeImage()
}

func loadCGImage(_ path: String) -> CGImage {
    guard let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
          let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
    else {
        fail("could not decode screenshot: \(path)")
    }
    return image
}

func findAllText(_ path: String, _ pattern: String) {
    // The raw screenshot on disk (`path`) remains the authoritative visual
    // evidence untouched by this helper; only the in-memory copy handed to
    // Vision below is upscaled.
    let sourceImage = loadCGImage(path)
    guard let processedImage = uniformlyUpscaled(sourceImage, factor: ocrUpscaleFactor) else {
        fail("could not allocate upscaled OCR input bitmap")
    }

    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    request.usesLanguageCorrection = false
    request.recognitionLanguages = ["en-US", "ja-JP"]
    do {
        try VNImageRequestHandler(cgImage: processedImage, options: [:]).perform([request])
    } catch {
        fail("OCR failed: \(error)")
    }
    guard let regex = try? NSRegularExpression(pattern: pattern) else {
        fail("invalid regex pattern: \(pattern)")
    }
    var matches: [[String: Any]] = []
    for (observationIndex, observation) in (request.results ?? []).enumerated() {
        let lineBox = rectDictionary(observation.boundingBox)
        for (rank, candidate) in observation.topCandidates(maxCandidatesPerObservation).enumerated() {
            let text = candidate.string
            let fullRange = NSRange(text.startIndex..<text.endIndex, in: text)
            for match in regex.matches(in: text, range: fullRange) {
                guard let range = Range(match.range, in: text),
                      let box = try? candidate.boundingBox(for: range)
                else { continue }
                let rect = box.boundingBox
                // observation.boundingBox covers the whole recognized text
                // line and is the same for every candidate of this
                // observation, whereas `rect` covers only this specific
                // candidate's regex match. The focused date-badge validator
                // needs both: exact filename cases use the match box, and
                // the intentionally truncated long-name case uses the whole
                // line so a badge overlapping later visible text/ellipsis
                // cannot be accepted merely because an early prefix matched.
                // Both boxes are normalized to `processedImage`, which is
                // numerically identical to normalizing against the raw
                // screenshot (see `ocrUpscaleFactor` above), so downstream
                // geometry checks need no awareness of this preprocessing.
                matches.append([
                    "matched_text": String(text[range]),
                    "recognized_line": text,
                    "bounding_box": rectDictionary(rect),
                    "line_bounding_box": lineBox,
                    "observation_index": observationIndex,
                    "candidate_rank": rank,
                    "confidence": Double(candidate.confidence),
                ])
            }
        }
    }
    // `preprocessing` lets callers keep the applied OCR input transform as
    // traceable evidence (method, scale factor, source/processed pixel
    // size) without having to re-derive or trust it implicitly.
    let payload: [String: Any] = [
        "matches": matches,
        "preprocessing": [
            "method": "uniform_upscale",
            "scale_factor": ocrUpscaleFactor,
            "source_size": ["width": sourceImage.width, "height": sourceImage.height],
            "processed_size": ["width": processedImage.width, "height": processedImage.height],
        ],
    ]
    guard let data = try? JSONSerialization.data(withJSONObject: payload),
          let json = String(data: data, encoding: .utf8)
    else {
        fail("could not encode OCR geometry as JSON")
    }
    print(json)
}

let arguments = Array(CommandLine.arguments.dropFirst())
guard arguments.count == 3, arguments[0] == "find-all" else {
    fail("usage: hosted_date_badge_gui.swift find-all <screenshot> <regex>")
}
findAllText(arguments[1], arguments[2])
