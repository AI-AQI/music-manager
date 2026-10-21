import Foundation
import AppKit
import Darwin

struct InstallError: LocalizedError {
    let message: String
    init(_ message: String) { self.message = message }
    var errorDescription: String? { message }
}

struct InAppUpdateRequest {
    let destination: URL
    let parentPID: Int32
    let version: String
    let reply: URL
    let token: String

    static func parse(_ args: [String], allowedDestinations: [URL]) throws -> InAppUpdateRequest? {
        let keys = ["--update-destination", "--parent-pid", "--expected-version", "--handoff", "--token"]
        // Finder may add its own launch arguments; only our explicit flags request an update.
        if !args.contains(where: { keys.contains($0) }) { return nil }
        guard args.count == keys.count * 2 else { throw InstallError("应用内更新参数不完整。") }
        var values: [String: String] = [:]
        for index in stride(from: 0, to: args.count, by: 2) {
            guard keys.contains(args[index]), values[args[index]] == nil else { throw InstallError("应用内更新参数无效。") }
            values[args[index]] = args[index + 1]
        }
        guard let path = values["--update-destination"], path.hasPrefix("/"),
              allowedDestinations.contains(where: { $0.path == path }),
              let pid = Int32(values["--parent-pid"] ?? ""), pid > 1, pid != ProcessInfo.processInfo.processIdentifier,
              let version = values["--expected-version"], version.range(of: #"^\d+\.\d+\.\d+$"#, options: .regularExpression) != nil,
              let replyPath = values["--handoff"], replyPath.hasPrefix("/"),
              let token = values["--token"], token.range(of: #"^helper-[a-zA-Z0-9_-]+$"#, options: .regularExpression) != nil else {
            throw InstallError("无法确认本次更新的应用位置或来源参数。")
        }
        let reply = URL(fileURLWithPath: replyPath)
        guard reply.lastPathComponent == "handoff.json", reply.deletingLastPathComponent().lastPathComponent == token else {
            throw InstallError("更新交接位置无效。")
        }
        return InAppUpdateRequest(destination: URL(fileURLWithPath: path), parentPID: pid, version: version, reply: reply, token: token)
    }
    func respond(error: String? = nil) throws {
        var data: [String: Any] = ["token": token, "version": version, "pid": ProcessInfo.processInfo.processIdentifier]
        if let error = error { data["error"] = error }
        try JSONSerialization.data(withJSONObject: data).write(to: reply, options: .atomic)
    }
    func waitForParentExit(timeout: TimeInterval = 30, isRunning: (Int32) -> Bool = { Darwin.kill($0, 0) == 0 || errno == EPERM }) throws {
        let deadline = Date().addingTimeInterval(timeout)
        while isRunning(parentPID) && Date() < deadline { Thread.sleep(forTimeInterval: 0.1) }
        guard !isRunning(parentPID) else {
            throw InstallError("声场档案尚未退出，更新未开始。请退出应用后再点击“安装 / 更新”。")
        }
    }
}

struct AppInfo {
    let version: String
    let schema: Int?
    let protocolVersion: Int?
    let minimumOS: String
    static func read(_ app: URL, identifier: String = "com.musicmanager.app") throws -> AppInfo {
        let file = app.appendingPathComponent("Contents/Info.plist")
        guard let dict = try PropertyListSerialization.propertyList(from: Data(contentsOf: file), format: nil) as? [String: Any],
              dict["CFBundleIdentifier"] as? String == identifier,
              dict["CFBundleExecutable"] as? String == "music-manager",
              let version = dict["CFBundleShortVersionString"] as? String,
              version.range(of: #"^\d+\.\d+\.\d+$"#, options: .regularExpression) != nil else {
            throw InstallError("安装包中的应用身份或版本无效，请重新下载官方安装包。")
        }
        return AppInfo(version: version, schema: dict["MusicManagerDatabaseSchema"] as? Int,
                       protocolVersion: dict["MusicManagerStartupProtocol"] as? Int,
                       minimumOS: dict["LSMinimumSystemVersion"] as? String ?? "10.15")
    }
    var rollbackSchema: Int? {
        // 0.2.5 is the first released version that refuses to create a replacement
        // library on a missing drive. Earlier bundles cannot safely be auto-restored.
        schema ?? (version == "0.2.5" ? 3 : nil)
    }
}

struct InstallReceipt: Codable {
    let session: String
    let destination: String
    let backup: String
    let staging: String
    let version: String
    let previousVersion: String?
    let previousSchema: Int?
    var diagnosticsSession: String
    var phase: String
    var launched: Bool
    var finished: Bool { ["completed", "restored", "failed_before_replace"].contains(phase) }
}

struct StartupReport: Decodable {
    let `protocol`: Int
    let session: String
    let pid: Int32
    let version: String
    let state: String
}

enum StartupResult: Equatable {
    case ready, waitingLibrary, libraryUnavailable, cancelled
    var message: String {
        switch self {
        case .ready: return "更新完成，音乐库界面已就绪。"
        case .waitingLibrary: return "安装完成。请在声场档案窗口中选择已有音乐库或创建新库。"
        case .libraryUnavailable: return "应用已启动，但音乐库暂不可用。请到声场档案窗口连接硬盘或查看具体提示；无需重新安装。"
        case .cancelled: return "安装完成。你已退出音乐库选择，下次可直接打开声场档案。"
        }
    }
}

func evaluateStartup(_ data: Data?, session: String, version: String, pid: Int32, running: Bool) throws -> StartupResult? {
    if let data = data, let report = try? JSONDecoder().decode(StartupReport.self, from: data),
       report.protocol == 1, report.session == session, report.version == version, report.pid == pid {
        if report.state == "cancelled" { return .cancelled }
        if report.state == "failed" { throw InstallError("新版启动时发生错误。可重新尝试、恢复旧版或导出诊断信息。") }
        if running {
            switch report.state {
            case "ready": return .ready
            case "waiting_library": return .waitingLibrary
            case "library_unavailable": return .libraryUnavailable
            default: break
            }
        }
    }
    if !running { throw InstallError("新版在启动完成前退出。旧版本备份已保留，可恢复旧版或导出诊断信息。") }
    return nil
}

protocol InstallerIO {
    func run(_ executable: String, _ arguments: [String], privileged: Bool) throws -> String
    func stopApplication(at destination: URL) throws
}

final class SystemIO: InstallerIO {
    let log: (String) -> Void
    let identifier: String
    init(log: @escaping (String) -> Void, identifier: String = "com.musicmanager.app") {
        self.log = log; self.identifier = identifier
    }
    static func shellQuote(_ value: String) -> String { "'" + value.replacingOccurrences(of: "'", with: "'\\''") + "'" }
    func run(_ executable: String, _ arguments: [String], privileged: Bool = false) throws -> String {
        let task = Process()
        let pipe = Pipe()
        if privileged {
            let command = ([executable] + arguments).map(Self.shellQuote).joined(separator: " ")
            task.executableURL = URL(fileURLWithPath: "/usr/bin/osascript")
            task.arguments = ["-e", "on run argv\n do shell script (item 1 of argv) with administrator privileges\nend run", command]
        } else {
            task.executableURL = URL(fileURLWithPath: executable)
            task.arguments = arguments
        }
        task.standardOutput = pipe
        task.standardError = pipe
        log("执行：\(executable) \(arguments.joined(separator: " "))")
        try task.run()
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        task.waitUntilExit()
        let output = String(decoding: data, as: UTF8.self)
        log(output)
        guard task.terminationStatus == 0 else {
            throw InstallError("操作未完成（\(URL(fileURLWithPath: executable).lastPathComponent)，\(task.terminationStatus)）。\n\(String(output.suffix(1500)))\n可以导出诊断信息发给开发者。")
        }
        return output
    }
    func stopApplication(at destination: URL) throws {
        // Include copies launched from an older DMG. Never replace an active executable.
        let apps = NSRunningApplication.runningApplications(withBundleIdentifier: identifier)
        for app in apps { _ = app.terminate() }
        let deadline = Date().addingTimeInterval(15)
        while apps.contains(where: { !$0.isTerminated }) && Date() < deadline { Thread.sleep(forTimeInterval: 0.2) }
        guard apps.allSatisfy({ $0.isTerminated }) else {
            throw InstallError("声场档案仍在运行，可能有尚未完成的操作。请先在应用中退出，再点击重新尝试；不会强制结束进程。")
        }
    }
}

final class Installer {
    static let appName = "声场档案.app"
    let fm = FileManager.default
    let source: URL
    let destination: URL
    let logRoot: URL
    let runDirectory: URL
    let io: InstallerIO
    let progress: (String) -> Void
    var receipt: InstallReceipt?
    private(set) var recoveryError: String?
    var launchedProcess: Process?
    private var lockFD: Int32 = -1
    private var appLog: FileHandle?
    private let systemChecks: Bool
    private let identifier: String

