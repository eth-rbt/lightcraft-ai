// Reads text aloud with the system voice and stays running, so a sentence starts at once: starting
// `say` for each sentence costs about a second before any sound. The chat server runs this.
//
// Lines in:  "say <id> <text>" queues a sentence, "stop" drops everything, said or queued.
// Lines out: "ready" once the voice is loaded, then "start <id>" and "end <id>" for each sentence.
//
// `--mute` speaks at no volume, to time the voice without making a sound.
@preconcurrency import AVFoundation
import Foundation

setvbuf(stdout, nil, _IOLBF, 0)

let mute = CommandLine.arguments.contains("--mute")

// Everything here runs on the main thread.
final class Voice: NSObject, AVSpeechSynthesizerDelegate, @unchecked Sendable {
  private let synthesizer = AVSpeechSynthesizer()
  private var ids: [ObjectIdentifier: String] = [:]

  override init() {
    super.init()
    synthesizer.delegate = self
  }

  private func utterance(_ text: String) -> AVSpeechUtterance {
    let utterance = AVSpeechUtterance(string: text)
    // The voice and speed chosen under System Settings > Accessibility > Spoken Content.
    utterance.prefersAssistiveTechnologySettings = true
    return utterance
  }

  /// Loads the voice by rendering a word to nowhere, so the first real sentence does not wait for it.
  func warmUp() {
    var done = false
    synthesizer.write(utterance("Ready.")) { buffer in
      let ended = (buffer as? AVAudioPCMBuffer)?.frameLength == 0
      if ended && !done {
        done = true
        print("ready")
      }
    }
  }

  func say(id: String, text: String) {
    let utterance = utterance(text)
    if mute {
      utterance.prefersAssistiveTechnologySettings = false  // which could set the volume itself
      utterance.volume = 0
    }
    ids[ObjectIdentifier(utterance)] = id
    synthesizer.speak(utterance)
  }

  func stop() {
    synthesizer.stopSpeaking(at: .immediate)
  }

  private func report(_ event: String, _ utterance: AVSpeechUtterance, forget: Bool) {
    let key = ObjectIdentifier(utterance)
    guard let id = ids[key] else { return }
    if forget { ids[key] = nil }
    print("\(event) \(id)")
  }

  func speechSynthesizer(_ synthesizer: AVSpeechSynthesizer, didStart utterance: AVSpeechUtterance) {
    report("start", utterance, forget: false)
  }

  func speechSynthesizer(_ synthesizer: AVSpeechSynthesizer, didFinish utterance: AVSpeechUtterance) {
    report("end", utterance, forget: true)
  }

  func speechSynthesizer(_ synthesizer: AVSpeechSynthesizer, didCancel utterance: AVSpeechUtterance) {
    report("end", utterance, forget: true)
  }
}

let voice = Voice()
voice.warmUp()

// Stop when the server that started this goes away (its end of stdin closes).
DispatchQueue.global().async {
  while let line = readLine() {
    DispatchQueue.main.async {
      if line == "stop" {
        voice.stop()
      } else if line.hasPrefix("say ") {
        let parts = line.dropFirst(4).split(separator: " ", maxSplits: 1)
        if parts.count == 2 { voice.say(id: String(parts[0]), text: String(parts[1])) }
      }
    }
  }
  exit(0)
}
RunLoop.main.run()
