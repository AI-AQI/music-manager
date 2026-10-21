import AppKit

final class InstallerDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate {
    var window: NSWindow!
    var status: NSTextField!
    var spinner: NSProgressIndicator!
    var installButton: NSButton!
    var retryButton: NSButton!
    var restoreButton: NSButton!
    var exportButton: NSButton!
    var installer: Installer?
    var busy = false
    var updateRequest: InAppUpdateRequest?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        let menu = NSMenu()
        let item = NSMenuItem()
        menu.addItem(item)
        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "退出安装与更新", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        item.submenu = appMenu
        NSApp.mainMenu = menu
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 620, height: 340),
                          styleMask: [.titled, .closable, .miniaturizable], backing: .buffered, defer: false)
        window.title = "声场档案 · 安装与更新"
        window.delegate = self
        window.isReleasedWhenClosed = false
        let content = window.contentView!
        let heading = NSTextField(labelWithString: "安装与更新声场档案")
        heading.font = .systemFont(ofSize: 23, weight: .semibold)
        heading.frame = NSRect(x: 28, y: 274, width: 560, height: 36)
        content.addSubview(heading)
        let subtitle = NSTextField(wrappingLabelWithString: "更新会退出正在运行的声场档案，并保留旧应用备份。音乐库和原始音频独立保留。")
        subtitle.frame = NSRect(x: 28, y: 218, width: 560, height: 48)
        content.addSubview(subtitle)
        status = NSTextField(wrappingLabelWithString: "正在读取安装信息…")
        status.isSelectable = true
        status.frame = NSRect(x: 28, y: 90, width: 560, height: 114)
        content.addSubview(status)
        spinner = NSProgressIndicator(frame: NSRect(x: 28, y: 70, width: 560, height: 12))
        spinner.style = .bar; spinner.isIndeterminate = true; spinner.isDisplayedWhenStopped = false
        content.addSubview(spinner)
        installButton = button("安装 / 更新", action: #selector(install), x: 28, width: 125)
        retryButton = button("重新尝试启动", action: #selector(retry), x: 160, width: 130)
        restoreButton = button("恢复旧版", action: #selector(restore), x: 296, width: 110)
        exportButton = button("导出诊断信息", action: #selector(exportLogs), x: 414, width: 175)
        window.center(); window.makeKeyAndOrderFront(nil); NSApp.activate(ignoringOtherApps: true)
        do {
            let fm = FileManager.default
            let shared = URL(fileURLWithPath: "/Applications/声场档案.app")
            let personal = fm.homeDirectoryForCurrentUser.appendingPathComponent("Applications/声场档案.app")
            var logRoot = fm.homeDirectoryForCurrentUser.appendingPathComponent("Library/Logs/com.musicmanager.installer")
            var identifier = "com.musicmanager.app"
            // Keep an existing installation at its original location. Fresh installs don't need admin access.
            var destination = fm.fileExists(atPath: shared.path) ? shared : personal
            #if INSTALLER_UI_TESTS
            // Compiled only into the native UI test fixture, never the distributed installer.
            let testRoot = URL(fileURLWithPath: "/private/tmp/music-manager-installer-ui")
            destination = testRoot.appendingPathComponent("Applications/声场档案.app")
            logRoot = testRoot.appendingPathComponent("logs")
            identifier = "com.musicmanager.installertest"
            #endif
            #if INSTALLER_UI_TESTS
            let allowedDestinations = [destination]
            #else
            let allowedDestinations = [shared, personal]
            #endif
            updateRequest = try InAppUpdateRequest.parse(Array(CommandLine.arguments.dropFirst()), allowedDestinations: allowedDestinations)
            if let data = try? Data(contentsOf: logRoot.appendingPathComponent("latest.json")),
               let previous = try? JSONDecoder().decode(InstallReceipt.self, from: data) {
                #if !INSTALLER_UI_TESTS
                guard [shared.path, personal.path].contains(previous.destination) else {
                    throw InstallError("上次更新记录的安装位置异常，已停止安装。请联系开发者。")
                }
                if !previous.finished { destination = URL(fileURLWithPath: previous.destination) }
                #endif
            }
            if let request = updateRequest { destination = request.destination }
            if destination == personal {
                try fm.createDirectory(at: personal.deletingLastPathComponent(), withIntermediateDirectories: true)
            }
            let source = Bundle.main.resourceURL!.appendingPathComponent("声场档案.app")
            let info = try AppInfo.read(source, identifier: identifier)
            installer = try Installer(source: source, destination: destination, logRoot: logRoot, identifier: identifier, progress: { [weak self] message in
                DispatchQueue.main.async { self?.status.stringValue = message }
            })
            status.stringValue = "安装包版本：\(info.version)\n安装位置：\(destination.path)\n\n点击“安装 / 更新”开始。失败后可以在这里恢复旧版或导出诊断信息。"
            if let error = installer?.recoveryError { status.stringValue = error }
            if let receipt = installer?.receipt, !["completed", "restored", "failed_before_replace"].contains(receipt.phase) {
                status.stringValue = "检测到上次未完成的更新。\n可以重新尝试启动，或点击“恢复旧版”。旧应用备份和音乐库已保留。"
            }
            if let request = updateRequest, let installer = installer {
                try installer.validateInAppUpdate(version: request.version)
                try request.respond()
                status.stringValue = "更新程序已就绪，正在等待声场档案退出…"
                perform(completion: { NSApp.terminate(nil) }) {
                    try request.waitForParentExit()
                    try installer.install()
                    _ = try installer.launch()
                }
            }
        } catch {
            try? updateRequest?.respond(error: error.localizedDescription)
            show(error)
        }
        refreshButtons()
    }
    func button(_ title: String, action: Selector, x: CGFloat, width: CGFloat) -> NSButton {
        let b = NSButton(title: title, target: self, action: action)
        b.bezelStyle = .rounded; b.frame = NSRect(x: x, y: 25, width: width, height: 32)
        window.contentView!.addSubview(b); return b
    }
    func refreshButtons() {
        installButton.isEnabled = !busy && installer != nil && installer?.recoveryError == nil
        exportButton.isEnabled = !busy && installer != nil
        retryButton.isEnabled = !busy && installer?.canRetry == true
        restoreButton.isEnabled = !busy && installer?.canRestore == true
    }
    func perform(completion: (() -> Void)? = nil, _ work: @escaping () throws -> Void) {
        busy = true; refreshButtons(); spinner.startAnimation(nil)
        DispatchQueue.global(qos: .userInitiated).async {
            var failure: Error?
            do { try work() } catch { failure = error }
            DispatchQueue.main.async {
                self.busy = false; self.spinner.stopAnimation(nil); self.refreshButtons()
                if let failure = failure { self.show(failure) }
                else { completion?() }
            }
        }
    }
    func show(_ error: Error) {
        status.stringValue = error.localizedDescription
        if let dir = installer?.runDirectory {
            try? error.localizedDescription.write(to: dir.appendingPathComponent("error.txt"), atomically: true, encoding: .utf8)
        }
    }
    func windowShouldClose(_ sender: NSWindow) -> Bool {
        if busy { NSSound.beep() }
        return !busy
    }
    @objc func install() {
        guard let installer = installer else { return }
        perform { try installer.install(); _ = try installer.launch() }
    }
    @objc func retry() {
        guard let installer = installer else { return }
        perform { _ = try installer.launch() }
    }
    @objc func restore() {
        guard let installer = installer else { return }
        let alert = NSAlert()
        alert.messageText = "恢复旧版本？"
        alert.informativeText = "会先退出声场档案，并检查旧版能否读取当前音乐库。只恢复应用，不回滚或覆盖音乐库。"
        alert.addButton(withTitle: "恢复旧版"); alert.addButton(withTitle: "取消")
        if alert.runModal() == .alertFirstButtonReturn { perform { try installer.restore() } }
    }
    @objc func exportLogs() {
        guard let installer = installer else { return }
        let panel = NSSavePanel()
        panel.title = "导出诊断信息（不含音乐库或音频）"
        panel.message = "包含安装日志、启动日志和最近的应用崩溃报告，可能包含本机文件路径。"
        panel.nameFieldStringValue = "声场档案诊断-\(Int(Date().timeIntervalSince1970)).zip"
        panel.allowedFileTypes = ["zip"]
        if panel.runModal() == .OK, let url = panel.url {
            perform {
                try installer.exportDiagnostics(to: url)
                DispatchQueue.main.async { self.status.stringValue = "诊断信息已导出：\(url.path)\n请把这个文件发给开发者排查。" }
            }
        }
    }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { !busy }
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        if busy {
            let alert = NSAlert(); alert.messageText = "安装操作正在进行"
            alert.informativeText = "请等待当前操作结束，再关闭安装窗口。"; alert.runModal()
            return .terminateCancel
        }
        return .terminateNow
    }
}

let delegate = InstallerDelegate()
NSApplication.shared.delegate = delegate
NSApplication.shared.run()