    init(source: URL, destination: URL, logRoot: URL, io: InstallerIO? = nil,
         systemChecks: Bool = true, identifier: String = "com.musicmanager.app",
         progress: @escaping (String) -> Void = { _ in }) throws {
        self.source = source
        self.destination = destination
        self.logRoot = logRoot
        self.progress = progress
        self.systemChecks = systemChecks
        self.identifier = identifier
        runDirectory = logRoot.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try fm.createDirectory(at: runDirectory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        let logFile = runDirectory.appendingPathComponent("installer.log")
        let logLock = NSLock()
        let append: (String) -> Void = { message in
            logLock.lock(); defer { logLock.unlock() }
            let line = "\(ISO8601DateFormatter().string(from: Date())) \(message)\n"
            if !FileManager.default.fileExists(atPath: logFile.path) { FileManager.default.createFile(atPath: logFile.path, contents: nil) }
            if let file = try? FileHandle(forWritingTo: logFile) {
                file.seekToEndOfFile(); file.write(Data(line.utf8)); file.closeFile()
            }
        }
        self.io = io ?? SystemIO(log: append, identifier: identifier)
        append("系统：\(ProcessInfo.processInfo.operatingSystemVersionString)；目标：\(destination.path)")
        lockFD = Darwin.open(logRoot.appendingPathComponent("installer.lock").path, O_CREAT | O_RDWR, 0o600)
        guard lockFD >= 0, flock(lockFD, LOCK_EX | LOCK_NB) == 0 else {
            if lockFD >= 0 { Darwin.close(lockFD); lockFD = -1 }
            throw InstallError("另一个安装窗口正在工作，请返回那个窗口完成操作。")
        }
        if let data = try? Data(contentsOf: logRoot.appendingPathComponent("latest.json")) {
            do {
                receipt = try JSONDecoder().decode(InstallReceipt.self, from: data)
                if let r = receipt, r.destination != destination.path, r.finished {
                    try validateReceipt(at: URL(fileURLWithPath: r.destination))
                    try archiveReceipt()
                    try fm.removeItem(at: logRoot.appendingPathComponent("latest.json"))
                    receipt = nil
                } else {
                    try validateReceipt()
                }
            } catch {
                receipt = nil
                recoveryError = "更新记录无法安全读取，已停止安装和恢复。请导出诊断信息发给开发者。"
                append("更新记录错误：\(error.localizedDescription)")
            }
        }
    }
    deinit {
        appLog?.closeFile()
        if lockFD >= 0 { flock(lockFD, LOCK_UN); Darwin.close(lockFD) }
    }
    func validateReceipt(at location: URL? = nil) throws {
        guard let r = receipt else { return }
        let location = location ?? destination
        let parent = location.deletingLastPathComponent()
        let expectedBackup = parent.appendingPathComponent(".music-manager-backups/\(r.session)/\(Self.appName)").path
        let expectedStaging = parent.appendingPathComponent(".music-manager-stage-\(r.session).app").path
        guard UUID(uuidString: r.session) != nil, r.destination.hasPrefix("/"), r.destination == location.path,
              r.backup == expectedBackup, r.staging == expectedStaging else {
            throw InstallError("更新记录与安装位置不一致，已停止操作。请导出诊断信息。")
        }
    }
    func save() throws {
        let data = try JSONEncoder().encode(receipt)
        try data.write(to: logRoot.appendingPathComponent("latest.json"), options: .atomic)
        try data.write(to: runDirectory.appendingPathComponent("transaction.json"), options: .atomic)
    }
    func archiveReceipt() throws {
        guard let r = receipt else { return }
        let directory = logRoot.appendingPathComponent("transactions")
        try fm.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        try JSONEncoder().encode(r).write(to: directory.appendingPathComponent(r.session + ".json"), options: .atomic)
    }
    func operation(_ executable: String, _ args: [String], writingTo directories: [URL]) throws {
        let privileged = directories.contains { directory in
            // mkdir/ditto may create several missing directories. Check the nearest
            // existing ancestor, including backup directories owned by another user.
            var existing = directory.standardizedFileURL
            while !fm.fileExists(atPath: existing.path) {
                let parent = existing.deletingLastPathComponent()
                if parent == existing { return true }
                existing = parent
            }
            return !fm.isWritableFile(atPath: existing.path) || !fm.isExecutableFile(atPath: existing.path)
        }
        _ = try io.run(executable, args, privileged: privileged)
    }
    var canRetry: Bool {
        guard let r = receipt else { return false }
        if hasPublishedReplacement(r) {
            // Button eligibility only; launch rechecks the full signature before proceeding.
            guard let info = try? AppInfo.read(destination, identifier: identifier) else { return false }
            return info.version == r.version && info.protocolVersion == 1
        }
        return ["installed", "starting", "completed"].contains(r.phase) && fm.fileExists(atPath: r.destination)
    }
    private func hasPublishedReplacement(_ r: InstallReceipt) -> Bool {
        r.phase == "replacing" && fm.fileExists(atPath: r.destination) && !fm.fileExists(atPath: r.staging)
    }
    private func canSupersede(_ r: InstallReceipt, with new: AppInfo) -> Bool {
        guard ["installed", "starting"].contains(r.phase) || hasPublishedReplacement(r),
              new.version.compare(r.version, options: .numeric) == .orderedDescending,
              let current = try? AppInfo.read(destination, identifier: identifier) else { return false }
        return current.version == r.version
    }
    @discardableResult
    private func resumePublishedReplacement() throws -> Bool {
        guard let r = receipt, hasPublishedReplacement(r) else { return false }
        try validateReceipt()
        let info = try verify(destination, expected: r.version)
        guard info.protocolVersion == 1 else { throw InstallError("已就位的应用不支持启动检测，已停止继续更新。") }
        // Recover the rename-before-journal window without making a new backup or
        // marking startup successful. The original backup and transaction stay intact.
        receipt!.phase = "installed"
        try save()
        return true
    }
    var canRestore: Bool {
        guard let r = receipt else { return false }
        return fm.fileExists(atPath: r.backup) || isRestoring(r)
    }
    private func isRestoring(_ r: InstallReceipt) -> Bool {
        ["restoring", "restoring_unpublished"].contains(r.phase)
    }
    func displacedApplication(_ r: InstallReceipt) -> URL {
        // Deterministic within the validated transaction; recoverable after either rename.
        URL(fileURLWithPath: r.backup).deletingLastPathComponent().appendingPathComponent("未启用的新版.app")
    }
    func attemptsDirectory(_ r: InstallReceipt) -> URL {
        logRoot.appendingPathComponent(r.session).appendingPathComponent("attempts")
    }
    func verify(_ app: URL, expected: String? = nil, allowTranslatedBackup: Bool = false) throws -> AppInfo {
        let info = try AppInfo.read(app, identifier: identifier)
        if let expected = expected, info.version != expected { throw InstallError("复制后的应用版本不一致，已停止更新。") }
        if systemChecks {
            _ = try io.run("/usr/bin/codesign", ["--verify", "--deep", "--strict", app.path], privileged: false)
            let binary = app.appendingPathComponent("Contents/MacOS/music-manager")
            let arch = (try? io.run("/usr/sbin/sysctl", ["-n", "hw.optional.arm64"], privileged: false))?.trimmingCharacters(in: .whitespacesAndNewlines) == "1" ? "arm64" : "x86_64"
            // lipo belongs to developer tools on some systems; /usr/bin/file is built in.
            let description = try io.run("/usr/bin/file", ["-b", binary.path], privileged: false)
            if !description.contains(arch) {
                guard allowTranslatedBackup, arch == "arm64", description.contains("x86_64") else {
                    throw InstallError("安装包不适合这台 Mac，请下载\(arch == "arm64" ? "Apple Silicon" : "Intel")版本。")
                }
                // Probe translation with a system binary, not the old app: executing
                // an old app before the schema check could open/migrate the library.
                do {
                    _ = try io.run("/usr/bin/arch", ["-x86_64", "/usr/bin/true"], privileged: false)
                } catch {
                    throw InstallError("旧版是 Intel 应用，但当前无法通过 Rosetta 运行，已停止恢复。新版、旧版备份与音乐库均已保留。")
                }
            }
            let os = ProcessInfo.processInfo.operatingSystemVersion
            let current = "\(os.majorVersion).\(os.minorVersion).\(os.patchVersion)"
            guard current.compare(info.minimumOS, options: .numeric) != .orderedAscending else {
                throw InstallError("此版本需要 macOS \(info.minimumOS) 或更新系统；当前系统为 \(current)。")
            }
        }
        return info
    }
    func validateInAppUpdate(version: String) throws {
        if let error = recoveryError { throw InstallError(error) }
        let new = try verify(source, expected: version)
        if let r = receipt, isRestoring(r) ||
            (["preparing", "replacing", "installed", "starting"].contains(r.phase) && fm.fileExists(atPath: r.backup) && !canSupersede(r, with: new)) {
            throw InstallError("上次更新尚未完成，请先在更新窗口恢复或处理旧版备份。当前应用继续运行。")
        }
        let old = try AppInfo.read(destination, identifier: identifier)
        guard new.protocolVersion == 1, new.version.compare(old.version, options: .numeric) == .orderedDescending else {
            throw InstallError("更新包必须支持启动检测，且版本高于当前已安装版本。")
        }
    }
    func install() throws {
        if let error = recoveryError { throw InstallError(error) }
        let new = try verify(source)
        guard new.protocolVersion == 1 else { throw InstallError("这个安装包不支持启动检测，请下载带有“安装与更新”入口的新安装包。") }
        if let r = receipt, !canSupersede(r, with: new), try resumePublishedReplacement() { return }
        // An interrupted rename can leave no app at the destination. Recover before a new transaction.
        if let r = receipt, isRestoring(r) || (fm.fileExists(atPath: r.backup) && !fm.fileExists(atPath: r.destination)) {
            try restore()
        }
        if let r = receipt, ["preparing", "replacing", "installed", "starting"].contains(r.phase), fm.fileExists(atPath: r.backup), !canSupersede(r, with: new) {
            throw InstallError("上次更新尚未确认完成。请先点击“重新尝试启动”或“恢复旧版”，不要重复替换。")
        }
        progress("正在检查安装包、芯片架构和版本…")
        var old: AppInfo?
        if fm.fileExists(atPath: destination.path) {
            old = try AppInfo.read(destination, identifier: identifier)
            if new.version.compare(old!.version, options: .numeric) == .orderedAscending {
                throw InstallError("已安装 \(old!.version)，安装包为 \(new.version)。普通更新不支持降级，请使用“恢复旧版”。")
            }
        }
        let session = UUID().uuidString
        let parent = destination.deletingLastPathComponent()
        try archiveReceipt()
        receipt = InstallReceipt(session: session, destination: destination.path,
            backup: parent.appendingPathComponent(".music-manager-backups/\(session)/\(Self.appName)").path,
            staging: parent.appendingPathComponent(".music-manager-stage-\(session).app").path,
            version: new.version, previousVersion: old?.version, previousSchema: old?.rollbackSchema,
            diagnosticsSession: runDirectory.lastPathComponent,
            phase: "preparing", launched: false)
        try save()
        progress("正在准备新版 \(new.version)，旧版将保留为备份…")
        do {
            let staging = URL(fileURLWithPath: receipt!.staging)
            let backupParent = URL(fileURLWithPath: receipt!.backup).deletingLastPathComponent()
            try operation("/usr/bin/ditto", [source.path, staging.path], writingTo: [staging])
            _ = try verify(staging, expected: new.version)
            // Removing quarantine is restricted to this verified, newly staged application.
            try operation("/bin/sh", ["-c", "if /usr/bin/xattr -p com.apple.quarantine \"$1\" >/dev/null 2>&1; then /usr/bin/xattr -dr com.apple.quarantine \"$1\"; fi", "installer", staging.path], writingTo: [staging])
            progress("正在退出旧应用并安全替换…")
            try io.stopApplication(at: destination)
            receipt!.phase = "replacing"; try save()
            if old != nil {
                try operation("/bin/mkdir", ["-p", backupParent.path], writingTo: [backupParent])
                try operation("/bin/mv", [destination.path, receipt!.backup], writingTo: [parent, backupParent])
            }
            try operation("/bin/mv", [staging.path, destination.path], writingTo: [staging.deletingLastPathComponent(), parent])
            _ = try verify(destination, expected: new.version)
            receipt!.phase = "installed"; try save()
        } catch {
            let original = error
            if let r = receipt, fm.fileExists(atPath: r.backup) {
                do { try restore() } catch {
                    throw InstallError("更新未完成，自动恢复也未完成。旧版备份仍在：\(r.backup)\n\(error.localizedDescription)\n请点击恢复旧版或导出诊断信息。")
                }
            } else {
                receipt!.phase = "failed_before_replace"; try? save()
            }
            throw original
        }
    }
    func launch(timeout: TimeInterval = 45) throws -> StartupResult {
        try resumePublishedReplacement()
        guard let r = receipt, canRetry else { throw InstallError("请先完成安装或恢复旧版。") }
        _ = try verify(destination, expected: r.version)
        try io.stopApplication(at: destination)
        // A new token and expected PID prevent a previous successful launch masking a crash.
        let token = UUID().uuidString
        let attempt = attemptsDirectory(r).appendingPathComponent(token)
        try fm.createDirectory(at: attempt, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        let status = attempt.appendingPathComponent("startup.json")
        let metadata = ["session": token, "version": r.version, "startedAt": ISO8601DateFormatter().string(from: Date())]
        try JSONEncoder().encode(metadata).write(to: attempt.appendingPathComponent("attempt.json"), options: .atomic)
        let task = Process()
        task.executableURL = destination.appendingPathComponent("Contents/MacOS/music-manager")
        task.currentDirectoryURL = destination.deletingLastPathComponent()
        var environment = ProcessInfo.processInfo.environment
        environment["MUSIC_MANAGER_STARTUP_REPORT"] = status.path
        environment["MUSIC_MANAGER_STARTUP_SESSION"] = token
        task.environment = environment
        let output = attempt.appendingPathComponent("application.log")
        appLog?.closeFile()
        fm.createFile(atPath: output.path, contents: nil, attributes: [.posixPermissions: 0o600])
        appLog = try FileHandle(forWritingTo: output)
        task.standardOutput = appLog; task.standardError = appLog
        receipt!.phase = "starting"; receipt!.launched = true
        receipt!.diagnosticsSession = runDirectory.lastPathComponent
        try save()
        progress("正在启动新版，等待应用回报状态…")
        do {
            try task.run()
            launchedProcess = task
            let deadline = Date().addingTimeInterval(timeout)
            var firstResult: (StartupResult, Date)?
            while Date() < deadline {
                let result = try evaluateStartup(try? Data(contentsOf: status), session: token,
                                                 version: r.version, pid: task.processIdentifier, running: task.isRunning)
                if let result = result {
                    if result == .cancelled || (firstResult?.0 == result && Date().timeIntervalSince(firstResult!.1) >= 2) {
                        receipt!.phase = "completed"; try save(); progress(result.message); return result
                    }
                    if firstResult?.0 != result { firstResult = (result, Date()) }
                } else { firstResult = nil }
                Thread.sleep(forTimeInterval: 0.2)
            }
            throw InstallError("应用仍在运行，但尚未确认启动完成。可先查看应用窗口，再重新尝试启动或导出诊断信息；不会强制结束进程或自动降级。")
        } catch {
            try? error.localizedDescription.write(to: attempt.appendingPathComponent("error.txt"), atomically: true, encoding: .utf8)
            throw error
        }
    }
    func restore() throws {
        guard let r = receipt else { throw InstallError("没有可恢复的旧版备份。") }
        try validateReceipt()
        let backup = URL(fileURLWithPath: r.backup)
        let displaced = displacedApplication(r)
        if !fm.fileExists(atPath: r.backup) {
            // The old app was already moved back, but the completion record wasn't saved.
            guard isRestoring(r), let previous = r.previousVersion else {
                throw InstallError("没有可恢复的旧版备份。")
            }
            let info = try AppInfo.read(destination, identifier: identifier)
            guard info.version == previous else {
                throw InstallError("恢复后的应用与更新记录不一致，已停止操作。")
            }
            if r.phase == "restoring" { _ = try verify(destination, expected: previous, allowTranslatedBackup: true) }
            receipt!.phase = "restored"; try save()
            progress("已确认旧版 \(previous) 恢复完成。音乐库未被替换。")
            return
        }
        let backupInfo = try AppInfo.read(backup, identifier: identifier)
        guard backupInfo.version == r.previousVersion else { throw InstallError("旧版备份与更新记录不一致，已停止恢复。") }
        progress("正在检查旧版和音乐库兼容性…")
        try io.stopApplication(at: destination)
        if let task = launchedProcess, task.isRunning {
            throw InstallError("新版进程还没有退出，请先退出声场档案，再恢复旧版。")
        }
        // Only a staged app that has not been moved into place proves it wasn't published.
        // `launched == false` alone says nothing about launches from Finder.
        let unpublished = r.phase == "restoring_unpublished" ||
            (r.phase == "replacing" && !r.launched &&
             fm.fileExists(atPath: r.staging) && !fm.fileExists(atPath: r.destination))
        if !unpublished {
            _ = try verify(backup, expected: r.previousVersion, allowTranslatedBackup: true)
            guard let supported = backupInfo.rollbackSchema else {
                throw InstallError("旧版缺少可确认的数据库兼容性和启动保护，不能自动恢复。旧版备份与音乐库均已保留，请导出诊断信息。")
            }
            // A damaged installed binary must not prevent checking the untouched DB.
            // Fall back only to an independently verified payload; never skip the check.
            var checker: URL?
            var candidates = [destination]
            if isRestoring(r) { candidates.append(displaced) }
            candidates.append(source)
            for candidate in candidates {
                guard let info = try? verify(candidate), info.protocolVersion == 1,
                      info.version == r.version || (candidate == source && info.version.compare(r.version, options: .numeric) == .orderedDescending) else { continue }
                checker = candidate
                break
            }
            guard let checker = checker else { throw InstallError("没有可安全执行数据库兼容性检查的应用，请重新下载本版或更新版本的安装包。音乐库和旧版备份已保留。") }
            _ = try io.run(checker.appendingPathComponent("Contents/MacOS/music-manager").path,
                           ["--installer-check-rollback", String(supported)], privileged: false)
        }
        let hadNew = fm.fileExists(atPath: destination.path)
        if hadNew && fm.fileExists(atPath: displaced.path) {
            throw InstallError("恢复暂存位置已有应用，已停止操作以保留所有副本。请导出诊断信息。")
        }
        // Journal BEFORE either rename. Both paths are recoverable without a completion write.
        receipt!.phase = unpublished ? "restoring_unpublished" : "restoring"
        try save()
        let parent = destination.deletingLastPathComponent()
        if hadNew { try operation("/bin/mv", [destination.path, displaced.path], writingTo: [parent, displaced.deletingLastPathComponent()]) }
        try operation("/bin/mv", [r.backup, destination.path], writingTo: [backup.deletingLastPathComponent(), parent])
        receipt!.phase = "restored"; try save()
        progress("已恢复旧版 \(backupInfo.version)。音乐库未被替换。请从应用程序中打开声场档案。")
    }
    func exportDiagnostics(to output: URL) throws {
        // Export only this installer session plus the most recent transaction.
        // Do not include settings.json, database, audio, or the entire user log directory.
        let bundle = runDirectory.appendingPathComponent("diagnostics-\(UUID().uuidString)")
        try fm.createDirectory(at: bundle, withIntermediateDirectories: false)
        for name in ["installer.log", "application.log", "startup.json", "transaction.json", "error.txt"] {
            if let data = try? Data(contentsOf: runDirectory.appendingPathComponent(name)) {
                try Data(data.suffix(2_000_000)).write(to: bundle.appendingPathComponent(name))
            }
        }
        if let data = try? Data(contentsOf: logRoot.appendingPathComponent("latest.json")) {
            try data.write(to: bundle.appendingPathComponent("latest-transaction.json"))
        }
        if let previous = receipt?.diagnosticsSession, UUID(uuidString: previous) != nil,
           previous != runDirectory.lastPathComponent {
            for name in ["installer.log", "application.log", "startup.json", "transaction.json", "error.txt"] {
                if let data = try? Data(contentsOf: logRoot.appendingPathComponent(previous).appendingPathComponent(name)) {
                    try Data(data.suffix(2_000_000)).write(to: bundle.appendingPathComponent("上次-" + name))
                }
            }
        }
        if let r = receipt {
            let attempts = attemptsDirectory(r)
            let entries = (try? fm.contentsOfDirectory(at: attempts, includingPropertiesForKeys: nil)) ?? []
            for entry in entries where UUID(uuidString: entry.lastPathComponent) != nil {
                let target = bundle.appendingPathComponent("启动记录").appendingPathComponent(entry.lastPathComponent)
                try fm.createDirectory(at: target, withIntermediateDirectories: true)
                for name in ["application.log", "startup.json", "attempt.json", "error.txt"] {
                    if let data = try? Data(contentsOf: entry.appendingPathComponent(name)) {
                        try Data(data.suffix(2_000_000)).write(to: target.appendingPathComponent(name))
                    }
                }
            }
        }
        let reportDirectory = fm.homeDirectoryForCurrentUser.appendingPathComponent("Library/Logs/DiagnosticReports")
        let reports = (try? fm.contentsOfDirectory(at: reportDirectory, includingPropertiesForKeys: [.contentModificationDateKey])) ?? []
        let matching = reports.filter { $0.lastPathComponent.hasPrefix("music-manager") && ["ips", "crash"].contains($0.pathExtension) }
            .sorted { ((try? $0.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast) > ((try? $1.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast) }
        if let newest = matching.first, let data = try? Data(contentsOf: newest), data.count < 5_000_000 {
            try data.write(to: bundle.appendingPathComponent(newest.lastPathComponent))
        }
        try "系统：\(ProcessInfo.processInfo.operatingSystemVersionString)\n诊断文件可能包含本机路径和系统崩溃报告，请仅发给你信任的开发者。\n".write(to: bundle.appendingPathComponent("说明.txt"), atomically: true, encoding: .utf8)
        _ = try io.run("/usr/bin/ditto", ["-c", "-k", "--keepParent", bundle.path, output.path], privileged: false)
    }
}
