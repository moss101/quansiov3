// The recorder's control channel (ADP-003, DOMAIN.md 11.7 B): a Unix-domain socket only this user can reach,
// opened by one connection that proves it holds a one-time token.
//
// How the channel is trusted, in the order a connection meets it:
//  1. the token arrives in a bootstrap file with mode 0600, owned by this user, that this process deletes as it
//     reads it (a world-readable file is refused and deleted: the token in it is compromised);
//  2. the socket lives in a directory this user owns with no access for anyone else, and the socket itself is
//     0600;
//  3. the first line of a connection must be `hello` with the token. The token works once: the connection that
//     presented it is the channel, and a second connection with the same token is refused. Too many wrong
//     tokens end the server rather than let it be guessed at;
//  4. when the channel's connection closes the recorder discards its recording and stops: with nobody left to
//     finish it, nothing is kept.
//
// Requests and answers are one JSON object per line.

import Foundation

public enum ControlServerError: Error, Equatable {
    case bootstrapInvalid(String)
    case directoryInvalid(String)
    case socketPathTooLong
    case socketFailed(String)
}

/// Read the one-time token from its bootstrap file and delete the file.
///
/// - Throws: `bootstrapInvalid` when the file is not a regular file owned by this user with mode 0600 and a
///   token-shaped body. The file is deleted whether or not it was acceptable.
public func readBootstrapToken(at path: String) throws -> String {
    var info = stat()
    guard lstat(path, &info) == 0 else { throw ControlServerError.bootstrapInvalid("the bootstrap file does not exist") }
    defer { unlink(path) }
    guard (info.st_mode & S_IFMT) == S_IFREG else { throw ControlServerError.bootstrapInvalid("not a regular file") }
    guard info.st_uid == geteuid() else { throw ControlServerError.bootstrapInvalid("not owned by this user") }
    guard (info.st_mode & 0o777) == 0o600 else {
        throw ControlServerError.bootstrapInvalid("the file's mode is not 0600: anyone who could read it holds the token")
    }
    guard info.st_size >= 32, info.st_size <= 256 else { throw ControlServerError.bootstrapInvalid("not a token") }
    guard let data = FileManager.default.contents(atPath: path), let text = String(data: data, encoding: .utf8) else {
        throw ControlServerError.bootstrapInvalid("unreadable")
    }
    let token = text.trimmingCharacters(in: .whitespacesAndNewlines)
    guard token.count >= 32, token.unicodeScalars.allSatisfy({ CharacterSet(charactersIn: "0123456789abcdef").contains($0) })
    else { throw ControlServerError.bootstrapInvalid("the token must be 32 or more lowercase hex characters") }
    return token
}

private func constantTimeEqual(_ a: String, _ b: String) -> Bool {
    let x = Array(a.utf8)
    let y = Array(b.utf8)
    guard x.count == y.count else { return false }
    var difference: UInt8 = 0
    for index in 0..<x.count { difference |= x[index] ^ y[index] }
    return difference == 0
}

public final class RecorderControlServer: @unchecked Sendable {
    private let socketDirectory: String
    private let controller: RecorderController
    private let lock = NSLock()
    private var token: String?
    private var listener: Int32 = -1
    private var channel: Int32 = -1
    private var failedHellos = 0
    private var stopped = false
    private var onClosed: (@Sendable () -> Void)?

    /// Where the socket is, once started.
    public private(set) var socketPath = ""

    public init(socketDirectory: String, controller: RecorderController) {
        self.socketDirectory = socketDirectory
        self.controller = controller
    }

