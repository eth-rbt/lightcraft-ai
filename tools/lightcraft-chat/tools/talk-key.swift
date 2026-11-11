// Prints "down" and "up" when the right Option key is pressed and released, whichever app is in
// front. The chat server runs this and forwards the lines to the page as hold-to-talk.
//
// It listens only: it cannot change or block input, and it asks macOS for modifier-key changes
// alone, so it never sees what is typed. macOS still requires Input Monitoring permission for the
// app that started it (your terminal). `--check` reports that permission without asking for it.
import CoreGraphics
import Foundation

setvbuf(stdout, nil, _IOLBF, 0)

let rightOptionKeyCode: Int64 = 61  // kVK_RightOption
let rightOptionFlag: UInt64 = 0x40  // NX_DEVICERALTKEYMASK: the right key, not either Option key

if CommandLine.arguments.contains("--check") {
  print(CGPreflightListenEventAccess() ? "allowed" : "denied")
  exit(0)
}
if !CGPreflightListenEventAccess() {
  _ = CGRequestListenEventAccess()  // shows the system prompt the first time
}

var held = false
var tap: CFMachPort?

let onEvent: CGEventTapCallBack = { _, type, event, _ in
  if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
    if let tap { CGEvent.tapEnable(tap: tap, enable: true) }
  } else if type == .flagsChanged, event.getIntegerValueField(.keyboardEventKeycode) == rightOptionKeyCode {
    let down = event.flags.rawValue & rightOptionFlag != 0
    if down != held {
      held = down
      print(down ? "down" : "up")
    }
  }
  return Unmanaged.passUnretained(event)
}

guard
  let port = CGEvent.tapCreate(
    tap: .cgSessionEventTap,
    place: .headInsertEventTap,
    options: .listenOnly,
    eventsOfInterest: CGEventMask(1 << CGEventType.flagsChanged.rawValue),
    callback: onEvent,
    userInfo: nil
  )
else {
  print("denied")
  exit(2)
}
tap = port
CFRunLoopAddSource(CFRunLoopGetCurrent(), CFMachPortCreateRunLoopSource(kCFAllocatorDefault, port, 0), .commonModes)
CGEvent.tapEnable(tap: port, enable: true)
print("ready")

// Stop when the server that started this goes away (its end of stdin closes).
DispatchQueue.global().async {
  while readLine() != nil {}
  exit(0)
}
CFRunLoopRun()
