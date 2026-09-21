/* =========================================================
   波形 — Musicbed 式音频波形
   - 底部播放器：整曲波形 + 播放进度 + 点击/拖动跳转
   - 音乐行内：静态灰色小波形（懒解码、限并发、按 URL 缓存）
   独立于 app.js 运行；解码失败的曲目静默留空，不影响交互。
========================================================= */

(function () {
    "use strict";

    const audio = document.getElementById("barAudio");

    /* ---------------- 峰值解码 ---------------- */

    const peaksCache = new Map();
    const peaksPending = new Map();
    const queue = [];
    let activeJobs = 0;
    let decodeContext = null;

    function getDecodeContext() {
        if (!decodeContext) {
            const Ctx = window.OfflineAudioContext || window.webkitOfflineAudioContext;
            if (!Ctx) throw new Error("OfflineAudioContext 不可用");
            decodeContext = new Ctx(1, 1, 44100);
        }
        return decodeContext;
    }

    function computePeaks(buffer, buckets) {
        const channels = Math.min(buffer.numberOfChannels, 2);
        const data = [];
        for (let c = 0; c < channels; c++) data.push(buffer.getChannelData(c));

        const length = buffer.length;
        const per = Math.max(1, Math.floor(length / buckets));
        const peaks = new Float32Array(buckets);

        for (let b = 0; b < buckets; b++) {
            const start = b * per;
            const end = Math.min(length, start + per);
            const step = Math.max(1, Math.floor((end - start) / 64));
            let peak = 0;
            for (let i = start; i < end; i += step) {
                for (let c = 0; c < channels; c++) {
                    const v = Math.abs(data[c][i]);
                    if (v > peak) peak = v;
                }
            }
            peaks[b] = peak;
        }

        let max = 0;
        for (let i = 0; i < buckets; i++) if (peaks[i] > max) max = peaks[i];
        if (max > 0) {
            for (let i = 0; i < buckets; i++) peaks[i] = 0.1 + 0.9 * (peaks[i] / max);
        }
        return peaks;
    }

    async function decode(url, buckets) {
        const response = await fetch(url);
        if (!response.ok) throw new Error("读取音频失败: " + response.status);
        const raw = await response.arrayBuffer();
        const buffer = await getDecodeContext().decodeAudioData(raw);
        return computePeaks(buffer, buckets);
    }

    function pump() {
        while (activeJobs < 2 && queue.length) {
            const job = queue.shift();
            activeJobs++;
            decode(job.url, job.buckets)
                .then(job.resolve, job.reject)
                .finally(() => {
                    activeJobs--;
                    pump();
                });
        }
    }

    function getPeaks(url, buckets) {
        const key = url + "@" + buckets;
        if (peaksCache.has(key)) return Promise.resolve(peaksCache.get(key));
        if (peaksPending.has(key)) return peaksPending.get(key);

        const promise = new Promise((resolve, reject) => {
            queue.push({ url, buckets, resolve, reject });
            pump();
        }).then(peaks => {
            peaksCache.set(key, peaks);
            return peaks;
        });
        peaksPending.set(key, promise);
        promise.finally(() => peaksPending.delete(key));
        return promise;
    }

    /* ---------------- 绘制工具 ---------------- */

    function cssColor(name, fallback) {
        const value = getComputedStyle(document.body).getPropertyValue(name).trim();
        return value || fallback;
    }

    function fitCanvas(canvas) {
        const dpr = Math.min(window.devicePixelRatio || 1, 2);
        const w = canvas.clientWidth;
        const h = canvas.clientHeight;
        if (!w || !h) return null;
        const bw = Math.round(w * dpr);
        const bh = Math.round(h * dpr);
        if (canvas.width !== bw || canvas.height !== bh) {
            canvas.width = bw;
            canvas.height = bh;
        }
        const ctx = canvas.getContext("2d");
        ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
        ctx.clearRect(0, 0, w, h);
        return { ctx, w, h };
    }

    function drawBars(canvas, peaks, progressRatio, range) {
        const fitted = fitCanvas(canvas);
        if (!fitted || !peaks) return;
        const { ctx, w, h } = fitted;
        const n = peaks.length;
        const barW = w / n;
        const mid = h / 2;
        const playedColor = cssColor("--primary-strong", "#4f46e5");
        const restColor = cssColor("--subtext", "#8a90a5");

        for (let i = 0; i < n; i++) {
            const ratio = i / n;
            const barH = Math.max(1.5, peaks[i] * (h - 4));
            const inRange = range && ratio >= range.from && ratio <= range.to;
            const played = progressRatio > 0 && ratio <= progressRatio;
            const lit = played || inRange;
            ctx.globalAlpha = played ? 1 : inRange ? 0.9 : 0.42;
            ctx.fillStyle = lit ? playedColor : restColor;
            ctx.fillRect(
                i * barW + barW * 0.18,
                mid - barH / 2,
                Math.max(1, barW * 0.64),
                barH
            );
        }
        ctx.globalAlpha = 1;
    }

    /* ---------------- 播放器波形 ---------------- */

    const waveCanvas = document.getElementById("playerWave");

    if (waveCanvas && audio) {
        const PLAYER_BUCKETS = 320;
        let currentUrl = "";
        let currentPeaks = null;
        let dragging = false;

        function currentRatio() {
            const dur = audio.duration;
            if (!Number.isFinite(dur) || dur <= 0) return 0;
            return Math.min(1, Math.max(0, audio.currentTime / dur));
        }

        function redraw() {
            drawBars(waveCanvas, currentPeaks, currentRatio());
        }

        function seekTo(event) {
            const rect = waveCanvas.getBoundingClientRect();
            if (rect.width < 1) return;
            const ratio = Math.min(1, Math.max(0, (event.clientX - rect.left) / rect.width));
            const dur = audio.duration;
            if (Number.isFinite(dur) && dur > 0) {
                audio.currentTime = ratio * dur;
                redraw();
            }
        }

        waveCanvas.addEventListener("pointerdown", event => {
            dragging = true;
            waveCanvas.setPointerCapture(event.pointerId);
            seekTo(event);
        });
        waveCanvas.addEventListener("pointermove", event => {
            if (dragging) seekTo(event);
        });
        ["pointerup", "pointercancel"].forEach(type =>
            waveCanvas.addEventListener(type, () => { dragging = false; })
        );

        audio.addEventListener("loadedmetadata", () => {
            const url = audio.currentSrc;
            if (!url || url === currentUrl) return;
            currentUrl = url;
            currentPeaks = null;
            redraw();
            getPeaks(url, PLAYER_BUCKETS)
                .then(peaks => {
                    if (url !== currentUrl) return;
                    currentPeaks = peaks;
                    redraw();
                })
                .catch(error => console.warn("waveform: 播放器波形解码失败", error));
        });

        audio.addEventListener("timeupdate", () => {
            if (!dragging) redraw();
        });

        audio.addEventListener("emptied", () => {
            currentUrl = "";
            currentPeaks = null;
            redraw();
        });

        window.addEventListener("resize", redraw);

        // 主题切换后重取 CSS 颜色
        new MutationObserver(redraw)
            .observe(document.body, { attributes: true, attributeFilter: ["data-theme"] });
    }

    /* ---------------- 行内小波形 ---------------- */

    const ROW_BUCKETS = 140;

    function hydrateRowWave(canvas) {
        if (canvas.dataset.waveDone) return;
        canvas.dataset.waveDone = "1";
        const url = canvas.dataset.waveSrc;
        if (!url) return;
        const from = parseFloat(canvas.dataset.waveFrom);
        const to = parseFloat(canvas.dataset.waveTo);
        const range = (Number.isFinite(from) && Number.isFinite(to) && to > from)
            ? { from, to }
            : null;
        getPeaks(url, ROW_BUCKETS)
            .then(peaks => {
                if (canvas.isConnected) drawBars(canvas, peaks, 0, range);
            })
            .catch(() => {
                // 解码失败的曲目留空，行内布局不受影响
            });
    }

    function scanRowWaves(root) {
        if (root.nodeType !== 1) return;
        if (root.matches && root.matches("canvas.row-wave")) hydrateRowWave(root);
        if (root.querySelectorAll) {
            root.querySelectorAll("canvas.row-wave").forEach(hydrateRowWave);
        }
    }

    const contentArea = document.getElementById("contentArea");
    if (contentArea) {
        new MutationObserver(mutations => {
            for (const mutation of mutations) {
                mutation.addedNodes.forEach(scanRowWaves);
            }
        }).observe(contentArea, { childList: true, subtree: true });
    }

    document.querySelectorAll("canvas.row-wave").forEach(hydrateRowWave);

    // 供唱机模块复用峰值解码（唱片纹槽）
    window.Waveform = { getPeaks };
})();
