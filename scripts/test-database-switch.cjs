const { test } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

const source = fs.readFileSync(path.join(__dirname, "../frontend/app.js"), "utf8");
const bridge = source.slice(0, source.indexOf("const TAG_GROUP_COLLAPSED_STORAGE_KEY"));
const start = source.indexOf("async function changeDatabaseLocation()");
const switcher = source.slice(start, source.indexOf("\n\n/* ===", start));
const startup = source.slice(source.indexOf("async function initDB()"), source.indexOf("async function refreshLocationsAfterFinderChanges()"));
const initialize = source.slice(source.indexOf("async function init()"), source.lastIndexOf("\n\ninit();"));

function setup({ exists = true, selected = "/external/music-library", confirmed = true, onInvoke, retryAnswers = [], startupChoices = [], creationAnswers = [] } = {}) {
    const calls = [];
    const messages = [];
    const confirmations = [];
    const startupPrompts = [];
    const libraryChoices = [];
    const creationPrompts = [];
    const button = { disabled: false };
    const body = { inert: false };
    let reloads = 0;
    const context = {
        console: { error() {} },
        document: { body },
        window: { location: { reload() { reloads++; } } },
        $: id => id === "changeDbLocationBtn" ? button : { classList: { remove() {} } },
        confirm: async (message, title) => { confirmations.push({ message, title }); return confirmed; },
        showToast() {},
        showMessage: async message => messages.push(message),
        getErrorMessage: error => error.message || String(error),
        ensureTauri() {},
        alert: message => { throw new Error(`Unexpected initialization alert: ${message}`); },
        __TAURI__: {
            core: {
                convertFileSrc() {},
                invoke: async (command, args) => {
                    calls.push({ command, args });
                    const result = onInvoke ? await onInvoke(command, args) : undefined;
                    if (command === "init_db") return result ?? true;
                    if (command === "file_exists") return exists;
                    if (command === "set_database_location") return `${selected}/library.db`;
                }
            },
            dialog: {
                open: async () => selected,
                ask: async (message, options) => {
                    if (options.title === "创建新音乐库") {
                        creationPrompts.push({ message, options });
                        return creationAnswers.shift() ?? false;
                    }
                    startupPrompts.push({ message, options });
                    return retryAnswers.shift() ?? false;
                },
                message: async (message, options) => {
                    libraryChoices.push({ message, options });
                    return startupChoices.shift() ?? "退出";
                }
            }
        }
    };
    context.window.__TAURI__ = context.__TAURI__;
    vm.createContext(context);
    vm.runInContext(`${bridge}\n${switcher}\n${startup}\n${initialize}`, context);
    return { context, calls, messages, confirmations, startupPrompts, libraryChoices, creationPrompts, button, body, reloads: () => reloads };
}

test("restart preparation drains writes and blocks further commands until released", async () => {
    let complete;
    const pending = new Promise(resolve => { complete = resolve; });
    const app = setup({ retryAnswers: [true], onInvoke: command => command === "create_music" ? pending : undefined });
    const write = app.context.invoke("create_music");
    const preparing = app.context.prepareApplicationUpdate();
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(app.body.inert, true);
    await assert.rejects(app.context.invoke("delete_music"), /准备更新/);
    complete();
    await write;
    const release = await preparing;
    release();
    assert.equal(app.body.inert, false);
    await app.context.invoke("list_music");
});

test("failed in-flight writes abort restart and restore interaction", async () => {
    let fail;
    const pending = new Promise((_resolve, reject) => { fail = reject; });
    const app = setup({ retryAnswers: [true], onInvoke: () => pending });
    const write = app.context.invoke("create_music").catch(() => {});
    const preparing = app.context.prepareApplicationUpdate();
    await new Promise(resolve => setImmediate(resolve));
    fail(new Error("disk offline"));
    await assert.rejects(preparing, /操作未能完成/);
    await write;
    assert.equal(app.body.inert, false);
});

