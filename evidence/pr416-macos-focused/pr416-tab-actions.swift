import AppKit
import CoreGraphics
import Foundation
import Vision

func fail(_ s: String) -> Never { FileHandle.standardError.write(Data((s + "\n").utf8)); exit(2) }
func output(_ x: [String:Any]) { let d = try! JSONSerialization.data(withJSONObject:x, options:[.sortedKeys]); print(String(data:d,encoding:.utf8)!) }
func apple(_ s: String) -> NSAppleEventDescriptor { var e:NSDictionary?; let r=NSAppleScript(source:s)?.executeAndReturnError(&e); if let e { fail("AppleScript: \(e)") }; return r! }
func focus(_ p:pid_t) { guard let a=NSRunningApplication(processIdentifier:p) else { fail("missing process") }; _=a.activate(options:[.activateAllWindows]); _=apple("tell application \"System Events\" to set frontmost of first process whose unix id is \(p) to true"); Thread.sleep(forTimeInterval:0.2) }
func bounds(_ p:pid_t) -> CGRect { let ws=CGWindowListCopyWindowInfo([.optionOnScreenOnly,.excludeDesktopElements],kCGNullWindowID) as? [[String:Any]] ?? []; guard let w=ws.first(where:{ ($0[kCGWindowOwnerPID as String] as? Int)==Int(p) && ($0[kCGWindowLayer as String] as? Int)==0 }), let b=w[kCGWindowBounds as String] as? [String:Any], let r=CGRect(dictionaryRepresentation:b as CFDictionary) else { fail("no target window") }; return r }
func match(_ path:String,_ pat:String,_ region:String) -> (String,CGRect)? {
    let q=VNRecognizeTextRequest(); q.recognitionLevel = .accurate; q.usesLanguageCorrection=false; q.recognitionLanguages=["ja-JP","en-US"]
    do { try VNImageRequestHandler(url:URL(fileURLWithPath:path),options:[:]).perform([q]) } catch { fail("OCR: \(error)") }
    let observed = (q.results ?? []).compactMap { $0.topCandidates(1).first?.string }; FileHandle.standardError.write(Data(("OCR observed: " + observed.joined(separator: " | ") + "\n").utf8))
    guard let re=try? NSRegularExpression(pattern:pat,options:[.caseInsensitive]) else { fail("invalid regex") }
    for o in q.results ?? [] {
        guard let t=o.topCandidates(1).first, let m=re.firstMatch(in:t.string,range:NSRange(t.string.startIndex..<t.string.endIndex,in:t.string)), let r=Range(m.range,in:t.string), let box=try? t.boundingBox(for:r) else { continue }
        let b=box.boundingBox, y=1-b.midY
        if region=="tab" && !(b.midX>0.22 && y<0.10) { continue }
        if region=="sidebar" && !(b.midX<0.23 && y>0.10 && y<0.80) { continue }
        if region=="body" && !(b.midX>0.23 && y>0.10 && y<0.90) { continue }
        return (String(t.string[r]),b)
    }
    return nil
}
func details(_ m:(String,CGRect)?) -> [String:Any] { guard let (t,b)=m else { return ["found":false] }; return ["found":true,"text":t,"box":["x":b.minX,"y":b.minY,"width":b.width,"height":b.height],"fraction":["x":b.midX,"y_from_top":1-b.midY]] }
func click(_ p:pid_t,_ path:String,_ pat:String,_ region:String,_ button:String) {
    focus(p); guard let m=match(path,pat,region) else { fail("no observed OCR target: \(pat), \(region)") }; let b=bounds(p); let point=CGPoint(x:b.minX+m.1.midX*b.width,y:b.minY+(1-m.1.midY)*b.height)
    let mb:CGMouseButton = button=="right" ? .right : button=="middle" ? .center : .left
    let dn:CGEventType = button=="right" ? .rightMouseDown : button=="middle" ? .otherMouseDown : .leftMouseDown
    let up:CGEventType = button=="right" ? .rightMouseUp : button=="middle" ? .otherMouseUp : .leftMouseUp
    guard let d=CGEvent(mouseEventSource:nil,mouseType:dn,mouseCursorPosition:point,mouseButton:mb), let u=CGEvent(mouseEventSource:nil,mouseType:up,mouseCursorPosition:point,mouseButton:mb) else { fail("mouse event unavailable") }
    let inheritedDownFlags=d.flags.rawValue, inheritedUpFlags=u.flags.rawValue
    // This action requests an unmodified mouse click. CGEvent(nil) can inherit
    // synthetic keyboard flags from a preceding shortcut in the session table.
    // Record that state before specifying the intended input explicitly.
    d.flags=[]; u.flags=[]
    guard let move=CGEvent(mouseEventSource:nil,mouseType:.mouseMoved,mouseCursorPosition:point,mouseButton:.left) else { fail("pointer move unavailable") }; move.flags=[]; move.post(tap:.cghidEventTap); Thread.sleep(forTimeInterval:0.2); d.post(tap:.cghidEventTap); Thread.sleep(forTimeInterval:0.1); u.post(tap:.cghidEventTap); Thread.sleep(forTimeInterval:0.5)
    var info=details(m); info["button"]=button; info["point"]=["x":point.x,"y":point.y]; info["inherited_down_flags"]=inheritedDownFlags; info["inherited_up_flags"]=inheritedUpFlags; info["sent_down_flags"]=d.flags.rawValue; info["sent_up_flags"]=u.flags.rawValue; output(info)
}
func key(_ p:pid_t,_ name:String) {
    focus(p); let code:CGKeyCode = name=="escape" ? 53 : 48; var flags:CGEventFlags=[]
    if name != "escape" { flags.insert(.maskControl) }; if name=="previous" { flags.insert(.maskShift) }
    guard let d=CGEvent(keyboardEventSource:nil,virtualKey:code,keyDown:true),let u=CGEvent(keyboardEventSource:nil,virtualKey:code,keyDown:false) else { fail("keyboard event unavailable") }; d.flags=flags;u.flags=flags;d.post(tap:.cghidEventTap);Thread.sleep(forTimeInterval:0.05);u.post(tap:.cghidEventTap);Thread.sleep(forTimeInterval:0.35);output(["key":name,"sent_down_flags":d.flags.rawValue,"sent_up_flags":u.flags.rawValue,"session_flags_after":CGEventSource.flagsState(.combinedSessionState).rawValue])
}
func hover(_ p:pid_t,_ path:String,_ pat:String,_ region:String) {
    focus(p);guard let m=match(path,pat,region) else {fail("no observed hover target: \(pat)")};let b=bounds(p);let point=CGPoint(x:b.minX+m.1.midX*b.width,y:b.minY+(1-m.1.midY)*b.height)
    guard let e=CGEvent(mouseEventSource:nil,mouseType:.mouseMoved,mouseCursorPosition:point,mouseButton:.left) else {fail("hover event unavailable")};let inherited=e.flags.rawValue;e.flags=[];e.post(tap:.cghidEventTap);Thread.sleep(forTimeInterval:0.8);var info=details(m);info["hovered"]=true;info["inherited_flags"]=inherited;info["sent_flags"]=e.flags.rawValue;output(info)
}
let a=Array(CommandLine.arguments.dropFirst()); guard let c=a.first else { fail("command required") }
switch c {
case "find": guard a.count==4 else { fail("find image regex region") }; output(details(match(a[1],a[2],a[3])))
case "click": guard a.count==6,let p=pid_t(a[1]) else { fail("click pid image regex region button") }; click(p,a[2],a[3],a[4],a[5])
case "hover": guard a.count==5,let p=pid_t(a[1]) else {fail("hover pid image regex region")};hover(p,a[2],a[3],a[4])
case "key": guard a.count==3,let p=pid_t(a[1]),["next","previous","escape"].contains(a[2]) else { fail("key pid next|previous|escape") }; key(p,a[2])
case "move": guard a.count==2,let p=pid_t(a[1]) else { fail("move pid") }; focus(p);let b=bounds(p);let q=CGPoint(x:b.maxX-20,y:b.midY);guard let e=CGEvent(mouseEventSource:nil,mouseType:.mouseMoved,mouseCursorPosition:q,mouseButton:.left) else {fail("move unavailable")};let inherited=e.flags.rawValue;e.flags=[];e.post(tap:.cghidEventTap);Thread.sleep(forTimeInterval:0.4);output(["moved":true,"inherited_flags":inherited,"sent_flags":e.flags.rawValue])
case "clipboard": output(["text":NSPasteboard.general.string(forType:.string) ?? ""])
case "clipboard-set": guard a.count==2 else { fail("clipboard-set text") };NSPasteboard.general.clearContents();NSPasteboard.general.setString(a[1],forType:.string);output(["set":true])
case "finder":
    let p=apple("tell application \"Finder\"\nset paths to {}\nrepeat with f in (get selection)\nset end of paths to POSIX path of (f as alias)\nend repeat\nreturn paths\nend tell")
    var paths:[String]=[];if p.numberOfItems>0 {for i in 1...p.numberOfItems {if let s=p.atIndex(i)?.stringValue {paths.append(s)}}}
    guard let app=NSRunningApplication.runningApplications(withBundleIdentifier:"com.apple.finder").first else { fail("Finder missing") }
    let ws=CGWindowListCopyWindowInfo([.optionOnScreenOnly,.excludeDesktopElements],kCGNullWindowID) as? [[String:Any]] ?? []
    let windows=ws.filter{($0[kCGWindowOwnerPID as String] as? Int)==Int(app.processIdentifier) && ($0[kCGWindowLayer as String] as? Int)==0}.compactMap{$0[kCGWindowNumber as String] as? Int}
    output(["paths":paths,"pid":Int(app.processIdentifier),"active":app.isActive,"windows":windows])
default: fail("unknown command")
}
