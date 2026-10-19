const { test } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

const source = fs.readFileSync(path.join(__dirname, "../frontend/app.js"), "utf8");
const bridge = source.slice(0, source.indexOf("const TAG_GROUP_COLLAPSED_STORAGE_KEY"));
const start = source.indexOf("async function changeDatabaseLocation()");
const switcher = source.slice(start, source.indexOf("\n\n/* ===", start));

function setup({ exists = true, selected = "/external/music-library", confirmed = true, onInvoke } = {}) {
    const calls = [];
    const messages = [];
    const confirmations = [];
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
        __TAURI__: {
            core: {
                convertFileSrc() {},
                invoke: async (command, args) => {
                    calls.push({ command, args });
                    if (onInvoke) await onInvoke(command, args);
                    if (command === "file_exists") return exists;
                    if (command === "set_database_location") return `${selected}/library.db`;
                }
            },
            dialog: { open: async () => selected }
        }
    };
    context.window.__TAURI__ = context.__TAURI__;
    vm.createContext(context);
    vm.runInContext(`${bridge}\n${switcher}`, context);
    return { context, calls, messages, confirmations, button, body, reloads: () => reloads };
}

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