test("existing external library is opened without an overwrite parameter, then UI reloads", async () => {
    const app = setup();
    await app.context.changeDatabaseLocation();
    const call = app.calls.find(c => c.command === "set_database_location");
    assert.deepEqual(JSON.parse(JSON.stringify(call.args)), { dir: "/external/music-library", createIfMissing: false });
    assert.equal(app.confirmations[0].title, "打开已有音乐库");
    assert.equal(app.reloads(), 1);
    assert.equal(app.body.inert, true, "旧界面在刷新前不能继续操作");
});

test("empty directory requires confirmation to create an empty library", async () => {
    const app = setup({ exists: false });
    await app.context.changeDatabaseLocation();
    assert.equal(app.confirmations[0].title, "创建空音乐库");
    assert.match(app.confirmations[0].message, /当前音乐库会保留/);
    assert.equal(app.calls.find(c => c.command === "set_database_location").args.createIfMissing, true);
});

test("cancelling the confirmation does not switch or reload", async () => {
    const app = setup({ confirmed: false });
    await app.context.changeDatabaseLocation();
    assert.equal(app.calls.some(c => c.command === "set_database_location"), false);
    assert.equal(app.reloads(), 0);
    assert.equal(app.button.disabled, false);
    assert.equal(app.body.inert, false);
});

test("cancelling the directory picker leaves the current library active", async () => {
    const app = setup({ selected: null });
    await app.context.changeDatabaseLocation();
    assert.equal(app.calls.length, 0);
    assert.equal(app.reloads(), 0);
    assert.equal(app.button.disabled, false);
});

test("failed switch restores interaction and does not reload old data as a new library", async () => {
    const app = setup({ onInvoke: async command => {
        if (command === "set_database_location") throw new Error("数据库损坏");
    } });
    await app.context.changeDatabaseLocation();
    assert.equal(app.reloads(), 0);
    assert.equal(app.button.disabled, false);
    assert.equal(app.body.inert, false);
    assert.match(app.messages[0], /仍在使用原音乐库/);
    await app.context.invoke("list_music");
});

test("switch waits for an old-library write and blocks its follow-up writes", async () => {
    let finishWrite;
    const oldWrite = new Promise(resolve => { finishWrite = resolve; });
    const app = setup({ onInvoke: async command => {
        if (command === "update_music") await oldWrite;
    } });
    const pendingWrite = app.context.invoke("update_music", { id: "old-music" });
    const switching = app.context.changeDatabaseLocation();
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(app.calls.some(c => c.command === "set_database_location"), false);
    await assert.rejects(app.context.invoke("create_clip", { id: "old-clip" }), /正在切换音乐库/);
    finishWrite();
    await pendingWrite;
    await switching;
    assert.equal(app.calls.some(c => c.command === "create_clip"), false);
    assert.equal(app.reloads(), 1);
});

test("successful startup enables the interface only after opening the database", async () => {
    let finishOpening;
    const opening = new Promise(resolve => { finishOpening = resolve; });
    const app = setup({ onInvoke: async command => {
        if (command === "init_db") await opening;
    } });
    const startup = app.context.initDB();
    assert.equal(app.body.inert, true);
    finishOpening();
    assert.equal(await startup, true);
    assert.equal(app.body.inert, false);
    assert.equal(app.startupPrompts.length, 0);
});

test("missing drive prompts to connect and retries without selecting or creating another library", async () => {
    let attempts = 0;
    const app = setup({ retryAnswers: [true], onInvoke: async command => {
        if (command === "init_db" && attempts++ === 0) throw new Error("无法打开 /Volumes/Music/library.db，请连接硬盘");
    } });
    assert.equal(await app.context.initDB(), true);
    assert.deepEqual(app.calls.map(c => c.command), ["init_db", "report_startup_state", "init_db"]);
    assert.match(app.startupPrompts[0].message, /请先连接硬盘/);
    assert.equal(app.startupPrompts[0].options.okLabel, "重试");
    assert.equal(app.startupPrompts[0].options.cancelLabel, "退出");
    assert.equal(app.body.inert, false);
});

