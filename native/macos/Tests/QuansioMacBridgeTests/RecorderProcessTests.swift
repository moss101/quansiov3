import Darwin
import XCTest

@testable import QuansioMacBridge

/// The real `quansio-recorder` process, started the way the desktop starts it (ADP-003, DOMAIN.md 11.7 B):
/// a bootstrap file with a one-time token, a private socket directory, a hello, and the recorder gone when its
/// channel closes or its parent dies. Needs the executable built (`swift build --product quansio-recorder`);
/// without it the test says `BLOCKED_EXTERNAL`.
final class RecorderProcessTests: XCTestCase {
    private func recorderURL() throws -> URL {
        let products = Bundle(for: Self.self).bundleURL.deletingLastPathComponent()
        let candidate = products.appendingPathComponent("quansio-recorder")
        guard FileManager.default.isExecutableFile(atPath: candidate.path) else {
            throw XCTSkip("BLOCKED_EXTERNAL: quansio-recorder is not built (swift build --product quansio-recorder)")
        }
        return candidate
    }

    private func launch(_ files: (root: String, bootstrap: String, sockets: String, token: String), parent: pid_t? = nil) throws -> (Process, Pipe) {
        let process = Process()
        process.executableURL = try recorderURL()
        var arguments = ["--bootstrap", files.bootstrap, "--socket-dir", files.sockets]
        if let parent { arguments += ["--parent-pid", String(parent)] }
        process.arguments = arguments
        let output = Pipe()
        process.standardOutput = output
        process.standardError = FileHandle.nullDevice
        try process.run()
        return (process, output)
    }

    private func waitReady(_ output: Pipe) -> String? {
        var collected = Data()
        let deadline = Date().addingTimeInterval(90)  // the first launch of a new binary can be slow
        while Date() < deadline {
            let chunk = output.fileHandleForReading.availableData
            if chunk.isEmpty { return nil }
            collected.append(chunk)
            if let text = String(data: collected, encoding: .utf8), text.contains("\n") {
                return text.trimmingCharacters(in: .whitespacesAndNewlines).components(separatedBy: " ").last
            }
        }
        return nil
    }

    func testTheRealProcessReadsAndDeletesItsTokenAndAnswersOneAuthenticatedChannel() throws {
        let files = try makeRecorderDirectory()
        defer { try? FileManager.default.removeItem(atPath: files.root) }
        let (process, output) = try launch(files)
        defer { if process.isRunning { process.terminate() } }
        let socketPath = try XCTUnwrap(waitReady(output), "the recorder never said it was ready")
        XCTAssertFalse(FileManager.default.fileExists(atPath: files.bootstrap), "the process deleted its bootstrap file")
        var info = stat()
        XCTAssertEqual(stat(files.sockets, &info), 0)
        XCTAssertEqual(info.st_mode & 0o777, 0o700)

        let wrong = try ChannelClient(path: socketPath)
        XCTAssertEqual(try wrong.call(["op": "hello", "token": String(repeating: "2", count: 32)])["error"] as? String, "unauthorized")
        let channel = try ChannelClient(path: socketPath)
        let hello = try channel.call(["op": "hello", "token": files.token])
        XCTAssertEqual(hello["ok"] as? Bool, true, "\(hello)")
        XCTAssertEqual(hello["pid"] as? Int, Int(process.processIdentifier))
        let second = try ChannelClient(path: socketPath)
        XCTAssertEqual(try second.call(["op": "hello", "token": files.token])["ok"] as? Bool, false, "the token is spent")

        // The live permission answer and a start that is consistent with it: granted means it starts (and is
        // cancelled at once), anything else means a typed refusal and nothing captured.
        let permissions = try XCTUnwrap(try channel.call(["op": "permissions"])["permissions"] as? [String: String])
        let limits = RecordingLimits(screenshotsEnabled: false)
        let start = try channel.call([
            "op": "start", "id": "s1", "recording_id": testRecording, "owner": testOwner.wire, "limits": limits.wire,
            "max_duration_ms": 60_000,
        ])
        let needed = ["accessibility", "input_monitoring"]
        if needed.allSatisfy({ permissions[$0] == "granted" }) {
            XCTAssertEqual(start["ok"] as? Bool, true, "\(start)")
            let cancelled = try channel.call(["op": "cancel", "id": "c1", "owner": testOwner.wire])
            XCTAssertEqual(cancelled["ok"] as? Bool, true, "\(cancelled)")
        } else {
            XCTAssertEqual(start["error"] as? String, "permission_required", "\(start)")
            XCTAssertTrue(needed.contains(try XCTUnwrap(start["permission"] as? String)))
        }

        // Closing the channel ends the process: nothing is left to finish a recording.
        close(channel.descriptor)
        let deadline = Date().addingTimeInterval(15)
        while process.isRunning && Date() < deadline { Thread.sleep(forTimeInterval: 0.05) }
        XCTAssertFalse(process.isRunning, "the recorder exits when its channel closes")
    }

    func testTheRealProcessRefusesABootstrapFileOthersCouldRead() throws {
        let files = try makeRecorderDirectory()
        defer { try? FileManager.default.removeItem(atPath: files.root) }
        chmod(files.bootstrap, 0o644)
        let (process, _) = try launch(files)
        defer { if process.isRunning { process.terminate() } }
        let deadline = Date().addingTimeInterval(90)
        while process.isRunning && Date() < deadline { Thread.sleep(forTimeInterval: 0.05) }
        XCTAssertFalse(process.isRunning)
        XCTAssertEqual(process.terminationStatus, 2, "it refuses to start with a compromised token")
        XCTAssertFalse(FileManager.default.fileExists(atPath: files.bootstrap))
        XCTAssertFalse(FileManager.default.fileExists(atPath: files.sockets + "/recorder.sock"))
    }

    func testTheRealProcessExitsWhenItsParentGoesAway() throws {
        let files = try makeRecorderDirectory()
        defer { try? FileManager.default.removeItem(atPath: files.root) }
        // A stand-in parent the test controls: a process that sleeps and is then killed.
        let parent = Process()
        parent.executableURL = URL(fileURLWithPath: "/bin/sleep")
        parent.arguments = ["300"]
        try parent.run()
        let (process, output) = try launch(files, parent: parent.processIdentifier)
        defer {
            if process.isRunning { process.terminate() }
            if parent.isRunning { parent.terminate() }
        }
        _ = try XCTUnwrap(waitReady(output))
        XCTAssertTrue(process.isRunning)
        parent.terminate()
        let deadline = Date().addingTimeInterval(20)
        while process.isRunning && Date() < deadline { Thread.sleep(forTimeInterval: 0.05) }
        XCTAssertFalse(process.isRunning, "the recorder does not outlive the desktop that started it")
    }
}
