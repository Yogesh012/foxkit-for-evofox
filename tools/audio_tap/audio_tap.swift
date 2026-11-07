// Writes what the Mac is playing to stdout as 32-bit float samples.
// Status lines go to stderr, including the rate and channel count of what follows.
// Follows the default output device when it changes. Nothing is muted or rerouted.
// Build with build.sh.

import CoreAudio
import Foundation

func log(_ message: String) {
    FileHandle.standardError.write(Data((message + "\n").utf8))
}

func address(_ selector: AudioObjectPropertySelector, scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress(mSelector: selector, mScope: scope, mElement: kAudioObjectPropertyElementMain)
}

func read<T>(_ object: AudioObjectID, _ selector: AudioObjectPropertySelector, as initial: T) -> T? {
    var value = initial
    var size = UInt32(MemoryLayout<T>.size)
    var where_ = address(selector)
    return AudioObjectGetPropertyData(object, &where_, 0, nil, &size, &value) == noErr ? value : nil
}

func text(_ object: AudioObjectID, _ selector: AudioObjectPropertySelector) -> String? {
    read(object, selector, as: "" as CFString).map { $0 as String }
}

func defaultOutput() -> AudioObjectID? {
    read(AudioObjectID(kAudioObjectSystemObject), kAudioHardwarePropertyDefaultOutputDevice, as: AudioObjectID(0))
}

// Starting a tap without permission hands back silence or hangs, without ever asking.
// macOS has no public call for this permission, so this uses the private TCC one.
func requestPermission() -> Bool {
    typealias Preflight = @convention(c) (CFString, CFDictionary?) -> Int32
    typealias Request = @convention(c) (CFString, CFDictionary?, @escaping @convention(block) (Bool) -> Void) -> Void
    guard let tcc = dlopen("/System/Library/PrivateFrameworks/TCC.framework/TCC", RTLD_NOW),
          let preflight = dlsym(tcc, "TCCAccessPreflight"),
          let request = dlsym(tcc, "TCCAccessRequest")
    else { return false }
    let service = "kTCCServiceAudioCapture" as CFString
    if unsafeBitCast(preflight, to: Preflight.self)(service, nil) == 0 {
        return true
    }
    var granted = false
    let answered = DispatchSemaphore(value: 0)
    unsafeBitCast(request, to: Request.self)(service, nil) { answer in
        granted = answer
        answered.signal()
    }
    answered.wait()
    return granted
}

final class Capture {
    private let tapUID: String
    private let writes = DispatchQueue(label: "audio_tap.writes")
    private var aggregate = AudioObjectID(0)
    private var proc: AudioDeviceIOProcID?

    init(tapUID: String) {
        self.tapUID = tapUID
    }

    func start() {
        guard let output = defaultOutput(), let outputUID = text(output, kAudioDevicePropertyDeviceUID) else {
            log("error no default output device")
            return
        }
        let description: [String: Any] = [
            kAudioAggregateDeviceNameKey: "Foxkit Audio Tap",
            kAudioAggregateDeviceUIDKey: UUID().uuidString,
            kAudioAggregateDeviceIsPrivateKey: true,
            kAudioAggregateDeviceTapAutoStartKey: true,
            kAudioAggregateDeviceMainSubDeviceKey: outputUID,
            kAudioAggregateDeviceSubDeviceListKey: [[kAudioSubDeviceUIDKey: outputUID]],
            kAudioAggregateDeviceTapListKey: [[kAudioSubTapUIDKey: tapUID, kAudioSubTapDriftCompensationKey: true]],
        ]
        var status = AudioHardwareCreateAggregateDevice(description as CFDictionary, &aggregate)
        guard status == noErr else {
            log("error creating aggregate device: \(status)")
            return
        }
        status = AudioDeviceCreateIOProcIDWithBlock(&proc, aggregate, nil) { [writes] _, input, _, _, _ in
            // The tap is the last input stream, after any input the output device has.
            let buffers = UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: input))
            guard let tap = buffers.last, let samples = tap.mData else { return }
            let data = Data(bytes: samples, count: Int(tap.mDataByteSize))
            writes.async { FileHandle.standardOutput.write(data) }
        }
        guard status == noErr else {
            log("error creating IO proc: \(status)")
            return
        }
        status = AudioDeviceStart(aggregate, proc)
        guard status == noErr else {
            log("error starting capture: \(status)")
            return
        }
        let rate = read(aggregate, kAudioDevicePropertyNominalSampleRate, as: Float64(0)) ?? 0
        log("device \(text(output, kAudioObjectPropertyName) ?? "unknown") rate \(Int(rate))")
    }

    func stop() {
        if let proc {
            AudioDeviceStop(aggregate, proc)
            AudioDeviceDestroyIOProcID(aggregate, proc)
        }
        proc = nil
        if aggregate != 0 {
            AudioHardwareDestroyAggregateDevice(aggregate)
        }
        aggregate = 0
    }
}

// macOS can reopen the app by itself, with output going nowhere. Only run for a reader.
var outputInfo = stat()
guard fstat(STDOUT_FILENO, &outputInfo) == 0, outputInfo.st_mode & S_IFMT == S_IFIFO else {
    log("error nothing is reading the output")
    exit(1)
}
log("pid \(getpid())")
// The app passes its pid. A closed pipe doesn't reliably end this process, so watch the app.
if CommandLine.arguments.count > 1, let appPid = pid_t(CommandLine.arguments[1]) {
    DispatchQueue.global(qos: .utility).async {
        while kill(appPid, 0) == 0 || errno == EPERM {
            sleep(1)
        }
        _exit(0)
    }
}
for signalNumber in [SIGINT, SIGTERM] {
    signal(signalNumber) { _ in _exit(0) }
}
guard requestPermission() else {
    log("error System Audio Recording permission not granted")
    exit(1)
}
let tapDescription = CATapDescription(stereoGlobalTapButExcludeProcesses: [])
tapDescription.isPrivate = true
tapDescription.muteBehavior = .unmuted
var tap = AudioObjectID(0)
let tapStatus = AudioHardwareCreateProcessTap(tapDescription, &tap)
guard tapStatus == noErr else {
    log("error creating tap: \(tapStatus)")
    exit(1)
}

let capture = Capture(tapUID: tapDescription.uuid.uuidString)
let events = DispatchQueue(label: "audio_tap.events")
events.async { capture.start() }

var defaultOutputAddress = address(kAudioHardwarePropertyDefaultOutputDevice)
AudioObjectAddPropertyListenerBlock(AudioObjectID(kAudioObjectSystemObject), &defaultOutputAddress, events) { _, _ in
    capture.stop()
    capture.start()
}

dispatchMain()