    /// Read and delete the bootstrap file, prepare the directory, bind and listen.
    public func start(bootstrapFile: String, onClosed: (@Sendable () -> Void)? = nil) throws {
        token = try readBootstrapToken(at: bootstrapFile)
        self.onClosed = onClosed
        try prepareDirectory()
        socketPath = (socketDirectory as NSString).appendingPathComponent("recorder.sock")
        unlink(socketPath)
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let capacity = MemoryLayout.size(ofValue: address.sun_path)
        guard socketPath.utf8.count < capacity else { throw ControlServerError.socketPathTooLong }
        withUnsafeMutablePointer(to: &address.sun_path) {
            $0.withMemoryRebound(to: CChar.self, capacity: capacity) { _ = strlcpy($0, socketPath, capacity) }
        }
        let descriptor = socket(AF_UNIX, SOCK_STREAM, 0)
        guard descriptor >= 0 else { throw ControlServerError.socketFailed("socket: \(errno)") }
        var noSigPipe: Int32 = 1
        setsockopt(descriptor, SOL_SOCKET, SO_NOSIGPIPE, &noSigPipe, socklen_t(MemoryLayout<Int32>.size))
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(descriptor, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard bound == 0 else {
            close(descriptor)
            throw ControlServerError.socketFailed("bind: \(errno)")
        }
        chmod(socketPath, 0o600)
        guard listen(descriptor, 4) == 0 else {
            close(descriptor)
            throw ControlServerError.socketFailed("listen: \(errno)")
        }
        listener = descriptor
        Thread.detachNewThread { [weak self] in self?.acceptLoop(descriptor) }
    }

    private func prepareDirectory() throws {
        var info = stat()
        if lstat(socketDirectory, &info) == 0 {
            guard (info.st_mode & S_IFMT) == S_IFDIR else { throw ControlServerError.directoryInvalid("not a directory") }
            guard info.st_uid == geteuid() else { throw ControlServerError.directoryInvalid("not owned by this user") }
            guard (info.st_mode & 0o077) == 0 else {
                throw ControlServerError.directoryInvalid("other users can enter it (mode \(String(info.st_mode & 0o777, radix: 8)))")
            }
        } else {
            guard mkdir(socketDirectory, 0o700) == 0 else { throw ControlServerError.directoryInvalid("mkdir: \(errno)") }
        }
    }

    /// Stop listening, drop the channel and discard the recording.
    public func stop() {
        lock.lock()
        if stopped {
            lock.unlock()
            return
        }
        stopped = true
        let listening = listener
        let connected = channel
        lock.unlock()
        if listening >= 0 { close(listening) }
        if connected >= 0 { shutdown(connected, SHUT_RDWR) }
        unlink(socketPath)
        controller.shutdown()
    }

    // MARK: connections

    private func acceptLoop(_ descriptor: Int32) {
        while true {
            let connection = accept(descriptor, nil, nil)
            if connection < 0 { return }
            var noSigPipe: Int32 = 1
            setsockopt(connection, SOL_SOCKET, SO_NOSIGPIPE, &noSigPipe, socklen_t(MemoryLayout<Int32>.size))
            Thread.detachNewThread { [weak self] in self?.serve(connection) }
        }
    }

    private func serve(_ connection: Int32) {
        // A connection that does not say hello promptly is not a controller.
        var timeout = timeval(tv_sec: 5, tv_usec: 0)
        setsockopt(connection, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        var reader = LineReader(descriptor: connection)
        guard let first = reader.nextLine(), let hello = parse(first), hello["op"] as? String == "hello" else {
            refuse(connection, "unauthorized")
            return
        }
        guard authenticate(hello["token"] as? String, connection: connection) else {
            refuse(connection, "unauthorized")
            return
        }
        send(connection, ["ok": true, "op": "hello", "protocol": 1, "pid": Int(getpid())])
        var none = timeval(tv_sec: 0, tv_usec: 0)
        setsockopt(connection, SOL_SOCKET, SO_RCVTIMEO, &none, socklen_t(MemoryLayout<timeval>.size))
        while let line = reader.nextLine() {
            guard let request = parse(line) else {
                send(connection, ["ok": false, "error": "invalid_request", "detail": "not a JSON object"])
                continue
            }
            send(connection, controller.handle(request))
        }
        // The channel is gone: nobody can finish this recording any more.
        close(connection)
        lock.lock()
        let closed = onClosed
        lock.unlock()
        stop()
        closed?()
    }

    /// The token works once. Returns whether this connection is now the channel.
    private func authenticate(_ presented: String?, connection: Int32) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard let expected = token, let presented, constantTimeEqual(expected, presented) else {
            failedHellos += 1
            if failedHellos >= 5 { DispatchQueue.global().async { [weak self] in self?.stop() } }
            return false
        }
        token = nil
        channel = connection
        return true
    }

    private func refuse(_ connection: Int32, _ error: String) {
        send(connection, ["ok": false, "error": error])
        close(connection)
    }

    private func parse(_ line: Data) -> [String: Any]? {
        (try? JSONSerialization.jsonObject(with: line)) as? [String: Any]
    }

    private func send(_ connection: Int32, _ object: [String: Any]) {
        guard var data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys, .withoutEscapingSlashes])
        else { return }
        data.append(0x0A)
        data.withUnsafeBytes { buffer in
            var offset = 0
            while offset < buffer.count {
                let written = write(connection, buffer.baseAddress! + offset, buffer.count - offset)
                if written <= 0 { return }
                offset += written
            }
        }
    }
}

/// Reads newline-delimited lines from a socket, refusing one that is longer than a request has any need to be.
struct LineReader {
    let descriptor: Int32
    private var buffer = Data()
    static let maxLine = 1 << 20

    init(descriptor: Int32) {
        self.descriptor = descriptor
    }

    mutating func nextLine() -> Data? {
        while true {
            if let newline = buffer.firstIndex(of: 0x0A) {
                let line = buffer[buffer.startIndex..<newline]
                buffer.removeSubrange(buffer.startIndex...newline)
                return Data(line)
            }
            if buffer.count > Self.maxLine { return nil }
            var chunk = [UInt8](repeating: 0, count: 65536)
            let count = read(descriptor, &chunk, chunk.count)
            if count <= 0 { return nil }
            buffer.append(chunk, count: count)
        }
    }
}
