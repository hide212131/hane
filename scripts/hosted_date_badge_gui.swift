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
// bounded candidate at all -- not just top-1 (Issue #190). A uniform upscale
// of the *entire* screenshot (procedure v4) did not fix this: scaling every
// dimension by the same factor leaves each glyph's share of the analyzed
// frame unchanged, and PR #175 run #35286639940 confirmed a 4x full-frame
// upscale still returned zero filename/badge candidates. The sidebar rows
// this validator inspects occupy only a small, known region of the raw
// screenshot, so cropping to that region *before* upscaling multiplies the
// glyph's share of the analyzed frame by roughly
// `1 / (widthFraction * heightFraction)`, independent of any upscale factor.
//
// `SidebarROI` is a fixed, trusted contract (top-left origin, y-down,
// fraction of the raw screenshot) -- not read from target product code at
// runtime -- so this validator's pass/fail behavior cannot be steered by
// changes to the app under test. The values are chosen from PR #175 run
// #35286639940 raw-screenshot facts (960x681): the sidebar divider sits at
// about x=222px (~0.231 of width) and the fixture rows span about
// y=74..205px (~0.109..0.301 of height). Every edge below has margin beyond
// those observed facts so the crop keeps the whole sidebar rows region and
// the badges' right edge even if the hosted window size varies slightly.
// Must match `EXPECTED_ROI` in hosted_date_badge_gui.py.
enum SidebarROI {
    static let originContract = "top_left_y_down"
    static let xFraction = 0.0
    static let yFractionFromTop = 0.05
    static let widthFraction = 0.30
    static let heightFraction = 0.35
}

// Pure geometry, independent of Vision/CoreGraphics image decoding: the
// pixel rectangle (top-left origin, y-down) of `SidebarROI` within a raw
// screenshot of the given size. `CGImage.cropping(to:)` interprets its rect
// in that same top-left-origin, y-down pixel space (unlike `CGContext`,
// which is bottom-left-origin) -- this is the "pixel+origin contract" this
// helper commits to for the crop step.
func sidebarROICropRect(imageWidth: Double, imageHeight: Double) -> CGRect {
    CGRect(
        x: CGFloat((SidebarROI.xFraction * imageWidth).rounded()),
        y: CGFloat((SidebarROI.yFractionFromTop * imageHeight).rounded()),
        width: CGFloat((SidebarROI.widthFraction * imageWidth).rounded()),
        height: CGFloat((SidebarROI.heightFraction * imageHeight).rounded())
    )
}

// Pure geometry: reprojects a normalized rectangle Vision reported against
// the cropped-and-upscaled OCR input image back into the raw screenshot's
// own normalized coordinate space, so downstream same-row / strictly-right /
// non-overlap checks keep comparing every candidate in one consistent frame,
// exactly as procedure v4 did before this ROI crop existed.
//
// Vision's own `boundingBox`/`candidate.boundingBox(for:)` are normalized
// with a bottom-left origin, y increasing upward (the CoreGraphics/Vision
// convention), regardless of whether the analyzed image is the raw
// screenshot or this crop. `cropPixelRect`, by contrast, is in
// `sidebarROICropRect`'s top-left-origin, y-down pixel space. This function
// explicitly bridges the two origins instead of leaving that mismatch
// implicit. The upscale step needs no separate correction here: scaling both
// crop dimensions by the same integer factor leaves every candidate's
// Vision-normalized fraction of the crop numerically unchanged (the same
// reasoning procedure v4 used for the whole-frame upscale). Both reprojection
// axes are affine and monotonically increasing in their input, so `minX`/
// `minY` always reproject to the smaller output and `maxX`/`maxY` to the
// larger one -- no min/max swap is needed despite the y-origin flip.
func reprojectToFullWindow(_ rect: CGRect, cropPixelRect: CGRect, sourceWidth: Double, sourceHeight: Double) -> CGRect {
    let cropXFraction = Double(cropPixelRect.minX) / sourceWidth
    let cropWidthFraction = Double(cropPixelRect.width) / sourceWidth
    let cropYFractionFromTop = Double(cropPixelRect.minY) / sourceHeight
    let cropHeightFraction = Double(cropPixelRect.height) / sourceHeight

    // x does not cross an origin flip: both frames measure x left-to-right.
    func reprojectX(_ x: Double) -> Double { cropXFraction + x * cropWidthFraction }
    // y crosses the top-left/bottom-left origin flip described above. This
    // is still a single affine map; the flip only changes its constant term.
    func reprojectY(_ y: Double) -> Double {
        (1.0 - cropYFractionFromTop - cropHeightFraction) + y * cropHeightFraction
    }

    let minX = reprojectX(Double(rect.minX))
    let maxX = reprojectX(Double(rect.maxX))
    let minY = reprojectY(Double(rect.minY))
    let maxY = reprojectY(Double(rect.maxY))
    return CGRect(x: CGFloat(minX), y: CGFloat(minY), width: CGFloat(maxX - minX), height: CGFloat(maxY - minY))
}

