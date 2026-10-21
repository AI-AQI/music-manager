import Foundation
import Darwin

let fm = FileManager.default
var passed = 0
func check(_ condition: @autoclosure () throws -> Bool, _ message: String) throws {
    if try !condition() { throw InstallError("TEST FAILED: " + message) }
}
func fails(_ message: String, _ work: () throws -> Void) throws {
    do { try work() } catch { return }
    throw InstallError("TEST FAILED (expected failure): " + message)
}
func test(_ name: String, _ work: () throws -> Void) throws {
    try work(); passed += 1; print("PASS \(name)")
}
final class FakeIO: InstallerIO {
    var operations: [(executable: String, arguments: [String], privileged: Bool)] = []
    var protectedDirectories: [URL] = []
    var denyAuthorization = false
    var appleSilicon = true
    var architectures: [String: String] = [:]
    var invalidSignatures: Set<String> = []
    var rosettaAvailable = true
    var translationProbes = 0
    var failCopy = false
    var failNewMove = false
    var busy = false
    var compatible = true
    var stopped = 0
    var rollbackChecked = false
    var rollbackExecutable: String?
    var failAfterMoveSource: String?
    var copies = 0
    func stopApplication(at destination: URL) throws {
        stopped += 1
        if busy { throw InstallError("app still running") }
    }
    func run(_ executable: String, _ args: [String], privileged: Bool) throws -> String {
        operations.append((executable, args, privileged))
        if privileged && denyAuthorization { throw InstallError("administrator authorization cancelled") }
        // Emulate authorization inside the isolated fixture, without a real admin prompt.
        // Restore permissions after each operation so later operations must check again.
        var originalModes: [(URL, NSNumber)] = []
        if privileged {
            for directory in protectedDirectories {
                let attributes = try fm.attributesOfItem(atPath: directory.path)
                if let mode = attributes[.posixPermissions] as? NSNumber {
                    originalModes.append((directory, mode))
                    try fm.setAttributes([.posixPermissions: mode.intValue | 0o700], ofItemAtPath: directory.path)
                }
            }
        }
        defer {
            for (directory, mode) in originalModes {
                try? fm.setAttributes([.posixPermissions: mode], ofItemAtPath: directory.path)
            }
        }
        if executable == "/usr/bin/codesign", invalidSignatures.contains(args.last!) {
            throw InstallError("invalid signature")
        }
        if executable == "/usr/sbin/sysctl" { return appleSilicon ? "1" : "0" }
        if executable == "/usr/bin/file" { return "Mach-O executable " + (architectures[args.last!] ?? "arm64") }
        if executable == "/usr/bin/arch" {
            try check(args == ["-x86_64", "/usr/bin/true"] && !privileged, "unsafe translation probe")
            translationProbes += 1
            if !rosettaAvailable { throw InstallError("Bad CPU type in executable") }
            return ""
        }
        if executable == "/usr/bin/ditto" {
            if args.first == "-c" {
                return try SystemIO(log: { _ in }).run(executable, args, privileged: false)
            }
            copies += 1
            if failCopy { throw InstallError("copy failed") }
            try fm.copyItem(atPath: args[0], toPath: args[1])
        } else if executable == "/bin/mv" {
            if failNewMove && args[0].contains(".music-manager-stage-") { throw InstallError("rename failed") }
            try fm.moveItem(atPath: args[0], toPath: args[1])
            if args[0] == failAfterMoveSource { throw InstallError("interrupted after rename") }
        } else if executable == "/bin/mkdir" {
            try fm.createDirectory(atPath: args[1], withIntermediateDirectories: true)
        } else if args.first == "--installer-check-rollback" {
            rollbackChecked = true
            rollbackExecutable = executable
            if !compatible { throw InstallError("database incompatible") }
        }
        return ""
    }
}
final class Fixture {
    let root = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("mm-installer-tests-" + UUID().uuidString)
    var source: URL { root.appendingPathComponent("payload/声场档案.app") }
    var destination: URL { root.appendingPathComponent("Applications/声场档案.app") }
    var logs: URL { root.appendingPathComponent("logs") }
    var sentinel: URL { root.appendingPathComponent("external/library.db") }
    let io = FakeIO()
    var engine: Installer?
    init(old: String? = "0.2.5", new: String = "0.2.6") throws {
        try fm.createDirectory(at: destination.deletingLastPathComponent(), withIntermediateDirectories: true)
        try fm.createDirectory(at: sentinel.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("music library must survive".utf8).write(to: sentinel)
        try app(source, version: new, schema: 3)
        if let old = old { try app(destination, version: old, schema: nil) }
        try reopen()
    }
    func app(_ path: URL, version: String, schema: Int?) throws {
        try fm.createDirectory(at: path.appendingPathComponent("Contents/MacOS"), withIntermediateDirectories: true)
        var plist: [String: Any] = ["CFBundleIdentifier":"com.musicmanager.app", "CFBundleExecutable":"music-manager", "CFBundleShortVersionString":version, "MusicManagerStartupProtocol":1]
        if let schema = schema { plist["MusicManagerDatabaseSchema"] = schema }
        try PropertyListSerialization.data(fromPropertyList: plist, format: .xml, options: 0).write(to: path.appendingPathComponent("Contents/Info.plist"))
        try Data("#!/bin/sh\nexit 0\n".utf8).write(to: path.appendingPathComponent("Contents/MacOS/music-manager"))
    }
    func reopen(systemChecks: Bool = false) throws {
        engine = nil
        engine = try Installer(source: source, destination: destination, logRoot: logs, io: io, systemChecks: systemChecks)
    }
    func readyExecutable(version: String) throws {
        let executable = destination.appendingPathComponent("Contents/MacOS/music-manager")
        let script = """
        #!/bin/sh
        printf '{"protocol":1,"session":"%s","pid":%s,"version":"\(version)","state":"ready"}' "$MUSIC_MANAGER_STARTUP_SESSION" "$$" > "$MUSIC_MANAGER_STARTUP_REPORT"
        exec /bin/sleep 10
        """
        try Data(script.utf8).write(to: executable)
        try fm.setAttributes([.posixPermissions: 0o755], ofItemAtPath: executable.path)
    }
    func verifyLibrary() throws { try check(try String(contentsOf: sentinel, encoding: .utf8) == "music library must survive", "library modified") }
    deinit {
        engine?.launchedProcess?.terminate()
        engine = nil
        for directory in io.protectedDirectories {
            try? fm.setAttributes([.posixPermissions: 0o700], ofItemAtPath: directory.path)
        }
        try? fm.removeItem(at: root)
    }
}

do {
    try test("a protected backup ancestor requests authorization while staging stays unprivileged") {
        let f = try Fixture()
        let backupRoot = f.destination.deletingLastPathComponent().appendingPathComponent(".music-manager-backups")
        try fm.createDirectory(at: backupRoot, withIntermediateDirectories: true)
        try fm.setAttributes([.posixPermissions: 0o555], ofItemAtPath: backupRoot.path)
        f.io.protectedDirectories = [backupRoot]
        try check(fm.isWritableFile(atPath: f.destination.deletingLastPathComponent().path), "application parent should be writable")
        try check(!fm.isWritableFile(atPath: backupRoot.path), "backup ancestor should require authorization")
        try f.engine!.install()
        let mkdir = f.io.operations.filter { $0.executable == "/bin/mkdir" }
        try check(mkdir.count == 1 && mkdir[0].privileged, "backup creation did not request authorization")
        try check(f.io.operations.filter { $0.executable == "/usr/bin/ditto" }.allSatisfy { !$0.privileged }, "staging requested unnecessary privileges")
        try check(try AppInfo.read(f.destination).version == "0.2.6", "new app not installed")
        try check(try AppInfo.read(URL(fileURLWithPath: f.engine!.receipt!.backup)).version == "0.2.5", "old backup lost")
        try f.verifyLibrary()
    }
    try test("recovery checks both source and destination directory permissions") {
        let f = try Fixture(); try f.engine!.install()
        let backupParent = URL(fileURLWithPath: f.engine!.receipt!.backup).deletingLastPathComponent()
        try fm.setAttributes([.posixPermissions: 0o555], ofItemAtPath: backupParent.path)
        f.io.protectedDirectories = [backupParent]
        f.io.operations.removeAll()
        try f.engine!.restore()
        let moves = f.io.operations.filter { $0.executable == "/bin/mv" }
        try check(moves.count == 2 && moves.allSatisfy { $0.privileged }, "recovery did not authorize both rename directions")
        try check(f.io.operations.filter { $0.arguments.first == "--installer-check-rollback" }.allSatisfy { !$0.privileged }, "database preflight must run as the current user")
        try check(try AppInfo.read(f.destination).version == "0.2.5", "old app not restored")
        try f.verifyLibrary()
    }
    try test("cancelled backup authorization preserves the installed app and library") {
        let f = try Fixture()
        let backupRoot = f.destination.deletingLastPathComponent().appendingPathComponent(".music-manager-backups")
        try fm.createDirectory(at: backupRoot, withIntermediateDirectories: true)
        try fm.setAttributes([.posixPermissions: 0o555], ofItemAtPath: backupRoot.path)
        f.io.protectedDirectories = [backupRoot]
        f.io.denyAuthorization = true
        try fails("authorization cancelled") { try f.engine!.install() }
        try check(f.io.operations.contains { $0.executable == "/bin/mkdir" && $0.privileged }, "never requested the required authorization")
        try check(!f.io.operations.contains { $0.executable == "/bin/mv" }, "renamed an app after authorization was cancelled")
        try check(try AppInfo.read(f.destination).version == "0.2.5", "old app changed after cancellation")
        try f.verifyLibrary()
    }
    try test("writable installation and backup directories need no authorization") {
        let f = try Fixture(); try f.engine!.install(); try f.engine!.restore()
        try check(f.io.operations.allSatisfy { !$0.privileged }, "requested unnecessary administrator privileges")
        try check(try AppInfo.read(f.destination).version == "0.2.5", "old app not restored")
        try f.verifyLibrary()
    }
    try test("damaged published app restores via a verified payload checker") {
        let f = try Fixture()
        f.io.invalidSignatures.insert(f.destination.path)
        try f.reopen(systemChecks: true)
        try fails("post-rename signature check") { try f.engine!.install() }
        try check(f.engine!.receipt!.phase == "restored", "signature failure stranded the old app")
        try check(try AppInfo.read(f.destination).version == "0.2.5", "old app not restored")
        try check(f.io.rollbackExecutable == f.source.appendingPathComponent("Contents/MacOS/music-manager").path, "did not use the verified source checker")
        try f.verifyLibrary()
    }
    try test("fallback checker still refuses an incompatible database") {
        let f = try Fixture()
        f.io.invalidSignatures.insert(f.destination.path)
        f.io.compatible = false
        try f.reopen(systemChecks: true)
        try fails("incompatible automatic recovery") { try f.engine!.install() }
        try fails("incompatible manual recovery") { try f.engine!.restore() }
        try check(f.io.rollbackChecked, "fallback skipped the database check")
        try check(fm.fileExists(atPath: f.engine!.receipt!.backup), "old backup lost")
        try check(try AppInfo.read(f.destination).version == "0.2.6", "incompatible app was restored")
        try f.verifyLibrary()
    }
    try test("manual recovery refuses to execute any invalid checker") {
        let f = try Fixture(); try f.engine!.install()
        f.io.invalidSignatures.formUnion([f.destination.path, f.source.path])
        try f.reopen(systemChecks: true)
        try fails("no verified checker") { try f.engine!.restore() }
        try check(!f.io.rollbackChecked, "executed an invalid checker")
        try check(fm.fileExists(atPath: f.engine!.receipt!.backup), "old backup lost")
        try f.verifyLibrary()
    }
    for phase in ["installed", "starting", "replacing"] {
        try test("higher repair release supersedes \(phase) without deleting the original backup") {
            let f = try Fixture(old: "0.2.2"); try f.engine!.install()
            f.engine!.receipt!.phase = phase
            try f.engine!.save()
            let previous = f.engine!.receipt!
            try fails("unsafe legacy rollback") { try f.engine!.restore() }
            try f.app(f.source, version: "0.2.7", schema: 3)
            try f.reopen()
            try f.engine!.validateInAppUpdate(version: "0.2.7")
            try f.engine!.install()
            try check(try AppInfo.read(f.destination).version == "0.2.7", "repair release blocked")
            try check(try AppInfo.read(URL(fileURLWithPath: previous.backup)).version == "0.2.2", "original backup lost")
            try check(try AppInfo.read(URL(fileURLWithPath: f.engine!.receipt!.backup)).version == "0.2.6", "intermediate backup lost")
            let archive = f.logs.appendingPathComponent("transactions/\(previous.session).json")
            let archived = try JSONDecoder().decode(InstallReceipt.self, from: Data(contentsOf: archive))
            try check(archived.backup == previous.backup && archived.phase == phase, "previous recovery record lost")
            try f.verifyLibrary()
        }
    }
    for phase in ["completed", "restored", "failed_before_replace"] {
        try test("relocated application archives a terminal \(phase) receipt and remains updateable") {
            let f = try Fixture(); try f.engine!.install()
            f.engine!.receipt!.phase = phase
            try f.engine!.save()
            let previous = f.engine!.receipt!
            f.engine = nil
            let moved = f.root.appendingPathComponent("OtherApplications/声场档案.app")
            try fm.createDirectory(at: moved.deletingLastPathComponent(), withIntermediateDirectories: true)
            try fm.moveItem(at: f.destination, to: moved)
            try f.app(f.source, version: "0.2.7", schema: 3)
            f.engine = try Installer(source: f.source, destination: moved, logRoot: f.logs, io: f.io, systemChecks: false)
            try check(f.engine!.recoveryError == nil && f.engine!.receipt == nil, "terminal receipt blocked the new location")
            try f.engine!.validateInAppUpdate(version: "0.2.7")
            try f.engine!.install()
            try check(try AppInfo.read(moved).version == "0.2.7", "relocated installation not updated")
            try check(!fm.fileExists(atPath: f.destination.path), "recreated the abandoned installation")
            try check(fm.fileExists(atPath: previous.backup), "original backup removed")
            try check(fm.fileExists(atPath: f.logs.appendingPathComponent("transactions/\(previous.session).json").path), "recovery record removed")
            try f.verifyLibrary()
        }
    }
    try test("relocation cannot discard an unfinished recovery transaction") {
        let f = try Fixture(); try f.engine!.install()
        f.engine = nil
        let moved = f.root.appendingPathComponent("OtherApplications/声场档案.app")
        let other = try Installer(source: f.source, destination: moved, logRoot: f.logs, io: f.io, systemChecks: false)
        try check(other.recoveryError != nil, "unfinished transaction ignored")
        try fails("unfinished transaction") { try other.install() }
        try f.verifyLibrary()
    }
    for viaInstall in [false, true] {
        try test("published legacy upgrade resumes through \(viaInstall ? "install" : "retry") and confirms real startup") {
            let f = try Fixture(old: "0.2.2"); try f.engine!.install()
            let original = f.engine!.receipt!
            f.engine!.receipt!.phase = "replacing"; try f.engine!.save()
            try f.readyExecutable(version: original.version)
            try f.reopen(systemChecks: true)
            try check(f.engine!.canRetry, "retry disabled for published replacement")
            if viaInstall { try f.engine!.install() }
            try check(try f.engine!.launch(timeout: 5) == .ready, "startup not confirmed")
            try check(f.engine!.receipt!.phase == "completed", "transaction still blocked")
            try check(f.engine!.receipt!.session == original.session && f.io.copies == 1, "created another replacement")
            try check(try AppInfo.read(URL(fileURLWithPath: original.backup)).version == "0.2.2", "legacy backup lost")
            f.engine!.launchedProcess?.terminate()
            f.engine!.launchedProcess?.waitUntilExit()
            try fails("legacy safety must still apply") { try f.engine!.restore() }
            try f.app(f.source, version: "0.2.7", schema: 3)
            try f.engine!.validateInAppUpdate(version: "0.2.7")
            try f.verifyLibrary()
        }
    }
    for invalid in ["signature", "version", "identity", "protocol"] {
        try test("published replacement with invalid \(invalid) cannot resume") {
            let f = try Fixture(); try f.engine!.install()
            f.engine!.receipt!.phase = "replacing"; try f.engine!.save()
            if invalid == "signature" { f.io.invalidSignatures.insert(f.destination.path) }
            else {
                let plist = f.destination.appendingPathComponent("Contents/Info.plist")
                var info = try PropertyListSerialization.propertyList(from: Data(contentsOf: plist), format: nil) as! [String: Any]
                if invalid == "version" { info["CFBundleShortVersionString"] = "0.2.5" }
                if invalid == "identity" { info["CFBundleIdentifier"] = "unrelated.app" }
                if invalid == "protocol" { info["MusicManagerStartupProtocol"] = 0 }
                try PropertyListSerialization.data(fromPropertyList: info, format: .xml, options: 0).write(to: plist)
            }
            try f.reopen(systemChecks: true)
            try fails("install resume") { try f.engine!.install() }
            try fails("launch resume") { _ = try f.engine!.launch() }
            try check(f.engine!.receipt!.phase == "replacing" && f.engine!.launchedProcess == nil, "invalid app accepted")
            try check(f.io.copies == 1 && f.io.stopped == 1, "failed verification modified running app")
            try f.verifyLibrary()
        }
    }
    try test("staged same-version replacement is not mistaken for a published app") {
        let f = try Fixture(old: "0.2.6"); try f.engine!.install()
        let r = f.engine!.receipt!
        try fm.moveItem(atPath: f.destination.path, toPath: r.staging)
        try fm.copyItem(atPath: r.backup, toPath: f.destination.path)
        f.engine!.receipt!.phase = "replacing"; try f.engine!.save(); try f.reopen()
        try check(!f.engine!.canRetry, "old app became eligible for startup confirmation")
        try fails("still staged") { _ = try f.engine!.launch() }
        try fails("new install") { try f.engine!.install() }
        try check(f.engine!.receipt!.phase == "replacing", "unfinished replacement promoted")
    }
    for mode in ["available", "missing", "incompatible", "interrupted"] {
        try test("Intel backup recovery on Apple Silicon: \(mode)") {
            let f = try Fixture(); try f.engine!.install()
            let r = f.engine!.receipt!
            f.io.architectures[r.backup + "/Contents/MacOS/music-manager"] = "x86_64"
            f.io.rosettaAvailable = mode != "missing"
            f.io.compatible = mode != "incompatible"
            try f.reopen(systemChecks: true)
            if mode == "missing" || mode == "incompatible" {
                try fails("unsafe restoration") { try f.engine!.restore() }
                try check(fm.fileExists(atPath: r.backup), "backup removed")
                try check(try AppInfo.read(f.destination).version == r.version, "new app displaced")
                try check(f.io.rollbackChecked == (mode == "incompatible"), "wrong compatibility check order")
            } else {
                if mode == "interrupted" {
                    f.io.failAfterMoveSource = r.backup
                    try fails("after old app moved back") { try f.engine!.restore() }
                    f.io.failAfterMoveSource = nil
                    f.io.architectures[f.destination.path + "/Contents/MacOS/music-manager"] = "x86_64"
                    try f.reopen(systemChecks: true)
                }
                try f.engine!.restore()
                try check(f.engine!.receipt!.phase == "restored", "recovery not completed")
                try check(try AppInfo.read(f.destination).version == "0.2.5", "Intel backup not restored")
                try check(f.io.rollbackChecked, "database compatibility bypassed")
            }
            try check(f.io.translationProbes == (mode == "interrupted" ? 2 : 1), "translation not verified")
            try f.verifyLibrary()
        }
    }
    try test("Intel update packages still require native architecture on Apple Silicon") {
        let f = try Fixture()
        f.io.architectures[f.source.path + "/Contents/MacOS/music-manager"] = "x86_64"
        try f.reopen(systemChecks: true)
        try fails("non-native update") { try f.engine!.install() }
        try check(f.io.copies == 0 && f.io.translationProbes == 0, "native update requirement weakened")
    }
    try test("Intel hardware cannot restore an ARM-only backup") {
        let f = try Fixture(); try f.engine!.install()
        f.io.appleSilicon = false
        try f.reopen(systemChecks: true)
        try fails("ARM backup on Intel") { try f.engine!.restore() }
        try check(f.io.translationProbes == 0, "Rosetta used in the wrong direction")
        try check(fm.fileExists(atPath: f.engine!.receipt!.backup), "backup lost")
        try f.verifyLibrary()
    }
    try test("in-app handoff validates arguments and acknowledges the exact request") {
        let f = try Fixture()
        let reply = f.root.appendingPathComponent("helper-test/handoff.json")
        try fm.createDirectory(at: reply.deletingLastPathComponent(), withIntermediateDirectories: true)
        let args = ["--update-destination", f.destination.path, "--parent-pid", "99999",
                    "--expected-version", "0.2.6", "--handoff", reply.path, "--token", "helper-test"]
        let request = try InAppUpdateRequest.parse(args, allowedDestinations: [f.destination])!
        try request.respond()
        let response = try JSONSerialization.jsonObject(with: Data(contentsOf: reply)) as! [String: Any]
        try check(response["token"] as? String == "helper-test", "wrong handoff token")
        try check(response["version"] as? String == "0.2.6", "wrong handoff version")
        try check(response["pid"] as? Int32 == ProcessInfo.processInfo.processIdentifier, "wrong helper PID")
        try fails("unapproved destination") { _ = try InAppUpdateRequest.parse(args, allowedDestinations: []) }
        try fails("incomplete request") { _ = try InAppUpdateRequest.parse(Array(args.dropLast()), allowedDestinations: [f.destination]) }
        try fails("invalid token") {
            var invalid = args; invalid[invalid.count - 1] = "helper-other"
            _ = try InAppUpdateRequest.parse(invalid, allowedDestinations: [f.destination])
        }
        try check(try InAppUpdateRequest.parse([], allowedDestinations: []) == nil, "manual installer treated as auto update")
        try check(try InAppUpdateRequest.parse(["-psn_0_12345"], allowedDestinations: []) == nil, "Finder launch arguments rejected")
        try f.verifyLibrary()
    }
    try test("in-app updater cannot continue while its parent is running") {
        let f = try Fixture()
        let request = InAppUpdateRequest(destination: f.destination, parentPID: 99999, version: "0.2.6",
                                         reply: f.root.appendingPathComponent("handoff.json"), token: "helper-test")
        try fails("parent still running") { try request.waitForParentExit(timeout: 0, isRunning: { _ in true }) }
        try request.waitForParentExit(timeout: 0, isRunning: { _ in false })
        try check(f.io.copies == 0 && f.io.stopped == 0, "waiting changed the app")
        try f.verifyLibrary()
    }
    try test("in-app preflight fails before quitting the current app") {
        let f = try Fixture()
        try f.engine!.validateInAppUpdate(version: "0.2.6")
        try fails("wrong release") { try f.engine!.validateInAppUpdate(version: "0.2.7") }
        try check(f.io.stopped == 0 && f.io.copies == 0, "preflight changed current app")
        try f.engine!.install()
        try fails("unfinished transaction") { try f.engine!.validateInAppUpdate(version: "0.2.6") }
        try f.verifyLibrary()
    }
    try test("copy failure leaves old app and library untouched") {
        let f = try Fixture(); f.io.failCopy = true
        try fails("copy") { try f.engine!.install() }
        try check(try AppInfo.read(f.destination).version == "0.2.5", "old version lost")
        try check(f.io.stopped == 0, "old app was stopped before staging")
        try f.verifyLibrary()
    }
    try test("failed replacement rename automatically restores old app") {
        let f = try Fixture(); f.io.failNewMove = true
        try fails("rename") { try f.engine!.install() }
        try check(try AppInfo.read(f.destination).version == "0.2.5", "old app not restored")
        try check(f.engine!.receipt!.phase == "restored", "receipt not updated")
        try f.verifyLibrary()
    }
    try test("running app blocks replacement without forcing termination") {
        let f = try Fixture(); f.io.busy = true
        try fails("busy app") { try f.engine!.install() }
        try check(try AppInfo.read(f.destination).version == "0.2.5", "active binary replaced")
    }
    try test("successful replacement retains full old app and records new version") {
        let f = try Fixture(); try f.engine!.install()
        try check(try AppInfo.read(f.destination).version == "0.2.6", "new version not installed")
        try check(try AppInfo.read(URL(fileURLWithPath: f.engine!.receipt!.backup)).version == "0.2.5", "backup invalid")
        try f.verifyLibrary()
    }
    try test("fresh install has no backup to restore") {
        let f = try Fixture(old: nil); try f.engine!.install()
        try fails("no backup") { try f.engine!.restore() }
        try check(try AppInfo.read(f.destination).version == "0.2.6", "fresh install lost")
    }
    try test("downgrade is blocked before any copying") {
        let f = try Fixture(old: "0.2.7")
        try fails("downgrade") { try f.engine!.install() }
        try check(f.io.copies == 0, "downgrade copied payload")
    }
    try test("incompatible database blocks rollback after launch") {
        let f = try Fixture(); try f.engine!.install()
        f.engine!.receipt!.launched = true; f.io.compatible = false
        try fails("schema mismatch") { try f.engine!.restore() }
        try check(f.io.rollbackChecked, "did not check schema")
        try check(try AppInfo.read(f.destination).version == "0.2.6", "incompatible old app restored")
        try f.verifyLibrary()
    }
    try test("compatible rollback retains displaced new app and preserves library") {
        let f = try Fixture(); try f.engine!.install()
        f.engine!.receipt!.launched = true
        try f.engine!.restore()
        try check(f.io.rollbackChecked, "no compatibility preflight")
        try check(try AppInfo.read(f.destination).version == "0.2.5", "wrong rollback version")
        try f.verifyLibrary()
    }
    try test("unsafe legacy app cannot be restored after opening a library") {
        let f = try Fixture(old: "0.2.2"); try f.engine!.install()
        f.engine!.receipt!.launched = true
        try fails("unknown old schema and startup behavior") { try f.engine!.restore() }
        try check(try AppInfo.read(f.destination).version == "0.2.6", "unsafe old version restored")
    }
    try test("crash between rename steps can recover on next installer run") {
        let f = try Fixture(); try f.engine!.install()
        let r = f.engine!.receipt!
        try fm.removeItem(at: f.destination) // Simulate interruption before staged app became destination.
        try f.reopen(); try f.engine!.restore()
        try check(try AppInfo.read(f.destination).version == "0.2.5", "interrupted transaction not recovered")
        try check(!fm.fileExists(atPath: r.backup), "backup was not moved back")
        try f.verifyLibrary()
    }
    try test("a second installer cannot acquire the active transaction lock") {
        let f = try Fixture()
        try fails("lock") { _ = try Installer(source: f.source, destination: f.destination, logRoot: f.logs, io: f.io, systemChecks: false) }
    }
    try test("damaged transaction blocks replacement but keeps diagnostics available") {
        let f = try Fixture()
        try Data("broken".utf8).write(to: f.logs.appendingPathComponent("latest.json"))
        try f.reopen()
        try check(f.engine!.recoveryError != nil, "damaged record ignored")
        try fails("damaged record") { try f.engine!.install() }
        try check(f.io.copies == 0, "app replaced despite damaged record")
        try f.verifyLibrary()
    }
    try test("wrong bundle identity is rejected before replacement") {
        let f = try Fixture()
        let plist = f.source.appendingPathComponent("Contents/Info.plist")
        var d = try PropertyListSerialization.propertyList(from: Data(contentsOf: plist), format: nil) as! [String: Any]
        d["CFBundleIdentifier"] = "someone.else"
        try PropertyListSerialization.data(fromPropertyList: d, format: .xml, options: 0).write(to: plist)
        try fails("identity") { try f.engine!.install() }
        try check(f.io.copies == 0, "unrelated app copied")
    }
    func report(_ state: String, session: String = "current", version: String = "0.2.6", pid: Int = 123) throws -> Data {
        try JSONSerialization.data(withJSONObject: ["protocol":1,"session":session,"version":version,"pid":pid,"state":state])
    }
    try test("waiting for a library or drive is a healthy, distinct startup outcome") {
        try check(try evaluateStartup(report("waiting_library"), session: "current", version: "0.2.6", pid: 123, running: true) == .waitingLibrary, "missing library not recognized")
        try check(try evaluateStartup(report("library_unavailable"), session: "current", version: "0.2.6", pid: 123, running: true) == .libraryUnavailable, "offline drive treated as crash")
    }
    try test("stale status, wrong version, wrong process cannot mask failure") {
        for data in [try report("ready", session: "old"), try report("ready", version: "0.2.5"), try report("ready", pid: 12)] {
            try check(try evaluateStartup(data, session: "current", version: "0.2.6", pid: 123, running: true) == nil, "stale report accepted")
            try fails("process exited") { _ = try evaluateStartup(data, session: "current", version: "0.2.6", pid: 123, running: false) }
        }
    }
    try test("process exit invalidates even a prior ready report") {
        try fails("exit after ready") { _ = try evaluateStartup(report("ready"), session: "current", version: "0.2.6", pid: 123, running: false) }
        try check(try evaluateStartup(report("cancelled"), session: "current", version: "0.2.6", pid: 123, running: false) == .cancelled, "user exit treated as crash")
    }
    try test("early executable exit is detected by real process monitoring") {
        let f = try Fixture(); try f.engine!.install()
        try fm.setAttributes([.posixPermissions: 0o755], ofItemAtPath: f.destination.appendingPathComponent("Contents/MacOS/music-manager").path)
        try fails("real child exited") { _ = try f.engine!.launch(timeout: 2) }
        try check(f.engine!.receipt!.phase == "starting", "false success recorded")
        try f.verifyLibrary()
    }
    for afterOldRestored in [false, true] {
        try test("interrupted restore resumes after \(afterOldRestored ? "old app returned" : "new app moved aside")") {
            let f = try Fixture(); try f.engine!.install()
            f.engine!.receipt!.launched = true
            try f.engine!.save()
            f.io.failAfterMoveSource = afterOldRestored ? f.engine!.receipt!.backup : f.destination.path
            try fails("simulated power failure") { try f.engine!.restore() }
            try check(f.engine!.receipt!.phase == "restoring", "restore was not journaled")
            let displaced = f.engine!.displacedApplication(f.engine!.receipt!)
            try check(fm.fileExists(atPath: displaced.path), "new app not retained")
            f.io.failAfterMoveSource = nil
            try f.reopen()
            try check(f.engine!.canRestore && !f.engine!.canRetry, "incorrect buttons after interruption")
            f.io.rollbackChecked = false
            try f.engine!.restore()
            try check(try AppInfo.read(f.destination).version == "0.2.5", "old app not restored")
            try check(f.engine!.receipt!.phase == "restored", "completion missing")
            if !afterOldRestored {
                try check(f.io.rollbackExecutable == displaced.appendingPathComponent("Contents/MacOS/music-manager").path,
                          "did not use displaced new executable")
                try check(f.io.rollbackChecked, "resume failed to recheck compatibility")
            }
            try f.verifyLibrary()
        }
    }
    try test("normal install can continue after interrupted restore") {
        let f = try Fixture(); try f.engine!.install()
        f.io.failAfterMoveSource = f.destination.path
        try fails("interruption") { try f.engine!.restore() }
        f.io.failAfterMoveSource = nil
        try f.reopen(); try f.engine!.install()
        try check(try AppInfo.read(f.destination).version == "0.2.6", "install did not resume")
        try check(f.engine!.receipt!.previousVersion == "0.2.5", "lost recovered backup")
        try f.verifyLibrary()
    }
    try test("resume refuses rollback if library compatibility changed") {
        let f = try Fixture(); try f.engine!.install()
        f.io.failAfterMoveSource = f.destination.path
        try fails("interruption") { try f.engine!.restore() }
        f.io.failAfterMoveSource = nil; f.io.compatible = false
        try f.reopen()
        try fails("changed schema") { try f.engine!.restore() }
        try check(fm.fileExists(atPath: f.engine!.receipt!.backup), "backup lost")
        try check(fm.fileExists(atPath: f.engine!.displacedApplication(f.engine!.receipt!).path), "new app lost")
        f.io.compatible = true
        try f.engine!.restore()
        try f.verifyLibrary()
    }
    for phase in ["installed", "replacing"] {
        try test("manual Finder launch cannot bypass compatibility in \(phase) phase") {
            let f = try Fixture(); try f.engine!.install()
            // The new binary is visible, but the installer hasn't recorded its own launch.
            f.engine!.receipt!.phase = phase
            try f.engine!.save()
            f.io.compatible = false
            try f.reopen()
            try check(!f.engine!.receipt!.launched, "fixture unexpectedly launched")
            try fails("incompatible manual launch") { try f.engine!.restore() }
            try check(f.io.rollbackChecked, "skipped compatibility check")
            try check(try AppInfo.read(f.destination).version == "0.2.6", "unsafe rollback")
            try f.verifyLibrary()
        }
    }
    try test("unsafe legacy backup cannot be restored after a manual launch") {
        let f = try Fixture(old: "0.2.2"); try f.engine!.install(); try f.reopen()
        try fails("unsafe legacy backup") { try f.engine!.restore() }
        try check(try AppInfo.read(f.destination).version == "0.2.6", "unsafe old version restored")
        try f.verifyLibrary()
    }
    try test("unpublished replacement can still recover an old legacy app") {
        let f = try Fixture(old: "0.2.2"); f.io.failNewMove = true
        try fails("replacement failed before publication") { try f.engine!.install() }
        try check(try AppInfo.read(f.destination).version == "0.2.2", "original app stranded")
        try check(!f.io.rollbackChecked, "unpublished update needlessly opened library")
        try f.verifyLibrary()
    }
    try test("unpublished recovery survives a missing final completion record") {
        let f = try Fixture(old: "0.2.2"); try f.engine!.install()
        // Model the window before publication: new app still staged, old app in backup.
        try fm.moveItem(atPath: f.destination.path, toPath: f.engine!.receipt!.staging)
        f.engine!.receipt!.phase = "replacing"; try f.engine!.save()
        f.io.failAfterMoveSource = f.engine!.receipt!.backup
        try fails("old app restored but receipt not saved") { try f.engine!.restore() }
        f.io.failAfterMoveSource = nil
        try f.reopen()
        try check(f.engine!.canRestore, "completion recovery button missing")
        try f.engine!.restore()
        try check(try AppInfo.read(f.destination).version == "0.2.2", "original app not recovered")
        try check(!f.io.rollbackChecked, "opened database for unpublished update")
        try f.verifyLibrary()
    }
    try test("retry and reopen retain all failure logs in exported ZIP") {
        let f = try Fixture(); try f.engine!.install()
        let executable = f.destination.appendingPathComponent("Contents/MacOS/music-manager")
        let markers = ["FIRST_FAILURE", "SECOND_FAILURE", "AFTER_REOPEN_FAILURE"]
        for (index, marker) in markers.enumerated() {
            if index == 2 { try f.reopen() }
            try Data("#!/bin/sh\necho \(marker) >&2\nexit 42\n".utf8).write(to: executable)
            try fm.setAttributes([.posixPermissions: 0o755], ofItemAtPath: executable.path)
            try fails("real startup failure") { _ = try f.engine!.launch(timeout: 2) }
        }
        let attempts = try fm.contentsOfDirectory(at: f.engine!.attemptsDirectory(f.engine!.receipt!), includingPropertiesForKeys: nil)
        try check(attempts.count == 3, "attempt records overwritten")
        // Export must keep an allowlist, even if unrelated files exist in an attempt directory.
        try Data("never export".utf8).write(to: attempts[0].appendingPathComponent("library.db"))
        try f.reopen()
        let zip = f.root.appendingPathComponent("diagnostics.zip")
        try f.engine!.exportDiagnostics(to: zip)
        let extracted = f.root.appendingPathComponent("exported")
        _ = try SystemIO(log: { _ in }).run("/usr/bin/ditto", ["-x", "-k", zip.path, extracted.path], privileged: false)
        let entries = fm.enumerator(at: extracted, includingPropertiesForKeys: nil)!.allObjects as! [URL]
        let applicationLogs = entries.filter { $0.lastPathComponent == "application.log" }
        try check(applicationLogs.count == 3, "export dropped prior attempts")
        let content = try applicationLogs.map { try String(contentsOf: $0, encoding: .utf8) }.joined()
        for marker in markers { try check(content.contains(marker), "lost failure: " + marker) }
        try check(entries.filter { $0.lastPathComponent == "error.txt" }.count == 3, "missing per-attempt errors")
        try check(!entries.contains { $0.lastPathComponent == "library.db" }, "database included in ZIP")
        try f.verifyLibrary()
    }
    print("\(passed) installer tests passed")
} catch {
    fputs("\(error.localizedDescription)\n", stderr)
    exit(1)
}
