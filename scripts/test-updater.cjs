const { test } = require("node:test");
const assert = require("node:assert/strict");
const { createUpdaterController } = require("../frontend/updater.js");
const deferred = () => {
    let resolve, reject;
    const promise = new Promise((a, b) => { resolve = a; reject = b; });
    return { promise, resolve, reject };
};
function setup(options = {}) {
    const calls = [];
    let progress;
    let releases = 0;
    const controller = createUpdaterController({
        invoke: async command => {
            calls.push(command);
            if (options.invoke) return options.invoke(command);
            if (command === "check_app_update") return { currentVersion: "0.2.5", version: "0.2.6", notes: "updates" };
        },
        listen: async (_event, handler) => { progress = handler; return () => {}; },
        onChange() {},
        prepareRestart: options.prepareRestart || (async () => () => { releases++; })
    });
    return { controller, calls, progress: value => progress({ payload: value }), releases: () => releases };
}
test("checking never downloads or quits; download never installs automatically", async () => {
    const { controller: c, calls } = setup();
    await c.check();
    assert.equal(c.state.phase, "available");
    assert.deepEqual(calls, ["check_app_update"]);
    await c.download();
    assert.equal(c.state.phase, "ready");
    assert.deepEqual(calls, ["check_app_update", "download_app_update"]);
});
test("duplicate clicks cannot start concurrent checks or downloads", async () => {
    const work = deferred();
    const { controller: c, calls } = setup({ invoke: () => work.promise });
    const check = c.check();
    await c.check(); await c.download();
    assert.equal(calls.length, 1);
    work.resolve({ currentVersion: "0.2.5", version: null, notes: "" }); await check;
    assert.equal(c.state.phase, "latest");
});
test("download failure cannot enable restart and can be retried", async () => {
    let fail = true;
    const { controller: c, calls } = setup({ invoke: async cmd => {
        if (cmd === "check_app_update") return { currentVersion: "0.2.5", version: "0.2.6", notes: "" };
        if (fail) throw new Error("signature invalid");
    } });
    await c.check(); await c.download(); await c.restart();
    assert.equal(c.state.phase, "available");
    assert.match(c.state.message, /signature invalid/);
    assert.ok(!calls.includes("restart_for_app_update"));
    fail = false; await c.download();
    assert.equal(c.state.phase, "ready");
});
test("progress is displayed without marking an unverified download ready", async () => {
    const work = deferred();
    const { controller: c, progress } = setup({ invoke: cmd => cmd === "check_app_update"
        ? { currentVersion: "0.2.5", version: "0.2.6", notes: "" } : work.promise });
    await c.check();
    const downloading = c.download();
    progress({ phase: "downloading", downloaded: 50, total: 100 });
    assert.equal(c.state.downloaded, 50);
    progress({ phase: "verifying", downloaded: 0, total: null });
    progress({ phase: "ready", downloaded: 100, total: 100 });
    assert.equal(c.state.phase, "verifying");
    assert.equal(c.state.busy, true);
    work.resolve(); await downloading;
    assert.equal(c.state.phase, "ready");
});
test("cancelling restart retains download and never invokes the installer", async () => {
    const { controller: c, calls } = setup({ prepareRestart: async () => null });
    await c.check(); await c.download(); await c.restart();
    assert.equal(c.state.phase, "ready");
    assert.ok(!calls.includes("restart_for_app_update"));
});
test("handoff failure unlocks the library and allows retry", async () => {
    const app = setup({ invoke: async cmd => {
        if (cmd === "check_app_update") return { currentVersion: "0.2.5", version: "0.2.6", notes: "" };
        if (cmd === "restart_for_app_update") throw new Error("helper failed");
    } });
    await app.controller.check(); await app.controller.download(); await app.controller.restart();
    assert.equal(app.controller.state.phase, "ready");
    assert.match(app.controller.state.message, /helper failed/);
    assert.equal(app.releases(), 1);
});
test("restart waits for in-flight library work before calling backend", async () => {
    const drained = deferred();
    const { controller: c, calls } = setup({ prepareRestart: () => drained.promise });
    await c.check(); await c.download();
    const restarting = c.restart();
    assert.ok(!calls.includes("restart_for_app_update"));
    drained.resolve(() => {}); await restarting;
    assert.ok(calls.includes("restart_for_app_update"));
});
test("offline check shows an error rather than claiming the app is current", async () => {
    const { controller: c } = setup({ invoke: () => { throw new Error("offline"); } });
    await c.check();
    assert.equal(c.state.phase, "idle");
    assert.match(c.state.message, /offline/);
    assert.equal(c.state.busy, false);
});