// Applied to the ROI crop (not the whole screenshot, see `SidebarROI` above).
// Every normalized (0...1) bounding box Vision reports is already a fraction
// of whichever image it analyzed, so scaling both crop dimensions by the
// same integer factor leaves every candidate's normalized coordinates
// *relative to the crop* numerically unchanged -- `reprojectToFullWindow`
// still needs to run once, for the crop offset, not once per upscale factor.
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
    // evidence untouched by this helper; only an in-memory crop of it is
    // cropped and upscaled for Vision below.
    let sourceImage = loadCGImage(path)
    let sourceWidth = Double(sourceImage.width)
    let sourceHeight = Double(sourceImage.height)
    let cropPixelRect = sidebarROICropRect(imageWidth: sourceWidth, imageHeight: sourceHeight)
    guard let croppedImage = sourceImage.cropping(to: cropPixelRect) else {
        fail("could not crop screenshot to the trusted sidebar ROI: \(cropPixelRect)")
    }
    guard let processedImage = uniformlyUpscaled(croppedImage, factor: ocrUpscaleFactor) else {
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
        let lineBox = rectDictionary(
            reprojectToFullWindow(
                observation.boundingBox, cropPixelRect: cropPixelRect,
                sourceWidth: sourceWidth, sourceHeight: sourceHeight
            )
        )
        for (rank, candidate) in observation.topCandidates(maxCandidatesPerObservation).enumerated() {
            let text = candidate.string
            let fullRange = NSRange(text.startIndex..<text.endIndex, in: text)
            for match in regex.matches(in: text, range: fullRange) {
                guard let range = Range(match.range, in: text),
                      let box = try? candidate.boundingBox(for: range)
                else { continue }
                // observation.boundingBox covers the whole recognized text
                // line and is the same for every candidate of this
                // observation, whereas `box.boundingBox` covers only this
                // specific candidate's regex match. The focused date-badge
                // validator needs both: exact filename cases use the match
                // box, and the intentionally truncated long-name case uses
                // the whole line so a badge overlapping later visible
                // text/ellipsis cannot be accepted merely because an early
                // prefix matched. Both boxes are normalized to
                // `processedImage` (the ROI crop, upscaled) by Vision, then
                // explicitly reprojected here into the raw screenshot's own
                // normalized coordinate space so downstream geometry checks
                // need no awareness of this preprocessing.
                let rect = reprojectToFullWindow(
                    box.boundingBox, cropPixelRect: cropPixelRect,
                    sourceWidth: sourceWidth, sourceHeight: sourceHeight
                )
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
    // traceable evidence (method, ROI contract, scale factor, and
    // source/crop/processed pixel size) without having to re-derive or trust
    // it implicitly.
    let payload: [String: Any] = [
        "matches": matches,
        "preprocessing": [
            "method": "roi_crop_uniform_upscale",
            "scale_factor": ocrUpscaleFactor,
            "source_size": ["width": sourceImage.width, "height": sourceImage.height],
            "roi": [
                "origin": SidebarROI.originContract,
                "x_fraction": SidebarROI.xFraction,
                "y_fraction_from_top": SidebarROI.yFractionFromTop,
                "width_fraction": SidebarROI.widthFraction,
                "height_fraction": SidebarROI.heightFraction,
            ],
            "crop_size": ["width": croppedImage.width, "height": croppedImage.height],
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
