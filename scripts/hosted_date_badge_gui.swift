#!/usr/bin/env swift

// Trusted, read-only Vision helper for the focused sidebar date-badge GUI
// procedure. It only observes screenshot text geometry; it never posts input.

import Foundation
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

func findAllText(_ path: String, _ pattern: String) {
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    request.usesLanguageCorrection = false
    request.recognitionLanguages = ["en-US", "ja-JP"]
    do {
        try VNImageRequestHandler(url: URL(fileURLWithPath: path), options: [:]).perform([request])
    } catch {
        fail("OCR failed: \(error)")
    }
    guard let regex = try? NSRegularExpression(pattern: pattern) else {
        fail("invalid regex pattern: \(pattern)")
    }
    var output: [[String: Any]] = []
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
                output.append([
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
    guard let data = try? JSONSerialization.data(withJSONObject: output),
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
