// flick-listen: dictation for "Fix with Claude". Prints the transcript so far, one full line
// per update, until stdin closes, then prints the final transcript and exits.
// Errors go to stderr with exit status 1.
// The usage strings macOS shows in its permission prompts are embedded from tools/listen.plist.
import AVFoundation
import Foundation
import Speech

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write((msg + "\n").data(using: .utf8)!)
    exit(1)
}

func say(_ text: String) {
    print(text.replacingOccurrences(of: "\n", with: " "))
    fflush(stdout)
}

let settings = "System Settings > Privacy & Security"

func authorize() {
    let wait = DispatchSemaphore(value: 0)
    var speech = SFSpeechRecognizerAuthorizationStatus.notDetermined
    SFSpeechRecognizer.requestAuthorization { speech = $0; wait.signal() }
    wait.wait()
    if speech != .authorized { fail("Speech recognition is off for Flick. Turn it on in \(settings) > Speech Recognition.") }
    var mic = false
    AVCaptureDevice.requestAccess(for: .audio) { mic = $0; wait.signal() }
    wait.wait()
    if !mic { fail("Microphone access is off for Flick. Turn it on in \(settings) > Microphone.") }
}

authorize()
guard let recognizer = SFSpeechRecognizer(), recognizer.isAvailable else { fail("Speech recognition is not available.") }
let request = SFSpeechAudioBufferRecognitionRequest()
request.shouldReportPartialResults = true
request.addsPunctuation = true
// On-device when the language model is installed: private, and no one-minute limit.
if recognizer.supportsOnDeviceRecognition { request.requiresOnDeviceRecognition = true }

let engine = AVAudioEngine()
let input = engine.inputNode
input.installTap(onBus: 0, bufferSize: 1024, format: input.outputFormat(forBus: 0)) { buf, _ in request.append(buf) }
engine.prepare()
do { try engine.start() } catch { fail("Cannot start the microphone: \(error.localizedDescription)") }

var last = ""
let task = recognizer.recognitionTask(with: request) { result, error in
    if let result {
        last = result.bestTranscription.formattedString
        say(last)
        if result.isFinal { exit(0) }
    }
    // An error after stopping (e.g. "no speech detected") just ends the session.
    if error != nil { exit(0) }
}

// Stop when Flick closes our stdin, then give the recognizer a moment to finalize.
Thread {
    while readLine() != nil {}
    DispatchQueue.main.async {
        engine.stop()
        input.removeTap(onBus: 0)
        request.endAudio()
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { exit(0) }
    }
}.start()

_ = task
RunLoop.main.run()