test("cancelling startup exits without loading candidates, rendering or binding library actions", async () => {
    const app = setup({ retryAnswers: [false], onInvoke: async command => {
        if (command === "init_db") throw new Error("请连接硬盘");
    } });
    await app.context.init();
    assert.deepEqual(app.calls.map(c => c.command), ["init_db", "report_startup_state", "cancel_database_startup"]);
    assert.equal(app.body.inert, true);
    assert.equal(app.reloads(), 0);
});

test("failed retries remain blocked and never fall back to an empty library", async () => {
    const app = setup({ retryAnswers: [true, false], onInvoke: async command => {
        if (command === "init_db") throw new Error("音乐库不可用");
    } });
    assert.equal(await app.context.initDB(), false);
    assert.deepEqual(app.calls.map(c => c.command), ["init_db", "report_startup_state", "init_db", "report_startup_state", "cancel_database_startup"]);
    assert.equal(app.startupPrompts.length, 2);
    assert.equal(app.body.inert, true);
});

test("missing configuration requires explicit choice to open an existing library", async () => {
    const app = setup({ startupChoices: ["打开已有音乐库"], onInvoke: async command => {
        if (command === "init_db") return false;
    } });
    assert.equal(await app.context.initDB(), true);
    assert.deepEqual(app.calls.map(c => c.command), ["init_db", "report_startup_state", "configure_database_startup"]);
    assert.equal(app.calls.find(c => c.command === "configure_database_startup").args.createNew, false);
    assert.equal(app.calls.find(c => c.command === "configure_database_startup").args.dir, "/external/music-library");
    assert.equal(app.creationPrompts.length, 0);
    assert.equal(app.body.inert, false);
});

test("creating a startup library requires choosing create and confirming it", async () => {
    const app = setup({ startupChoices: ["创建新音乐库"], creationAnswers: [true], onInvoke: async command => {
        if (command === "init_db") return false;
    } });
    assert.equal(await app.context.initDB(), true);
    assert.equal(app.creationPrompts.length, 1);
    assert.equal(app.calls.find(c => c.command === "configure_database_startup").args.createNew, true);
});

test("cancelling creation never configures or creates a library", async () => {
    const app = setup({ startupChoices: ["创建新音乐库", "退出"], creationAnswers: [false], onInvoke: async command => {
        if (command === "init_db") return false;
    } });
    assert.equal(await app.context.initDB(), false);
    assert.equal(app.calls.some(c => c.command === "configure_database_startup"), false);
    assert.equal(app.body.inert, true);
});

test("cancelling directory selection does not substitute a default library", async () => {
    const app = setup({ selected: null, startupChoices: ["打开已有音乐库", "退出"], onInvoke: async command => {
        if (command === "init_db") return false;
    } });
    assert.equal(await app.context.initDB(), false);
    assert.equal(app.calls.some(c => c.command === "configure_database_startup"), false);
});

test("damaged configuration shows an error, never the create-new-library choice", async () => {
    const app = setup({ startupChoices: ["创建新音乐库"], onInvoke: async command => {
        if (command === "init_db") throw new Error("位置设置损坏");
    } });
    assert.equal(await app.context.initDB(), false);
    assert.equal(app.libraryChoices.length, 0);
    assert.equal(app.calls.some(c => c.command === "configure_database_startup"), false);
});


test("installer is told about waiting states, and diagnostic failure does not block startup", async () => {
    const app = setup({ startupChoices: ["打开已有音乐库"], onInvoke: async command => {
        if (command === "init_db") return false;
        if (command === "report_startup_state") throw new Error("diagnostic file unavailable");
    } });
    assert.equal(await app.context.initDB(), true);
    assert.equal(app.calls.find(c => c.command === "report_startup_state").args.state, "waiting_library");
    assert.equal(app.body.inert, false);
});

test("library-unavailable is a recoverable startup status, not a crash", async () => {
    const app = setup({ onInvoke: async command => {
        if (command === "init_db") throw new Error("offline");
    } });
    await app.context.initDB();
    assert.equal(app.calls.find(c => c.command === "report_startup_state").args.state, "library_unavailable");
});
