import Darwin
import XCTest

@testable import QuansioMacBridge

private let granted = PermissionProbes(
    accessibility: { .granted }, screenRecording: { .granted }, inputMonitoring: { .granted }, microphone: { .granted })

/// An observation source a test drives by hand: the observer's place in the pipeline, not a stand-in for
/// the channel, the token, the fence or the session, which are all real here.
final class ScriptedSource: ObservationSource, @unchecked Sendable {
    private let lock = NSLock()
    private var deliver: (@Sendable (Observation) -> Void)?
    private(set) var stopped = false

    func start(deliver: @escaping @Sendable (Observation) -> Void) throws {
        lock.lock()
        self.deliver = deliver
        lock.unlock()
    }

    func stop() {
        lock.lock()
        stopped = true
        deliver = nil
        lock.unlock()
    }

    func emit(_ observation: Observation) {
        lock.lock()
        let target = deliver
        lock.unlock()
        target?(observation)
    }
}

/// A blocking newline-JSON client for the recorder's socket.
final class ChannelClient {
    let descriptor: Int32
    private var reader: LineReader

    init(path: String) throws {
        let descriptor = socket(AF_UNIX, SOCK_STREAM, 0)
        XCTAssertGreaterThanOrEqual(descriptor, 0)
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let capacity = MemoryLayout.size(ofValue: address.sun_path)
        withUnsafeMutablePointer(to: &address.sun_path) {
            $0.withMemoryRebound(to: CChar.self, capacity: capacity) { _ = strlcpy($0, path, capacity) }
        }
        let status = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(descriptor, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard status == 0 else { throw ControlServerError.socketFailed("connect: \(errno)") }
        var timeout = timeval(tv_sec: 20, tv_usec: 0)
        setsockopt(descriptor, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        self.descriptor = descriptor
        self.reader = LineReader(descriptor: descriptor)
    }

    deinit { close(descriptor) }

    func send(_ object: [String: Any]) throws {
        var data = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
        data.append(0x0A)
        _ = data.withUnsafeBytes { write(descriptor, $0.baseAddress!, $0.count) }
    }

    func receive() -> [String: Any]? {
        guard let line = reader.nextLine() else { return nil }
        return (try? JSONSerialization.jsonObject(with: line)) as? [String: Any]
    }

    func call(_ object: [String: Any]) throws -> [String: Any] {
        try send(object)
        return try XCTUnwrap(receive(), "the channel closed instead of answering")
    }

    var isClosed: Bool {
        var byte: UInt8 = 0
        return recv(descriptor, &byte, 1, MSG_PEEK | MSG_DONTWAIT) == 0
    }
}

/// Where a test's private recorder files live: a short path, because a socket path must fit in 104 bytes.
func makeRecorderDirectory() throws -> (root: String, bootstrap: String, sockets: String, token: String) {
    var template = Array("/tmp/qrec.XXXXXX".utf8CString)
    guard let made = mkdtemp(&template) else { throw ControlServerError.directoryInvalid("mkdtemp") }
    let root = String(cString: made)
    let token = (0..<4).map { _ in String(format: "%08x", UInt32.random(in: 0...UInt32.max)) }.joined()
    let bootstrap = root + "/token"
    FileManager.default.createFile(atPath: bootstrap, contents: Data((token + "\n").utf8), attributes: [.posixPermissions: 0o600])
    return (root, bootstrap, root + "/sock", token)
}

final class RecorderControlTests: XCTestCase {
    private var cleanup: [String] = []
    private var servers: [RecorderControlServer] = []

    override func tearDown() {
        servers.forEach { $0.stop() }
        cleanup.forEach { try? FileManager.default.removeItem(atPath: $0) }
        servers.removeAll()
        cleanup.removeAll()
    }

    private func started(
        permissions: PermissionProbes = granted, source: ScriptedSource? = nil, clock: ManualClock = ManualClock(),
        secure: SecureInputProbe = ScriptedSecureInput()
    ) throws -> (server: RecorderControlServer, controller: RecorderController, token: String, paths: (root: String, bootstrap: String, sockets: String)) {
        let files = try makeRecorderDirectory()
        cleanup.append(files.root)
        let environment = RecorderEnvironment(
            clock: clock, secureInput: secure, recognizer: FixedRecognizer(text: "Notes"),
            capture: { makePNG(width: 64, height: 48) }, permissions: permissions, makeSource: { source })
        let controller = RecorderController(environment: environment)
        let server = RecorderControlServer(socketDirectory: files.sockets, controller: controller)
        try server.start(bootstrapFile: files.bootstrap)
        servers.append(server)
        return (server, controller, files.token, (files.root, files.bootstrap, files.sockets))
    }

    private func connected(_ setup: (server: RecorderControlServer, controller: RecorderController, token: String, paths: (root: String, bootstrap: String, sockets: String))) throws -> ChannelClient {
        let client = try ChannelClient(path: setup.server.socketPath)
        let hello = try client.call(["op": "hello", "token": setup.token])
        XCTAssertEqual(hello["ok"] as? Bool, true, "\(hello)")
        return client
    }

    private var startRequest: [String: Any] {
        [
            "op": "start", "id": "start_1", "recording_id": testRecording, "owner": testOwner.wire,
            "limits": RecordingLimits(screenshotMinIntervalMs: 60_000).wire, "max_duration_ms": 1_800_000,
        ]
    }

    // MARK: the bootstrap file and the directory

    func testTheBootstrapFileIsReadOnceAndDeleted() throws {
        let files = try makeRecorderDirectory()
        cleanup.append(files.root)
        XCTAssertEqual(try readBootstrapToken(at: files.bootstrap), files.token)
        XCTAssertFalse(FileManager.default.fileExists(atPath: files.bootstrap), "the token file is deleted as it is read")
        XCTAssertThrowsError(try readBootstrapToken(at: files.bootstrap), "a second read has nothing to read")
    }

    func testABootstrapFileOthersCouldReadIsRefusedAndDeletedAnyway() throws {
        let files = try makeRecorderDirectory()
        cleanup.append(files.root)
        chmod(files.bootstrap, 0o644)
        XCTAssertThrowsError(try readBootstrapToken(at: files.bootstrap)) {
            guard case .bootstrapInvalid = $0 as? ControlServerError else { return XCTFail("\($0)") }
        }
        XCTAssertFalse(FileManager.default.fileExists(atPath: files.bootstrap), "a compromised token does not linger")

        // A symlink, a short token and a non-hex token are not tokens either.
        let target = files.root + "/real"
        FileManager.default.createFile(atPath: target, contents: Data(String(repeating: "a", count: 40).utf8), attributes: [.posixPermissions: 0o600])
        try FileManager.default.createSymbolicLink(atPath: files.bootstrap, withDestinationPath: target)
        XCTAssertThrowsError(try readBootstrapToken(at: files.bootstrap))
        for body in ["abc", String(repeating: "Z", count: 40), String(repeating: "g", count: 40)] {
            FileManager.default.createFile(atPath: files.bootstrap, contents: Data(body.utf8), attributes: [.posixPermissions: 0o600])
            XCTAssertThrowsError(try readBootstrapToken(at: files.bootstrap), body)
            XCTAssertFalse(FileManager.default.fileExists(atPath: files.bootstrap))
        }
    }

    func testTheSocketDirectoryMustBePrivateAndTheSocketIsOwnerOnly() throws {
        let files = try makeRecorderDirectory()
        cleanup.append(files.root)
        mkdir(files.sockets, 0o755)
        chmod(files.sockets, 0o755)
        let refused = RecorderControlServer(socketDirectory: files.sockets, controller: RecorderController(environment: RecorderEnvironment()))
        XCTAssertThrowsError(try refused.start(bootstrapFile: files.bootstrap)) {
            guard case .directoryInvalid = $0 as? ControlServerError else { return XCTFail("\($0)") }
        }
        XCTAssertFalse(FileManager.default.fileExists(atPath: files.bootstrap))

        let setup = try started()
        var info = stat()
        XCTAssertEqual(stat(setup.paths.sockets, &info), 0)
        XCTAssertEqual(info.st_mode & 0o777, 0o700, "the directory is for this user only")
        XCTAssertEqual(stat(setup.server.socketPath, &info), 0)
        XCTAssertEqual(info.st_mode & 0o777, 0o600, "and so is the socket")
        XCTAssertFalse(FileManager.default.fileExists(atPath: setup.paths.bootstrap))
    }

    // MARK: authentication

    func testTheTokenWorksOnceAndOnlyTheFirstConnectionIsTheChannel() throws {
        let setup = try started()
        let wrong = try ChannelClient(path: setup.server.socketPath)
        let refusal = try wrong.call(["op": "hello", "token": String(repeating: "0", count: 32)])
        XCTAssertEqual(refusal["ok"] as? Bool, false)
        XCTAssertEqual(refusal["error"] as? String, "unauthorized")

        let skipped = try ChannelClient(path: setup.server.socketPath)
        let early = try skipped.call(["op": "permissions"])
        XCTAssertEqual(early["error"] as? String, "unauthorized", "no request is answered before hello")

        let channel = try connected(setup)
        let answer = try channel.call(["op": "permissions", "id": "p"])
        XCTAssertEqual(answer["ok"] as? Bool, true)

        let second = try ChannelClient(path: setup.server.socketPath)
        let reused = try second.call(["op": "hello", "token": setup.token])
        XCTAssertEqual(reused["ok"] as? Bool, false, "the one-time token is spent")
        XCTAssertEqual(reused["error"] as? String, "unauthorized")
        // The real channel is unharmed.
        XCTAssertEqual(try channel.call(["op": "permissions"])["ok"] as? Bool, true)
    }

    func testGuessingTheTokenEndsTheServerRatherThanLetItBeGuessed() throws {
        let setup = try started()
        for _ in 0..<5 {
            let client = try ChannelClient(path: setup.server.socketPath)
            _ = try client.call(["op": "hello", "token": String(repeating: "1", count: 32)])
        }
        let deadline = Date().addingTimeInterval(5)
        var refused = false
        while Date() < deadline {
            if (try? ChannelClient(path: setup.server.socketPath)) == nil { refused = true; break }
            Thread.sleep(forTimeInterval: 0.05)
        }
        XCTAssertTrue(refused, "after five wrong tokens nothing listens any more")
    }

    // MARK: a recording over the channel

    func testStartObserveFinishOverTheChannelAndTheArchiveHoldsWhatWasObserved() throws {
        let source = ScriptedSource()
        let clock = ManualClock()
        let setup = try started(source: source, clock: clock)
        let channel = try connected(setup)
        let started = try channel.call(startRequest)
        XCTAssertEqual(started["ok"] as? Bool, true, "\(started)")
        clock.advance(seconds: 1)
        source.emit(observation(.click, at: clock, title: "New Note"))
        clock.advance(seconds: 1)
        source.emit(observation(.type, at: clock, role: "AXTextArea", title: "Note body", text: "token AKIAIOSFODNN7EXAMPLE"))
        Thread.sleep(forTimeInterval: 0.1)
        let status = try channel.call(["op": "status", "owner": testOwner.wire])
        XCTAssertEqual(status["state"] as? String, "recording")
        XCTAssertEqual(status["actions"] as? Int, 2)
        XCTAssertEqual((status["counts"] as? [String: Int])?["api_key"], 1, "counts, never values")
        XCTAssertFalse("\(status)".contains("AKIA"))
        // No request can finish it except `finish`; nothing can inject an action.
        for op in ["inject", "observe", "complete", "stop", "end"] {
            let answer = try channel.call(["op": op, "owner": testOwner.wire, "id": "x_\(op)"])
            XCTAssertEqual(answer["error"] as? String, "unknown_op", op)
        }
        XCTAssertEqual(try channel.call(["op": "status", "owner": testOwner.wire])["state"] as? String, "recording")
        clock.advance(seconds: 1)
        let finished = try channel.call(["op": "finish", "id": "fin_1", "owner": testOwner.wire, "request_id": "fin_1"])
        XCTAssertEqual(finished["ok"] as? Bool, true, "\(finished)")
        let bytes = try XCTUnwrap(Data(base64Encoded: try XCTUnwrap(finished["archive_base64"] as? String)))
        XCTAssertFalse(String(decoding: bytes, as: UTF8.self).contains("AKIA"))
        let document = try jsonObject(bytes)
        XCTAssertEqual((document["actions"] as? [[String: Any]])?.count, 2)
        XCTAssertTrue(source.stopped, "observing stops when the recording is finished")
    }

    func testAnotherOwnerOrEpochOverTheChannelEndsTheRecordingAndDiscardsIt() throws {
        let source = ScriptedSource()
        let clock = ManualClock()
        let setup = try started(source: source, clock: clock)
        let channel = try connected(setup)
        XCTAssertEqual(try channel.call(startRequest)["ok"] as? Bool, true)
        clock.advance(seconds: 1)
        source.emit(observation(.click, at: clock, title: "New Note"))
        Thread.sleep(forTimeInterval: 0.1)
        let switched = RecordingOwner(tenantId: testOwner.tenantId, userId: "usr_01J8Z3K6F1N8VQ2X5W9YOTHER1", epoch: testOwner.epoch + 1)
        let refused = try channel.call(["op": "finish", "id": "fin_1", "owner": switched.wire, "request_id": "fin_1"])
        XCTAssertEqual(refused["ok"] as? Bool, false)
        XCTAssertEqual(refused["error"] as? String, "owner_changed")
        XCTAssertTrue(source.stopped, "observing stops at once")
        XCTAssertEqual(setup.controller.currentSession?.currentState, .aborted("owner_changed"))
        // The original owner finds nothing to finish: it was discarded.
        let late = try channel.call(["op": "finish", "id": "fin_2", "owner": testOwner.wire, "request_id": "fin_2"])
        XCTAssertEqual(late["ok"] as? Bool, false)
        XCTAssertEqual(late["error"] as? String, "not_recording")
        // A different epoch alone is enough.
        let setup2 = try started(source: ScriptedSource())
        let channel2 = try connected(setup2)
        XCTAssertEqual(try channel2.call(startRequest)["ok"] as? Bool, true)
        let epoch = RecordingOwner(tenantId: testOwner.tenantId, userId: testOwner.userId, epoch: testOwner.epoch + 1)
        XCTAssertEqual(try channel2.call(["op": "status", "owner": epoch.wire])["error"] as? String, "owner_changed")
    }

    func testStartAndFinishAreIdempotentByIdAndFingerprint() throws {
        let clock = ManualClock()
        let setup = try started(source: ScriptedSource(), clock: clock)
        let channel = try connected(setup)
        let first = try channel.call(startRequest)
        let replay = try channel.call(startRequest)
        XCTAssertEqual(first["ok"] as? Bool, true)
        XCTAssertEqual(replay["ok"] as? Bool, true, "the same id and arguments replay the first answer, not a second recording")
        var other = startRequest
        other["recording_id"] = "rec_01J8Z3K6F1N8VQ2X5W9YBBBBBB"
        let mismatch = try channel.call(other)
        XCTAssertEqual(mismatch["error"] as? String, "idempotency_mismatch")
        var again = startRequest
        again["id"] = "start_2"
        XCTAssertEqual(try channel.call(again)["error"] as? String, "already_recording", "another id while one is in progress")
        clock.advance(seconds: 1)
        let finishRequest: [String: Any] = ["op": "finish", "id": "fin_1", "owner": testOwner.wire, "request_id": "fin_1"]
        let one = try channel.call(finishRequest)
        let two = try channel.call(finishRequest)
        XCTAssertEqual(one["sha256"] as? String, two["sha256"] as? String)
        var different = finishRequest
        different["request_id"] = "fin_other"
        XCTAssertEqual(try channel.call(different)["error"] as? String, "idempotency_mismatch")
    }

    func testNothingIsCapturedUntilEveryNeededPermissionIsGranted() throws {
        let denied = PermissionProbes(
            accessibility: { .granted }, screenRecording: { .denied }, inputMonitoring: { .granted }, microphone: { .notDetermined })
        let source = ScriptedSource()
        let setup = try started(permissions: denied, source: source)
        let channel = try connected(setup)
        let refusal = try channel.call(startRequest)
        XCTAssertEqual(refusal["ok"] as? Bool, false)
        XCTAssertEqual(refusal["error"] as? String, "permission_required")
        XCTAssertEqual(refusal["permission"] as? String, "screen_recording")
        XCTAssertEqual(refusal["status"] as? String, "denied")
        XCTAssertEqual((refusal["permissions"] as? [String: String])?["microphone"], "not_determined")
        XCTAssertNil(setup.controller.currentSession, "no session, so nothing was captured")
        XCTAssertFalse(source.stopped)
        // Screenshots off, narration off: only Accessibility and Input Monitoring are needed.
        var limits = RecordingLimits()
        limits.screenshotsEnabled = false
        var request = startRequest
        request["id"] = "start_off"
        request["limits"] = limits.wire
        XCTAssertEqual(try channel.call(request)["ok"] as? Bool, true)
        // Narration needs the microphone.
        let noMic = try started(permissions: denied)
        let channelNoMic = try connected(noMic)
        var narrated = request
        narrated["narration"] = true
        XCTAssertEqual(try channelNoMic.call(narrated)["error"] as? String, "permission_required")
        // A refusal is not remembered: once granted, the same id succeeds.
        let flipping = LockedPermission()
        let probes = PermissionProbes(
            accessibility: { flipping.value }, screenRecording: { .granted }, inputMonitoring: { .granted }, microphone: { .granted })
        let retry = try started(permissions: probes)
        let channelRetry = try connected(retry)
        XCTAssertEqual(try channelRetry.call(startRequest)["error"] as? String, "permission_required")
        flipping.value = .granted
        XCTAssertEqual(try channelRetry.call(startRequest)["ok"] as? Bool, true)
    }

    func testClosingTheChannelDiscardsTheRecordingAndStopsTheRecorder() throws {
        let source = ScriptedSource()
        let clock = ManualClock()
        let closed = expectation(description: "the recorder stops when its channel closes")
        let files = try makeRecorderDirectory()
        cleanup.append(files.root)
        let controller = RecorderController(
            environment: RecorderEnvironment(
                clock: clock, secureInput: ScriptedSecureInput(), recognizer: FixedRecognizer(text: "Notes"),
                capture: { makePNG(width: 64, height: 48) }, permissions: granted, makeSource: { source }))
        let server = RecorderControlServer(socketDirectory: files.sockets, controller: controller)
        try server.start(bootstrapFile: files.bootstrap) { closed.fulfill() }
        servers.append(server)
        var channel: ChannelClient? = try ChannelClient(path: server.socketPath)
        _ = try channel?.call(["op": "hello", "token": files.token])
        XCTAssertEqual(try channel?.call(startRequest)["ok"] as? Bool, true)
        clock.advance(seconds: 1)
        source.emit(observation(.click, at: clock, title: "New Note"))
        channel = nil  // the desktop went away
        wait(for: [closed], timeout: 10)
        XCTAssertTrue(source.stopped)
        XCTAssertEqual(controller.currentSession?.currentState, .cancelled, "with nobody to finish it, nothing is kept")
    }

    func testTheRealPermissionStatusIsFourTypedAnswers() throws {
        let setup = try started(permissions: .live)
        let channel = try connected(setup)
        let answer = try channel.call(["op": "permissions"])
        let permissions = try XCTUnwrap(answer["permissions"] as? [String: String])
        XCTAssertEqual(Set(permissions.keys), ["accessibility", "screen_recording", "input_monitoring", "microphone"])
        for value in permissions.values { XCTAssertTrue(["granted", "denied", "not_determined", "restricted"].contains(value), value) }
    }
}

final class LockedPermission: @unchecked Sendable {
    private let lock = NSLock()
    private var current = PermissionState.denied
    var value: PermissionState {
        get { lock.lock(); defer { lock.unlock() }; return current }
        set { lock.lock(); current = newValue; lock.unlock() }
    }
}
