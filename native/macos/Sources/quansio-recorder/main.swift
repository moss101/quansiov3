// quansio-recorder: the person's recorder process (ADP-003, DOMAIN.md 11.7).
//
// Started by the desktop for the length of one recording and never installed as a service: it holds at most
// one recording in memory, answers one authenticated control connection, and exits when that connection
// closes or the desktop that started it is gone. It reads its one-time token from the bootstrap file named on
// the command line (and deletes it), and listens on a socket in the private directory named beside it.
//
//   quansio-recorder --bootstrap <token-file> --socket-dir <private-dir> [--parent-pid <pid>]

import Darwin
import Dispatch
import Foundation
import QuansioMacBridge

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data("quansio-recorder: \(message)\n".utf8))
    exit(2)
}

var bootstrap: String?
var socketDirectory: String?
var parentPid: pid_t?
var arguments = CommandLine.arguments.dropFirst()
while let flag = arguments.popFirst() {
    switch flag {
    case "--bootstrap": bootstrap = arguments.popFirst()
    case "--socket-dir": socketDirectory = arguments.popFirst()
    case "--parent-pid": parentPid = arguments.popFirst().flatMap { pid_t($0) }
    default: fail("unknown argument \(flag)")
    }
}
guard let bootstrap, let socketDirectory else {
    fail("usage: quansio-recorder --bootstrap <token-file> --socket-dir <private-dir> [--parent-pid <pid>]")
}

signal(SIGPIPE, SIG_IGN)
let controller = RecorderController(environment: RecorderEnvironment(makeSource: { LiveEventObserver() }))
let server = RecorderControlServer(socketDirectory: socketDirectory, controller: controller)
do {
    try server.start(bootstrapFile: bootstrap) { exit(0) }
} catch {
    fail("cannot start: \(error)")
}
// Say where the channel is; the desktop waits for this line.
print("quansio-recorder ready \(server.socketPath)")
fflush(stdout)

// Kept in a global: a dispatch source that goes out of scope is cancelled.
var parentWatch: DispatchSourceProcess?
if let parentPid {
    let watch = DispatchSource.makeProcessSource(identifier: parentPid, eventMask: .exit, queue: .main)
    watch.setEventHandler {
        server.stop()
        exit(0)
    }
    watch.resume()
    parentWatch = watch
    if kill(parentPid, 0) != 0 {
        server.stop()
        exit(0)
    }
}
dispatchMain()
