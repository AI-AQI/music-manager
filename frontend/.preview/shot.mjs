#!/usr/bin/env node
/* 预览截图驱动：CDP 控制无头 Chrome，等真实时间流逝后截图。 */
import { spawn } from "node:child_process";
import { writeFileSync } from "node:fs";

const [hash = "", out = "/tmp/mm-shot.png", waitMs = "2500", width = "1440", height = "900", evalExpr = "", postWaitMs = "0"] =
    process.argv.slice(2);
const PORT = 9223;
const PAGE = `http://127.0.0.1:8931/preview.html${hash}`;
const CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

const chrome = spawn(CHROME, [
    "--headless=new", "--use-angle=metal", "--hide-scrollbars",
    `--remote-debugging-port=${PORT}`, `--window-size=${width},${height}`, "about:blank"
], { stdio: "ignore" });

const sleep = ms => new Promise(r => setTimeout(r, ms));

async function getTarget() {
    for (let i = 0; i < 50; i++) {
        try {
            const list = await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json();
            const page = list.find(t => t.type === "page");
            if (page) return page;
        } catch {}
        await sleep(200);
    }
    throw new Error("Chrome DevTools 端口没有响应");
}

try {
    const target = await getTarget();
    const ws = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });

    let seq = 0;
    const pending = new Map();
    ws.onmessage = ev => {
        const msg = JSON.parse(ev.data);
        if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); }
        if (msg.method === "Runtime.exceptionThrown")
            console.error("[page exception]", JSON.stringify(msg.params.exceptionDetails).slice(0, 1200));
        if (msg.method === "Runtime.consoleAPICalled" && (msg.params.type === "error" || msg.params.type === "warning"))
            console.error("[page console." + msg.params.type + "]", msg.params.args.map(a => a.value ?? a.description ?? "").join(" ").slice(0, 800));
    };
    const send = (method, params = {}) => new Promise(res => {
        const id = ++seq;
        pending.set(id, res);
        ws.send(JSON.stringify({ id, method, params }));
    });

    await send("Page.enable");
    await send("Runtime.enable");
    await send("Emulation.setDeviceMetricsOverride", {
        width: Number(width), height: Number(height), deviceScaleFactor: 2, mobile: false
    });
    await send("Page.navigate", { url: PAGE });
    await sleep(Number(waitMs));

    if (evalExpr) {
        const r = await send("Runtime.evaluate", { expression: evalExpr, awaitPromise: true, returnByValue: true });
        console.error("[eval]", JSON.stringify(r.result?.result?.value ?? r, null, 1).slice(0, 4000));
        if (Number(postWaitMs) > 0) await sleep(Number(postWaitMs));
    }

    const shot = await send("Page.captureScreenshot", { format: "png" });
    writeFileSync(out, Buffer.from(shot.result.data, "base64"));
    console.log(out);
    ws.close();
} finally {
    chrome.kill("SIGKILL");
}
