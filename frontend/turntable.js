/* =========================================================
   声场唱机 — Three.js 首页唱机 + 播放条 mini 唱片
   独立于 app.js 运行；通过 window.Turntable 对外提供：
     mount(container)  挂到首页容器（幂等）
     sleep()           离开首页时休眠渲染
     react(view)       菜单悬停联动："all" | "clips" | "albums" | "recent"
     setProgram({counts})           计数窗真实数据
     setTrack({coverArt, grooveUrl}) 当前曲目封面/波形
     setTheme("archive"|"sky")      整机换肤
     diveIn(cb)        镜头推进唱片后回调（进工作区过渡）
   无 THREE（vendor 缺失）时大唱机自动禁用，mini 唱片不受影响。
========================================================= */

(function () {
    "use strict";

    const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
    const audio = document.getElementById("barAudio");

    let playing = false;

    function isPlaying() {
        return playing || !!(audio && !audio.paused && !audio.ended && audio.currentSrc);
    }

    /* =========================================================
       Mini 唱片（播放条，2D Canvas）
    ========================================================== */

    const miniCanvas = document.querySelector(".player-vinyl");
    const mini = { angle: 0, speed: 0, raf: 0, cover: null, disc: null };

    function miniDrawDisc() {
        if (!miniCanvas) return;
        const S = 168, C = S / 2;
        const off = document.createElement("canvas");
        off.width = off.height = S;
        const x = off.getContext("2d");

        x.fillStyle = "#0b0a0d";
        x.beginPath(); x.arc(C, C, C - 2, 0, Math.PI * 2); x.fill();

        for (let r = C * 0.42; r < C - 6; r += 4) {
            x.strokeStyle = "rgba(255,255,255,.07)";
            x.lineWidth = 1;
            x.beginPath(); x.arc(C, C, r, 0, Math.PI * 2); x.stroke();
        }

        x.save();
        x.beginPath(); x.arc(C, C, C * 0.38, 0, Math.PI * 2); x.clip();
        if (mini.cover) {
            x.drawImage(mini.cover, C - C * 0.38, C - C * 0.38, C * 0.76, C * 0.76);
        } else {
            const g = x.createRadialGradient(C - 6, C - 6, 2, C, C, C * 0.38);
            g.addColorStop(0, "#e08a3c");
            g.addColorStop(1, "#a85f1c");
            x.fillStyle = g;
            x.fillRect(0, 0, S, S);
        }
        x.restore();

        x.strokeStyle = "rgba(255,244,224,.55)";
        x.lineWidth = 1.5;
        x.beginPath(); x.arc(C, C, C * 0.38 - 1, 0, Math.PI * 2); x.stroke();

        x.fillStyle = "#0a0a0c";
        x.beginPath(); x.arc(C, C, 3, 0, Math.PI * 2); x.fill();

        mini.disc = off;
    }

    function miniFrame() {
        mini.raf = 0;
        mini.speed += ((isPlaying() ? 2.3 : 0) - mini.speed) * 0.03;
        mini.angle += mini.speed / 60;

        const ctx = miniCanvas.getContext("2d");
        const S = miniCanvas.width, C = S / 2;
        ctx.clearRect(0, 0, S, S);
        if (mini.disc) {
            ctx.save();
            ctx.translate(C, C);
            ctx.rotate(mini.angle);
            ctx.drawImage(mini.disc, -C, -C);
            ctx.restore();
            ctx.save();
            ctx.beginPath(); ctx.arc(C, C, C - 2, 0, Math.PI * 2); ctx.clip();
            ctx.translate(C, C);
            ctx.rotate(mini.angle * 0.92);
            const g = ctx.createLinearGradient(0, -C, 0, C);
            g.addColorStop(0, "rgba(255,255,255,0)");
            g.addColorStop(.5, "rgba(255,255,255,.12)");
            g.addColorStop(1, "rgba(255,255,255,0)");
            ctx.fillStyle = g;
            ctx.beginPath();
            ctx.moveTo(0, 0);
            ctx.arc(0, 0, C, -0.4, 0.4);
            ctx.closePath();
            ctx.fill();
            ctx.restore();
        }
        if (mini.speed > 0.004 || isPlaying()) {
            mini.raf = requestAnimationFrame(miniFrame);
        }
    }

    function miniKick() {
        if (!miniCanvas || reducedMotion.matches) { miniStatic(); return; }
        if (!mini.raf) mini.raf = requestAnimationFrame(miniFrame);
    }

    function miniStatic() {
        if (!miniCanvas) return;
        const ctx = miniCanvas.getContext("2d");
        ctx.clearRect(0, 0, miniCanvas.width, miniCanvas.height);
        if (mini.disc) ctx.drawImage(mini.disc, 0, 0);
    }

    function miniSetCover(dataUrl) {
        if (!dataUrl) { mini.cover = null; miniDrawDisc(); miniKick(); return; }
        const img = new Image();
        img.onload = () => { mini.cover = img; miniDrawDisc(); miniKick(); };
        img.src = dataUrl;
    }

    /* =========================================================
       大唱机（Three.js，首页）
       造型参照复古台式唱机：木框 + 黑色台面、烟熏防尘盖、
       金属边转盘、S 形唱臂、前立面拉丝面板 + 实体旋钮
    ========================================================== */

    const FAKE_PEAKS = new Float32Array(720);
    for (let i = 0; i < 720; i++) {
        const t = i / 720;
        FAKE_PEAKS[i] = Math.min(1, Math.abs(
            Math.sin(t * 43) * .5 + Math.sin(t * 17 + 2) * .3 + Math.sin(t * 91 + 5) * .2
        ));
    }

    let S = null;
    let hasIntro = false;
    let hasDived = false;
    let pendingStats = null;
    let emptyDropHandler = null;   // 空落针时由应用侧决定放哪张；返回 true 表示已接管
    let lastTrackInfo = { title: "", artist: "" };

    // 标签配色：档案室=暖琥珀（参考图的橙标），云雾=玫瑰粉
    const LABEL_COLORS = { archive: ["#e08a3c", "#a85f1c"], sky: ["#e98aa6", "#c95d7d"] };

    /* ---------------- 程序化贴图 ---------------- */

    function makeWoodTexture(mode) {
        const c = document.createElement("canvas");
        c.width = c.height = 512;
        const x = c.getContext("2d");
        const g = x.createLinearGradient(0, 0, 512, 512);
        if (mode === "light") {
            // 云雾主题：白蜡木（去黄低饱和，和粉透彩胶同家族）
            g.addColorStop(0, "#efe4d6");
            g.addColorStop(.5, "#e6d8c5");
            g.addColorStop(1, "#d9c8b0");
        } else if (mode) {
            g.addColorStop(0, "#4a2f1a");
            g.addColorStop(.5, "#38220f");
            g.addColorStop(1, "#2a1809");
        } else {
            g.addColorStop(0, "#6b4a2f");
            g.addColorStop(1, "#4a2f1a");
        }
        x.fillStyle = g;
        x.fillRect(0, 0, 512, 512);
        const grainA = mode === "light" ? "168,148,120" : "16,9,4";
        const grainB = mode === "light" ? "255,250,240" : "132,92,52";
        for (let i = 0; i < 110; i++) {
            const y0 = Math.random() * 512;
            x.strokeStyle = `rgba(${Math.random() > .5 ? grainA : grainB},${.05 + Math.random() * .08})`;
            x.lineWidth = .6 + Math.random() * 2.4;
            x.beginPath();
            x.moveTo(0, y0);
            for (let px = 0; px <= 512; px += 32) {
                x.lineTo(px, y0 + Math.sin(px * .02 + i) * 6);
            }
            x.stroke();
        }
        const tex = new THREE.CanvasTexture(c);
        tex.encoding = THREE.sRGBEncoding;
        return tex;
    }

    // 环境反射图：暖房间，左侧一扇亮窗（金属与防尘盖反光的来源）
    function makeEnvTexture(renderer, scene) {
        const c = document.createElement("canvas");
        c.width = 1024; c.height = 512;
        const x = c.getContext("2d");
        const g = x.createLinearGradient(0, 0, 0, 512);
        g.addColorStop(0, "#3a2c1c");
        g.addColorStop(.55, "#241a10");
        g.addColorStop(1, "#120c07");
        x.fillStyle = g;
        x.fillRect(0, 0, 1024, 512);
        const win = x.createRadialGradient(200, 210, 10, 200, 210, 220);
        win.addColorStop(0, "rgba(255,222,170,.95)");
        win.addColorStop(.4, "rgba(255,200,140,.45)");
        win.addColorStop(1, "rgba(255,200,140,0)");
        x.fillStyle = win;
        x.fillRect(0, 0, 1024, 512);
        const warm = x.createRadialGradient(820, 260, 10, 820, 260, 180);
        warm.addColorStop(0, "rgba(255,190,120,.5)");
        warm.addColorStop(1, "rgba(255,190,120,0)");
        x.fillStyle = warm;
        x.fillRect(0, 0, 1024, 512);

        const tex = new THREE.CanvasTexture(c);
        tex.mapping = THREE.EquirectangularReflectionMapping;
        const pmrem = new THREE.PMREMGenerator(renderer);
        const env = pmrem.fromEquirectangular(tex).texture;
        pmrem.dispose();
        tex.dispose();
        scene.environment = env;
    }

    function drawRecord(state) {
        const c = state.recordCanvas;
        const x = c.getContext("2d");
        const S = c.width, C = S / 2;
        const R_OUT = C - 4, R_IN = 182, R_LABEL = 170;
        const peaks = state.peaks;
        const isSky = state.theme === "sky";

        const base = x.createRadialGradient(C, C, R_LABEL, C, C, R_OUT);
        if (isSky) {
            /* 粉透彩胶：暖玫瑰粉径向渐变，和白蜡木同属暖粉家族 */
            base.addColorStop(0, "#f3a3b3");
            base.addColorStop(.78, "#e87f9a");
            base.addColorStop(1, "#d96785");
        } else {
            base.addColorStop(0, "#141317");
            base.addColorStop(.78, "#0b0a0d");
            base.addColorStop(1, "#070608");
        }
        x.fillStyle = base;
        x.fillRect(0, 0, S, S);

        for (let r = R_IN; r < R_OUT - 6; r += 3) {
            for (let s = 0; s < 360; s += 3) {
                const a0 = s / 360 * Math.PI * 2;
                const peak = peaks[Math.floor(s / 360 * peaks.length)];
                x.strokeStyle = `rgba(255,255,255,${.03 + peak * .08})`;
                x.lineWidth = 1.1;
                x.beginPath();
                x.arc(C, C, r, a0, a0 + 0.052);
                x.stroke();
            }
        }

        x.strokeStyle = "rgba(255,255,255,.15)";
        x.lineWidth = 2;
        x.beginPath(); x.arc(C, C, R_OUT - 4, 0, Math.PI * 2); x.stroke();

        const [la, lb] = LABEL_COLORS[state.theme] || LABEL_COLORS.archive;
        x.save();
        x.beginPath(); x.arc(C, C, R_LABEL, 0, Math.PI * 2); x.clip();
        if (state.coverImg) {
            const img = state.coverImg;
            const side = Math.min(img.width, img.height);
            x.drawImage(img, (img.width - side) / 2, (img.height - side) / 2, side, side,
                C - R_LABEL, C - R_LABEL, R_LABEL * 2, R_LABEL * 2);
        } else {
            const lg = x.createRadialGradient(C - 34, C - 34, 10, C, C, R_LABEL);
            lg.addColorStop(0, la); lg.addColorStop(1, lb);
            x.fillStyle = lg;
            x.fillRect(C - R_LABEL, C - R_LABEL, R_LABEL * 2, R_LABEL * 2);
            x.fillStyle = isSky ? "rgba(255,255,255,.94)" : "rgba(40,24,10,.85)";
            x.font = "600 44px 'Songti SC', serif";
            x.textAlign = "center"; x.textBaseline = "middle";
            x.fillText("声场档案", C, C - 30);
            x.font = "19px ui-monospace, monospace";
            x.fillStyle = isSky ? "rgba(255,255,255,.68)" : "rgba(40,24,10,.6)";
            x.fillText("CUT & CUE · 33⅓ RPM", C, C + 24);
        }
        x.restore();

        x.strokeStyle = isSky ? "rgba(255,255,255,.6)" : "rgba(255,244,224,.5)";
        x.lineWidth = 3;
        x.beginPath(); x.arc(C, C, R_LABEL - 9, 0, Math.PI * 2); x.stroke();

        x.fillStyle = "#0a0a0c";
        x.beginPath(); x.arc(C, C, 12, 0, Math.PI * 2); x.fill();

        state.recordTex.needsUpdate = true;
    }

    function drawCounter(state) {
        const c = state.counterCanvas;
        const x = c.getContext("2d");
        x.fillStyle = "#171208";
        x.fillRect(0, 0, c.width, c.height);
        const color = state.theme === "sky" ? "#f2a9bd" : "#e8b34c";
        x.fillStyle = color;
        x.font = "52px ui-monospace, monospace";
        x.textAlign = "center"; x.textBaseline = "middle";
        x.shadowColor = color; x.shadowBlur = 14;
        const stat = state.stats[state.statIdx];
        x.fillText(stat ? `${stat.label} ${stat.value}` : "TRACKS 0", 256, 66);
        state.counterTex.needsUpdate = true;
    }

    /* 防尘盖曲目窗：LCD 风格——半透明深色底条 + 亮色荧光字，
       烟熏/全透盖、深浅封面上都看得清。无曲目时整窗隐藏。 */
    function drawLidHud(state) {
        const c = state.lidHudCanvas;
        if (!c) return;
        const x = c.getContext("2d");
        x.clearRect(0, 0, c.width, c.height);
        const title = state.trackTitle || "";
        if (!title) {
            state.lidHud.visible = false;
            state.lidHudTex.needsUpdate = true;
            return;
        }

        // 底条：圆角深色玻璃（手写圆角，别依赖新 canvas API）
        const pad = 26, bw = c.width - pad * 2, bh = c.height - pad * 2, r = 40;
        x.beginPath();
        x.moveTo(pad + r, pad);
        x.arcTo(pad + bw, pad, pad + bw, pad + bh, r);
        x.arcTo(pad + bw, pad + bh, pad, pad + bh, r);
        x.arcTo(pad, pad + bh, pad, pad, r);
        x.arcTo(pad, pad, pad + bw, pad, r);
        x.closePath();
        x.fillStyle = "rgba(10, 8, 7, .78)";
        x.fill();
        x.strokeStyle = state.theme === "sky" ? "rgba(255, 190, 205, .5)" : "rgba(255, 214, 150, .45)";
        x.lineWidth = 3;
        x.stroke();

        const main = state.theme === "sky" ? "#ffeef3" : "#ffedcd";
        const sub = state.theme === "sky" ? "#ffabbf" : "#ffc46e";

        const fit = (text, font, maxWidth) => {
            x.font = font;
            if (x.measureText(text).width <= maxWidth) return text;
            let t = text;
            while (t.length > 1 && x.measureText(t + "…").width > maxWidth) t = t.slice(0, -1);
            return t + "…";
        };

        x.textAlign = "center";
        x.textBaseline = "middle";
        x.shadowColor = sub;
        x.shadowBlur = 16;
        x.fillStyle = main;
        x.fillText(fit(title, "600 74px 'Songti SC', serif", 860), 512, 96);
        const subline = state.trackArtist || "";
        if (subline) {
            x.shadowBlur = 10;
            x.fillStyle = sub;
            x.fillText(fit(subline, "30px ui-monospace, monospace", 820), 512, 188);
        }
        state.lidHud.visible = true;
        state.lidHudTex.needsUpdate = true;
    }

    // 前立面：青铜拉丝面板 + 刻字
    function drawFrontPanel(state) {
        const c = document.createElement("canvas");
        c.width = 2048; c.height = 256;
        const x = c.getContext("2d");
        const g = x.createLinearGradient(0, 0, 0, 256);
        if (state.theme === "sky") {
            g.addColorStop(0, "#2a2a30");
            g.addColorStop(.5, "#1f1f25");
            g.addColorStop(1, "#16161b");
        } else {
            g.addColorStop(0, "#5a4526");
            g.addColorStop(.5, "#43311a");
            g.addColorStop(1, "#33250f");
        }
        x.fillStyle = g;
        x.fillRect(0, 0, 2048, 256);
        for (let i = 0; i < 220; i++) {
            const y0 = Math.random() * 256;
            x.strokeStyle = `rgba(${Math.random() > .5 ? "255,230,180" : "20,12,4"},${.03 + Math.random() * .05})`;
            x.lineWidth = .8;
            x.beginPath();
            x.moveTo(0, y0);
            x.lineTo(2048, y0 + (Math.random() - .5) * 3);
            x.stroke();
        }
        /* 不在左侧刻大字：会压住左旋钮。型号行放在左旋钮与计数窗之间的空档。 */
        x.fillStyle = state.theme === "sky" ? "rgba(242,169,189,.72)" : "rgba(227,192,124,.72)";
        x.font = "26px ui-monospace, monospace";
        x.textAlign = "left";
        x.textBaseline = "middle";
        x.fillText("CUT & CUE · STEREO ARCHIVE", 450, 128);
        const tex = new THREE.CanvasTexture(c);
        tex.encoding = THREE.sRGBEncoding;
        tex.anisotropy = 8;
        return tex;
    }

    /* ---------------- 场景构建 ---------------- */

    function buildScene(container) {
        const state = {
            container, deck: null,
            renderer: null, scene: null, camera: null,
            cam: { radius: 8.8, theta: 0.65, phi: 1.2 },
            camBase: { radius: 8.8, theta: 0.65, phi: 1.2 },
            camOffset: { theta: 0, phi: 0, radius: 0 },
            camOffsetTgt: { theta: 0, phi: 0, radius: 0 },
            camTarget: null,
            needle: "parked",   // "parked"(回架) | "down"(落针)
            cur: {
                recordY: 1.15, speed: 0, armLift: 0, armAngle: -1.59, spin: 0, boost: 0, flash: 0,
                recordX: 0, scratchV: 0, scratchTime: 0, knobFlick: 0, leverFlick: 0
            },
            recordGroup: null, armLiftG: null, armSwingG: null,
            ledMat: null, sheen: null, plinthMat: null, groundMat: null, frontMat: null,
            recordCanvas: null, recordTex: null, recordMat: null,
            counterCanvas: null, counterTex: null, counterMat: null,
            plateMesh: null, counterMesh: null, frontPanelMesh: null,
            recordMesh: null, leverMesh: null, flickKnob: null,
            hitTargets: [], platterPos: null, platterX: 0, demo: false, scratching: false,
            scratchAudio: false, prevVolume: 1,
            peaks: FAKE_PEAKS, coverImg: null,
            theme: "archive",
            stats: [], statIdx: 0, statTimer: 0,
            awake: false, busy: false, raf: 0,
            platterTop: 0.78, armPark: -1.59, armPlay: -2.17,
            trackTitle: "", trackArtist: ""
        };

        const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true });
        renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
        renderer.shadowMap.enabled = true;
        renderer.shadowMap.type = THREE.PCFSoftShadowMap;
        renderer.outputEncoding = THREE.sRGBEncoding;
        renderer.toneMapping = THREE.ACESFilmicToneMapping;
        renderer.toneMappingExposure = 1.1;
        state.renderer = renderer;

        const scene = new THREE.Scene();
        state.scene = scene;
        makeEnvTexture(renderer, scene);

        state.camera = new THREE.PerspectiveCamera(34, 2, 0.1, 100);
        state.camTarget = new THREE.Vector3(0, 0.6, 0.25);

        // 暖色电影光：左侧窗光为主光
        scene.add(new THREE.HemisphereLight(0xffe8c8, 0x4a3a26, 0.42));

        const key = new THREE.DirectionalLight(0xffd9a0, 1.35);
        key.position.set(-5.5, 5.5, 3);
        key.castShadow = true;
        key.shadow.mapSize.set(2048, 2048);
        key.shadow.camera.left = key.shadow.camera.bottom = -6;
        key.shadow.camera.right = key.shadow.camera.top = 6;
        key.shadow.radius = 7;
        scene.add(key);
        state.keyLight = key;

        const fill = new THREE.PointLight(0x9db8c8, 0.3, 30);
        fill.position.set(-4.5, 3, -2.5);
        scene.add(fill);

        const sheen = new THREE.PointLight(0xfff2dd, 0.55, 12);
        sheen.position.set(-0.6, 3.4, 0.4);
        scene.add(sheen);
        state.sheen = sheen;

        // 只接收阴影的透明地面：不要桌面，唱机直接落在页面背景上
        const groundMat = new THREE.ShadowMaterial({ opacity: 0.26 });
        state.groundMat = groundMat;
        const ground = new THREE.Mesh(new THREE.PlaneGeometry(60, 60), groundMat);
        ground.rotation.x = -Math.PI / 2;
        ground.receiveShadow = true;
        scene.add(ground);

        /* ---------- 机身：木框 + 黑色台面 ---------- */

        const deck = new THREE.Group();
        scene.add(deck);
        state.deck = deck;

        const plinthMat = new THREE.MeshStandardMaterial({ roughness: .5, metalness: .08 });
        state.plinthMat = plinthMat;
        const plinth = new THREE.Mesh(new THREE.BoxGeometry(5.8, 0.62, 4.4), plinthMat);
        plinth.position.y = 0.31;
        plinth.castShadow = plinth.receiveShadow = true;
        deck.add(plinth);

        // 黑色台面（内嵌于木框）
        const topPlate = new THREE.Mesh(
            new THREE.BoxGeometry(5.35, 0.06, 3.95),
            new THREE.MeshStandardMaterial({ color: 0x17151a, roughness: .42, metalness: .25 })
        );
        state.topPlateMat = topPlate.material;
        topPlate.position.y = 0.65;
        topPlate.castShadow = topPlate.receiveShadow = true;
        deck.add(topPlate);

        /* ---------- 前立面：拉丝面板 + 计数窗 + 实体旋钮 ---------- */

        state.frontMat = new THREE.MeshStandardMaterial({ roughness: .45, metalness: .5 });
        state.frontPanelMesh = new THREE.Mesh(new THREE.PlaneGeometry(5.5, 0.52), state.frontMat);
        state.frontPanelMesh.position.set(0, 0.31, 2.203);
        deck.add(state.frontPanelMesh);

        state.counterCanvas = document.createElement("canvas");
        state.counterCanvas.width = 512;
        state.counterCanvas.height = 128;
        state.counterTex = new THREE.CanvasTexture(state.counterCanvas);
        state.counterTex.encoding = THREE.sRGBEncoding;
        state.counterMat = new THREE.MeshStandardMaterial({
            map: state.counterTex,
            emissive: 0xffffff,
            emissiveMap: state.counterTex,
            emissiveIntensity: .8,
            roughness: .3
        });
        state.counterMesh = new THREE.Mesh(new THREE.PlaneGeometry(1.15, 0.28), state.counterMat);
        state.counterMesh.position.set(0.62, 0.31, 2.208);
        deck.add(state.counterMesh);

        const knobMat = new THREE.MeshStandardMaterial({ color: 0xc8a468, metalness: .95, roughness: .3 });
        const knobMeshes = [];
        // 左小右大，参考前立面布局
        [[-2.15, 0.13], [1.85, 0.19]].forEach(([kx, kr]) => {
            const knob = new THREE.Mesh(new THREE.CylinderGeometry(kr, kr * 1.06, 0.14, 36), knobMat);
            knob.rotation.x = Math.PI / 2;
            knob.position.set(kx, 0.31, 2.24);
            knob.castShadow = true;
            deck.add(knob);
            knobMeshes.push(knob);
            const pointerMark = new THREE.Mesh(
                new THREE.BoxGeometry(0.018, 0.05, 0.02),
                new THREE.MeshStandardMaterial({ color: 0x2a2016 })
            );
            pointerMark.position.set(kx, 0.31 + kr * 0.55, 2.315);
            deck.add(pointerMark);
        });

        /* ---------- 转盘：金属边 + 胶垫 ---------- */

        const PLATTER = { x: -0.4, z: 0.1 };

        const rim = new THREE.Mesh(
            new THREE.CylinderGeometry(1.95, 1.99, 0.1, 96),
            new THREE.MeshStandardMaterial({ color: 0xd8d4c8, metalness: .95, roughness: .22 })
        );
        rim.position.set(PLATTER.x, 0.73, PLATTER.z);
        rim.castShadow = rim.receiveShadow = true;
        deck.add(rim);

        // 云雾主题：氛围灯环包在整个转盘外沿，光从彩胶边缘透出来
        const glowRing = new THREE.Mesh(
            new THREE.CylinderGeometry(2.01, 2.01, 0.05, 96),
            new THREE.MeshBasicMaterial({ color: 0xff9d85 })
        );
        glowRing.position.set(PLATTER.x, 0.71, PLATTER.z);
        glowRing.visible = false;
        deck.add(glowRing);
        state.glowRing = glowRing;

        const mat = new THREE.Mesh(
            new THREE.CylinderGeometry(1.88, 1.88, 0.025, 96),
            new THREE.MeshStandardMaterial({ color: 0x1a191c, roughness: .9 })
        );
        state.platterFeltMat = mat.material;
        mat.position.set(PLATTER.x, 0.785, PLATTER.z);
        mat.receiveShadow = true;
        deck.add(mat);

        // 云雾主题彩胶底光：粉透唱片的发光感（档案室主题下强度为 0）
        const glowLight = new THREE.PointLight(0xffa8c2, 0, 5);
        glowLight.position.set(PLATTER.x, 1.0, PLATTER.z);
        scene.add(glowLight);
        state.glowLight = glowLight;

        const recordGroup = new THREE.Group();
        recordGroup.position.set(PLATTER.x, 0.82, PLATTER.z);
        deck.add(recordGroup);
        state.recordGroup = recordGroup;

        state.recordCanvas = document.createElement("canvas");
        state.recordCanvas.width = state.recordCanvas.height = 1024;
        state.recordTex = new THREE.CanvasTexture(state.recordCanvas);
        state.recordTex.encoding = THREE.sRGBEncoding;
        state.recordTex.anisotropy = renderer.capabilities.getMaxAnisotropy();

        state.recordMat = new THREE.MeshStandardMaterial({
            map: state.recordTex, roughness: .3, metalness: .2
        });
        const recordSideMat = new THREE.MeshStandardMaterial({ color: 0x0a0a0c, roughness: .4 });
        state.recordSideMat = recordSideMat;

        const record = new THREE.Mesh(
            new THREE.CylinderGeometry(1.78, 1.78, 0.035, 96),
            [recordSideMat, state.recordMat, recordSideMat]
        );
        record.castShadow = record.receiveShadow = true;
        recordGroup.add(record);

        const spindle = new THREE.Mesh(
            new THREE.CylinderGeometry(0.032, 0.032, 0.15, 24),
            new THREE.MeshStandardMaterial({ color: 0xd8d2c2, metalness: .9, roughness: .3 })
        );
        spindle.position.y = 0.08;
        recordGroup.add(spindle);

        /* ---------- S 形唱臂 ---------- */

        const metalMat = new THREE.MeshStandardMaterial({ color: 0xd9d4c4, metalness: .95, roughness: .24 });
        const darkMat = new THREE.MeshStandardMaterial({ color: 0x2c2820, roughness: .5, metalness: .3 });
        // 唱臂组件单独一套材质：档案室银、云雾黑（参考粉色唱机图）
        const armMat = new THREE.MeshStandardMaterial({ color: 0xd9d4c4, metalness: .95, roughness: .24 });
        state.armMat = armMat;

        const armBase = new THREE.Group();
        armBase.position.set(2.42, 0.68, -1.28);
        deck.add(armBase);

        const baseDisc = new THREE.Mesh(new THREE.CylinderGeometry(0.2, 0.23, 0.08, 40), armMat);
        baseDisc.position.y = 0.04;
        baseDisc.castShadow = true;
        armBase.add(baseDisc);

        const column = new THREE.Mesh(new THREE.CylinderGeometry(0.08, 0.1, 0.42, 32), armMat);
        column.position.y = 0.28;
        column.castShadow = true;
        armBase.add(column);

        // 万向节
        const gimbal = new THREE.Mesh(new THREE.CylinderGeometry(0.045, 0.045, 0.2, 16), darkMat);
        gimbal.rotation.z = Math.PI / 2;
        gimbal.position.y = 0.44;
        armBase.add(gimbal);

        const armLiftG = new THREE.Group();
        armLiftG.position.y = 0.42;
        armBase.add(armLiftG);
        state.armLiftG = armLiftG;

        const armSwingG = new THREE.Group();
        armSwingG.rotation.y = state.armPark;
        armLiftG.add(armSwingG);
        state.armSwingG = armSwingG;

        // S 形臂杆：CatmullRom 曲线 + 圆管
        const wandCurve = new THREE.CatmullRomCurve3([
            new THREE.Vector3(-0.5, 0.02, 0),
            new THREE.Vector3(0.1, 0, 0.055),
            new THREE.Vector3(0.9, -0.01, -0.045),
            new THREE.Vector3(1.7, -0.04, 0.02),
            new THREE.Vector3(2.28, -0.1, 0.06)
        ]);
        const wand = new THREE.Mesh(new THREE.TubeGeometry(wandCurve, 40, 0.032, 12), armMat);
        wand.castShadow = true;
        armSwingG.add(wand);

        /* 隐形加粗热区：臂杆本身太细难点，套一根看不见的粗管跟着臂走，
           指针判定和 hover 光标都吃它。 */
        const armHit = new THREE.Mesh(
            new THREE.TubeGeometry(wandCurve, 20, 0.2, 8),
            new THREE.MeshBasicMaterial({ transparent: true, opacity: 0, depthWrite: false })
        );
        armSwingG.add(armHit);
        const headshellHit = new THREE.Mesh(
            new THREE.BoxGeometry(0.6, 0.45, 0.4),
            armHit.material
        );
        headshellHit.position.set(2.45, -0.12, 0.06);
        armSwingG.add(headshellHit);

        const counterweight = new THREE.Mesh(new THREE.CylinderGeometry(0.13, 0.13, 0.24, 32), armMat);
        counterweight.rotation.z = Math.PI / 2;
        counterweight.position.set(-0.62, 0.02, 0);
        counterweight.castShadow = true;
        armSwingG.add(counterweight);

        const counterStub = new THREE.Mesh(new THREE.CylinderGeometry(0.028, 0.028, 0.34, 12), armMat);
        counterStub.rotation.z = Math.PI / 2;
        counterStub.position.set(-0.35, 0.02, 0);
        armSwingG.add(counterStub);

        const headshell = new THREE.Mesh(new THREE.BoxGeometry(0.34, 0.05, 0.13), armMat);
        headshell.position.set(2.4, -0.11, 0.06);
        headshell.rotation.y = -0.2;
        headshell.castShadow = true;
        armSwingG.add(headshell);

        // 指提（headshell 上翘起的小杆）
        const fingerLift = new THREE.Mesh(new THREE.CylinderGeometry(0.012, 0.012, 0.16, 8), armMat);
        fingerLift.rotation.z = 0.5;
        fingerLift.position.set(2.52, -0.02, 0.1);
        armSwingG.add(fingerLift);

        const cartridge = new THREE.Mesh(new THREE.BoxGeometry(0.14, 0.09, 0.11), darkMat);
        cartridge.position.set(2.42, -0.17, 0.06);
        armSwingG.add(cartridge);

        const needle = new THREE.Mesh(
            new THREE.CylinderGeometry(0.007, 0.003, 0.07, 8),
            new THREE.MeshStandardMaterial({ color: 0xd8d2c2 })
        );
        needle.position.set(2.5, -0.23, 0.06);
        armSwingG.add(needle);

        // 停靠架（带卡扣）
        const restPost = new THREE.Mesh(new THREE.CylinderGeometry(0.035, 0.045, 0.4, 16), metalMat);
        restPost.position.set(2.42, 0.85, 1.15);
        restPost.castShadow = true;
        deck.add(restPost);
        const restClip = new THREE.Mesh(new THREE.BoxGeometry(0.07, 0.06, 0.18), darkMat);
        restClip.position.set(2.42, 1.06, 1.15);
        deck.add(restClip);

        // 台面上的启动杆
        const switchBase = new THREE.Mesh(new THREE.BoxGeometry(0.22, 0.03, 0.3), darkMat);
        switchBase.position.set(1.95, 0.695, 1.35);
        deck.add(switchBase);
        const switchLever = new THREE.Mesh(new THREE.CylinderGeometry(0.02, 0.02, 0.16, 12), metalMat);
        switchLever.position.set(1.95, 0.78, 1.35);
        switchLever.castShadow = true;
        deck.add(switchLever);

        /* ---------- 防尘盖（烟熏亚克力，打开态） ---------- */

        /* ---------- 防尘盖（打开态） ----------
           真玻璃：transmission 必须给到 1——少一点都不行，
           残余的白色漫反射会把深色内容洗白（实测 r149 管线本身没毛病）。
           档案室=烟熏，云雾=全透（applyTheme 切换） */
        const lidMat = new THREE.MeshPhysicalMaterial({
            color: 0x2a221c,
            transparent: true,
            opacity: 0.22,
            roughness: 0.06,
            metalness: 0,
            transmission: 0,
            thickness: 0.08,
            ior: 1.5,
            clearcoat: 0.5,
            clearcoatRoughness: 0.2,
            side: THREE.DoubleSide,
            depthWrite: false
        });
        lidMat.envMapIntensity = 1.4;
        state.lidMat = lidMat;

        const lid = new THREE.Group();
        lid.position.set(0, 0.68, -2.05);
        deck.add(lid);
        state.lidGroup = lid;

        // 真薄板（有厚度）而不是平面：棱边会吃高光，是「看得见的透明盖」的关键
        const lidPanel = new THREE.Mesh(new THREE.BoxGeometry(5.35, 3.4, 0.05), lidMat);
        lidPanel.position.set(0, 1.62, -0.42);
        lidPanel.rotation.x = -0.24;
        lid.add(lidPanel);

        // 云雾主题全透盖的轮廓线：描整块薄板的 12 条棱
        const lidOutline = new THREE.LineSegments(
            new THREE.EdgesGeometry(new THREE.BoxGeometry(5.35, 3.4, 0.05)),
            new THREE.LineBasicMaterial({ color: 0x93a8b4, transparent: true, opacity: .9 })
        );
        lidOutline.position.copy(lidPanel.position);
        lidOutline.rotation.copy(lidPanel.rotation);
        lidOutline.visible = false;
        lid.add(lidOutline);
        state.lidOutline = lidOutline;

        // 盖面上的斜向高光带：玻璃的「反光痕」，比染底色更像透明亚克力
        const lidSheenC = document.createElement("canvas");
        lidSheenC.width = lidSheenC.height = 256;
        const lsx = lidSheenC.getContext("2d");
        const lsg = lsx.createLinearGradient(30, 226, 226, 30);
        lsg.addColorStop(0, "rgba(255,255,255,0)");
        lsg.addColorStop(.32, "rgba(255,255,255,0)");
        lsg.addColorStop(.45, "rgba(255,255,255,.6)");
        lsg.addColorStop(.55, "rgba(255,255,255,.16)");
        lsg.addColorStop(.68, "rgba(255,255,255,0)");
        lsg.addColorStop(1, "rgba(255,255,255,0)");
        lsx.fillStyle = lsg;
        lsx.fillRect(0, 0, 256, 256);
        const lidSheenTex = new THREE.CanvasTexture(lidSheenC);
        const lidSheen = new THREE.Mesh(
            new THREE.PlaneGeometry(5.35, 3.4),
            new THREE.MeshBasicMaterial({ map: lidSheenTex, transparent: true, depthWrite: false })
        );
        lidSheen.position.set(0, 1.62, -0.415);
        lidSheen.rotation.x = -0.24;
        lidSheen.visible = false;
        lid.add(lidSheen);
        state.lidSheen = lidSheen;

        // 盖沿金属包边
        const lidEdge = new THREE.Mesh(
            new THREE.BoxGeometry(5.35, 0.05, 0.05),
            new THREE.MeshStandardMaterial({ color: 0xc8a468, metalness: .9, roughness: .3 })
        );
        state.lidEdgeMat = lidEdge.material;
        state.lidEdge = lidEdge;
        lidEdge.position.set(0, 3.3, -0.82);
        lidEdge.rotation.x = -0.24;
        lid.add(lidEdge);

        // 铰链
        state.lidHinges = [];
        [-2.3, 2.3].forEach(hx => {
            const hinge = new THREE.Mesh(new THREE.CylinderGeometry(0.05, 0.05, 0.24, 16), darkMat);
            hinge.rotation.z = Math.PI / 2;
            hinge.position.set(hx, 0.68, -2.05);
            deck.add(hinge);
            state.lidHinges.push(hinge);
        });

        // 盖上的铭牌
        const lidBadgeCanvas = document.createElement("canvas");
        lidBadgeCanvas.width = 512; lidBadgeCanvas.height = 96;
        const bx = lidBadgeCanvas.getContext("2d");
        bx.fillStyle = "rgba(30,22,14,.85)";
        bx.fillRect(0, 0, 512, 96);
        bx.fillStyle = "#e3c07c";
        bx.font = "600 44px 'Songti SC', serif";
        bx.textAlign = "center"; bx.textBaseline = "middle";
        bx.fillText("CUT & CUE", 256, 50);
        const lidBadgeTex = new THREE.CanvasTexture(lidBadgeCanvas);
        lidBadgeTex.encoding = THREE.sRGBEncoding;
        const lidBadge = new THREE.Mesh(
            new THREE.PlaneGeometry(0.95, 0.18),
            new THREE.MeshStandardMaterial({ map: lidBadgeTex, roughness: .5, transparent: true })
        );
        lidBadge.position.set(0.9, 2.9, -0.66);
        lidBadge.rotation.x = -0.24;
        lid.add(lidBadge);
        state.lidBadge = lidBadge;

        /* 盖面上的曲目信息窗：装片后显示歌名 + 艺人，
           像贴在防尘盖内侧的一行小荧光字。 */
        state.lidHudCanvas = document.createElement("canvas");
        state.lidHudCanvas.width = 1024;
        state.lidHudCanvas.height = 256;
        state.lidHudTex = new THREE.CanvasTexture(state.lidHudCanvas);
        state.lidHudTex.encoding = THREE.sRGBEncoding;
        state.lidHudTex.anisotropy = 4;
        state.lidHudMat = new THREE.MeshBasicMaterial({
            map: state.lidHudTex,
            transparent: true,
            depthWrite: false
        });
        state.lidHud = new THREE.Mesh(new THREE.PlaneGeometry(2.5, 0.62), state.lidHudMat);
        state.lidHud.position.set(0, 1.95, -0.44);
        state.lidHud.rotation.x = -0.24;
        state.lidHud.visible = false;
        state.lidHud.renderOrder = 2;
        lid.add(state.lidHud);

        /* ---------- LED 电源灯 ---------- */

        const ledMat = new THREE.MeshStandardMaterial({ color: 0x5a5142 });
        state.ledMat = ledMat;
        const led = new THREE.Mesh(new THREE.SphereGeometry(0.04, 16, 12), ledMat);
        led.position.set(2.15, 0.7, 1.62);
        deck.add(led);

        /* ---------- 可交互部件：唱片 / 唱臂 / 拨杆 / 旋钮 ---------- */

        record.userData.part = "record";
        armSwingG.traverse(o => { o.userData.part = "arm"; });
        switchLever.userData.part = "lever";
        switchBase.userData.part = "lever";
        knobMeshes.forEach(k => { k.userData.part = "knob"; });

        state.recordMesh = record;
        state.leverMesh = switchLever;
        state.hitTargets = [record, armSwingG, switchLever, switchBase, ...knobMeshes];
        state.platterPos = new THREE.Vector3(PLATTER.x, 0.82, PLATTER.z);
        state.platterX = PLATTER.x;

        /* ---------- 互动：相机环绕缩放 + 点唱机本体 ---------- */

        let dragging = false, lastX = 0, lastY = 0;
        let downX = 0, downY = 0, lastAngle = 0;
        const el = renderer.domElement;
        el.style.cursor = "grab";
        el.style.display = "block";
        el.style.width = "100%";
        el.style.height = "100%";
        el.style.touchAction = "none";

        const raycaster = new THREE.Raycaster();

        function pointerHit(e) {
            const rect = el.getBoundingClientRect();
            raycaster.setFromCamera(new THREE.Vector2(
                ((e.clientX - rect.left) / rect.width) * 2 - 1,
                -((e.clientY - rect.top) / rect.height) * 2 + 1
            ), state.camera);
            return raycaster.intersectObjects(state.hitTargets, true)
                .find(h => h.object.userData.part);
        }

        // 指针相对盘心的转角（屏幕空间），搓碟用
        function platterAngle(e) {
            const rect = el.getBoundingClientRect();
            const v = state.platterPos.clone().project(state.camera);
            const cx = rect.left + (v.x * 0.5 + 0.5) * rect.width;
            const cy = rect.top + (-v.y * 0.5 + 0.5) * rect.height;
            return Math.atan2(e.clientY - cy, e.clientX - cx);
        }

        el.addEventListener("pointerdown", e => {
            downX = e.clientX; downY = e.clientY;
            const hit = pointerHit(e);
            if (hit && hit.object.userData.part === "record") {
                // 抓住唱片：进入搓碟而不是环绕相机；播放中联动音频
                state.scratching = true;
                state.cur.scratchV = 0;
                state.cur.scratchTime = 0;
                lastAngle = platterAngle(e);
                if (isPlaying()) {
                    state.scratchAudio = true;
                    state.prevVolume = audio.volume;
                    audio.volume = 0.3;
                }
            } else {
                dragging = true;
                lastX = e.clientX; lastY = e.clientY;
            }
            el.setPointerCapture(e.pointerId);
            el.style.cursor = "grabbing";
        });

        el.addEventListener("pointermove", e => {
            if (state.scratching) {
                // 屏幕角增量（y 向下、顺时针为正）翻成盘面旋转方向
                const a = platterAngle(e);
                let d = lastAngle - a;
                if (d > Math.PI) d -= Math.PI * 2;
                if (d < -Math.PI) d += Math.PI * 2;
                state.cur.spin += d;
                state.cur.scratchV = state.cur.scratchV * 0.7 + d * 0.3;
                // 一圈 = 1.8s（33⅓ RPM），换算成音频时间交给渲染帧节流应用
                state.cur.scratchTime += d / (Math.PI * 2) * 1.8;
                lastAngle = a;
                return;
            }
            if (!dragging) {
                // 悬停反馈：可点的部件上换成手型
                el.style.cursor = pointerHit(e) ? "pointer" : "grab";
                return;
            }
            state.camOffsetTgt.theta -= (e.clientX - lastX) * 0.006;
            state.camOffsetTgt.phi = Math.min(0.3, Math.max(-0.5,
                state.camOffsetTgt.phi - (e.clientY - lastY) * 0.004));
            lastX = e.clientX; lastY = e.clientY;
        });

        ["pointerup", "pointercancel"].forEach(type =>
            el.addEventListener(type, e => {
                const moved = Math.hypot(e.clientX - downX, e.clientY - downY);
                if (state.scratching) {
                    state.scratching = false;
                    if (state.scratchAudio) {
                        state.scratchAudio = false;
                        audio.volume = state.prevVolume;
                    }
                    if (moved < 6) {
                        // 点按唱片 = 播放/停止（和唱臂同一个两态开关）；
                        // 想定位段落就按住拖动搓碟
                        togglePlay(state);
                    } else {
                        // 松手带一点甩盘惯性
                        state.cur.boost = Math.max(-2.5, Math.min(2.5, state.cur.scratchV * 26));
                    }
                } else if (moved < 6) {
                    const hit = pointerHit(e);
                    const part = hit?.object.userData.part;
                    if (part === "arm" || part === "lever") {
                        togglePlay(state);
                    }
                    if (part === "lever") state.cur.leverFlick = 1;
                    if (part === "knob") {
                        state.flickKnob = hit.object;
                        state.cur.knobFlick = 1;
                    }
                }
                dragging = false;
                el.style.cursor = "grab";
            })
        );

        // 滚轮推拉镜头看细节；悬停在旋钮上时滚轮改为调音量
        el.addEventListener("wheel", e => {
            e.preventDefault();
            const hit = pointerHit(e);
            if (hit?.object.userData.part === "knob" && audio) {
                audio.volume = Math.min(1, Math.max(0, audio.volume - e.deltaY * 0.0012));
                state.flickKnob = hit.object;
                state.cur.knobFlick = Math.min(1, Math.abs(e.deltaY) * 0.01 + 0.3);
                return;
            }
            state.camOffsetTgt.radius = Math.min(3.2, Math.max(-3.4,
                state.camOffsetTgt.radius + e.deltaY * 0.004));
        }, { passive: false });
        el.addEventListener("dblclick", () => {
            state.camOffsetTgt.theta = 0;
            state.camOffsetTgt.phi = 0;
            state.camOffsetTgt.radius = 0;
        });

        return state;
    }

    /* ---------------- 动作 ---------------- */

    /* 33⅓ RPM = 0.5556 圈/秒 ≈ 3.49 rad/s；IDLE 是通电空转 */
    const FULL_SPEED = 3.49;
    const IDLE_SPEED = 0.9;

    /* 声槽几何：外圈半径 1.7 → 臂角 -2.13，内圈半径 0.75 → 臂角 -2.58。
       臂角随播放进度从外圈走向内圈，就是黑胶的「进度条」。 */
    const GROOVE_OUT = 1.7, GROOVE_IN = 0.75;
    const ARM_GROOVE_OUT = -2.13, ARM_GROOVE_IN = -2.58;

    function grooveAngle(p) {
        return ARM_GROOVE_OUT + (ARM_GROOVE_IN - ARM_GROOVE_OUT) * Math.min(1, Math.max(0, p));
    }

    function playProgress() {
        if (!audio || !Number.isFinite(audio.duration) || audio.duration <= 0) return 0;
        return Math.min(1, Math.max(0, audio.currentTime / audio.duration));
    }

    function ledOn(state) {
        state.ledMat.emissive.setHex(state.theme === "sky" ? 0x63b7cc : 0xe8b34c);
        state.ledMat.emissiveIntensity = 1.2;
    }

    function ledOff(state) {
        state.ledMat.emissive.setHex(0x000000);
    }

    /* 落针：回架 → 抬臂 → 对准当前进度的声槽 → 液压缓落 → 出声。 */
    function needleDropSequence(state, cb) {
        if (state.busy) { cb?.(); return; }
        state.busy = true;
        state.cur.armLiftT = 0.16;
        state.cur.speedT = FULL_SPEED;   // 转盘先转起来，再落针
        setTimeout(() => { state.cur.armAngleT = grooveAngle(playProgress()); }, 380);
        setTimeout(() => {
            state.cur.armLiftT = 0;
            state.needle = "down";
            ledOn(state);
        }, 1080);
        setTimeout(() => { state.busy = false; cb?.(); }, 1250);
    }

    /* 回架停转：抬臂 → 摆回臂架 → 落架 → 转盘缓停。 */
    function needleParkSequence(state, cb) {
        if (state.busy) { cb?.(); return; }
        state.busy = true;
        state.cur.armLiftT = 0.16;
        setTimeout(() => { state.cur.armAngleT = state.armPark; }, 350);
        setTimeout(() => {
            state.cur.armLiftT = 0;
            state.needle = "parked";
            ledOff(state);
            state.cur.speedT = 0;
            state.busy = false;
            /* 回架途中新曲目已经开播（连播切歌）：
               落针编排被 busy 挡掉了，在这里补上。 */
            if (isPlaying()) {
                needleDropSequence(state, cb);
            } else {
                cb?.();
            }
        }, 1000);
    }

    /* 没装片时点了播放：优先让应用侧决定放哪张（比如最近添加），
       应用侧没接管（曲库为空）就进演示态——唱臂落下、转盘空转。 */
    function handleNoTrack(state) {
        if (state.demo) {
            state.demo = false;
            needleParkSequence(state);
            return;
        }
        if (emptyDropHandler && emptyDropHandler() === true) return;
        state.demo = true;
        needleDropSequence(state);
    }

    /* 点唱臂 / 盘面 / 拨杆都是同一个两态开关：
       停 → 落针、LED 亮、转盘起转、出声；
       播 → 收针回架、LED 灭、转盘缓停、停声。 */
    function togglePlay(state) {
        if (audio && audio.currentSrc) {
            if (audio.paused) {
                needleDropSequence(state, () => audio.play().catch(() => {}));
            } else {
                audio.pause();
            }
            return;
        }
        handleNoTrack(state);
    }

    function playIntro(state) {
        if (reducedMotion.matches) {
            state.cur.recordY = 0;
            state.cur.speed = IDLE_SPEED;
            return;
        }
        setTimeout(() => { state.cur.recordYT = 0; }, 200);
        setTimeout(() => { state.cur.speedT = IDLE_SPEED; }, 1300);
    }

    /* ---------------- 渲染循环 ---------------- */

    function placeCamera(state) {
        const { cam, camTarget, camera } = state;
        camera.position.set(
            camTarget.x + cam.radius * Math.sin(cam.phi) * Math.sin(cam.theta),
            camTarget.y + cam.radius * Math.cos(cam.phi),
            camTarget.z + cam.radius * Math.sin(cam.phi) * Math.cos(cam.theta)
        );
        camera.lookAt(camTarget);
    }

    function resize(state) {
        const w = state.container.clientWidth;
        const h = state.container.clientHeight;
        if (!w || !h) return;
        const dpr = Math.min(window.devicePixelRatio || 1, 2);
        const canvas = state.renderer.domElement;
        if (canvas.width === Math.round(w * dpr) && canvas.height === Math.round(h * dpr)) return;
        state.renderer.setSize(w, h, false);
        state.camera.aspect = w / h;
        state.camera.updateProjectionMatrix();

        /* 取景适配：FOV 只管垂直方向，容器越扁整机越容易被上下裁掉。
           垂直项按斜俯机位反推——机位俯角约 21°，防尘盖顶比注视点高约 3.1
           个单位（注视点已下移 0.3），留 ~0.5 余量保证盖顶不被顶栏裁掉；
           水平项保证木框加唱臂入画。 */
        const halfTan = Math.tan(state.camera.fov * Math.PI / 360);
        state.camBase.radius = Math.max(
            3.58 / halfTan,
            3.55 / (halfTan * state.camera.aspect)
        );
    }

    function frame(state) {
        state.raf = 0;
        const cur = state.cur;

        cur.recordY += ((cur.recordYT ?? 0) - cur.recordY) * 0.055;
        cur.recordX += ((cur.recordXT ?? 0) - cur.recordX) * 0.055;

        /* 电机逻辑：落针播放、演示态都是满速 33⅓；回架后用 speedT 缓停到 0。 */
        const motorOn = isPlaying() || state.demo || state.needle !== "parked";
        const speedTgt = (state.scratching ? 0 : (motorOn ? FULL_SPEED : (cur.speedT ?? 0))) + cur.boost;
        cur.speed += (speedTgt - cur.speed) * 0.02;
        cur.spin += cur.speed / 60;
        cur.boost *= 0.97;
        cur.flash *= 0.94;

        /* 搓碟联动音频：盘角增量换算成时间（一圈 = 1.8s），
           在渲染帧里节流地改写播放位置。 */
        if (state.scratching && state.scratchAudio && Math.abs(cur.scratchTime) > 0.015) {
            const dur = Number.isFinite(audio.duration) ? audio.duration : 0;
            if (dur > 0) {
                audio.currentTime = Math.min(dur, Math.max(0, audio.currentTime + cur.scratchTime));
            }
            cur.scratchTime = 0;
        }

        /* 落针播放中：唱臂跟着进度从外圈走向内圈 */
        if (state.needle === "down" && isPlaying()) {
            cur.armAngleT = grooveAngle(playProgress());
        }

        cur.armLift += ((cur.armLiftT ?? 0) - cur.armLift) * 0.16;
        cur.armAngle += ((cur.armAngleT ?? state.armPark) - cur.armAngle) * 0.045;

        state.camOffset.theta += (state.camOffsetTgt.theta - state.camOffset.theta) * 0.09;
        state.camOffset.phi += (state.camOffsetTgt.phi - state.camOffset.phi) * 0.09;
        state.camOffset.radius += (state.camOffsetTgt.radius - state.camOffset.radius) * 0.09;

        state.recordGroup.position.x = state.platterX + cur.recordX;
        state.recordGroup.position.y = 0.82 + cur.recordY;
        state.recordGroup.rotation.y = cur.spin;
        state.armLiftG.position.y = 0.42 + cur.armLift;
        state.armSwingG.rotation.y = cur.armAngle;

        /* 待机呼吸：完全停机时 LED 缓慢起伏，暗示「可以点我」；
           落针播放 / 演示空转时由 ledOn 常亮接管。 */
        if (state.needle === "parked" && !state.demo && !isPlaying()) {
            const t = performance.now() / 1000;
            state.ledMat.emissive.setHex(state.theme === "sky" ? 0x63b7cc : 0xe8b34c);
            state.ledMat.emissiveIntensity = 0.2 + 0.16 * (0.5 + 0.5 * Math.sin(t * 1.6));
        }

        /* 旋钮拧动 / 拨杆回弹 */
        if (state.flickKnob && cur.knobFlick > 0.02) {
            state.flickKnob.rotation.y = Math.sin(cur.knobFlick * 12) * cur.knobFlick * 0.5;
            cur.knobFlick *= 0.9;
        } else if (state.flickKnob) {
            state.flickKnob.rotation.y = 0;
            state.flickKnob = null;
            cur.knobFlick = 0;
        }

        if (state.leverMesh && cur.leverFlick > 0.02) {
            state.leverMesh.rotation.x = -Math.sin(cur.leverFlick * Math.PI) * 0.55;
            cur.leverFlick *= 0.88;
        } else if (state.leverMesh && state.leverMesh.rotation.x !== 0) {
            state.leverMesh.rotation.x = 0;
            cur.leverFlick = 0;
        }

        if (cur.flash > 0.02) {
            state.counterMat.emissiveIntensity = .8 + cur.flash * 1.6;
        } else if (state.counterMat.emissiveIntensity !== .8) {
            state.counterMat.emissiveIntensity = .8;
        }

        state.cam.theta = state.camBase.theta + state.camOffset.theta;
        state.cam.phi = state.camBase.phi + state.camOffset.phi;
        state.cam.radius = state.camBase.radius + state.camOffset.radius;
        placeCamera(state);

        resize(state);
        state.renderer.render(state.scene, state.camera);

        if (state.awake && !document.hidden) {
            state.raf = requestAnimationFrame(() => frame(state));
        }
    }

    function startLoop(state) {
        if (!state.raf && !reducedMotion.matches) {
            state.raf = requestAnimationFrame(() => frame(state));
        } else if (reducedMotion.matches) {
            frame(state);
        }
    }

    /* ---------------- 对外 API ---------------- */

    function mount(container) {
        if (!window.THREE) return;
        if (!S) {
            S = buildScene(container);
            S.theme = document.body.dataset.theme === "sky" ? "sky" : "archive";
            S.trackTitle = lastTrackInfo.title;
            S.trackArtist = lastTrackInfo.artist;
            applyTheme(S);
            drawRecord(S);
            drawCounter(S);
            drawLidHud(S);
            container.appendChild(S.renderer.domElement);

            if (audio && !audio.paused && audio.currentSrc) {
                S.needle = "down";
                S.cur.armAngle = S.cur.armAngleT = grooveAngle(playProgress());
                S.cur.recordY = 0;
                ledOn(S);
            } else if (!hasIntro) {
                playIntro(S);
                hasIntro = true;
            }
        } else if (S.renderer.domElement.parentElement !== container) {
            S.container = container;
            container.appendChild(S.renderer.domElement);
        } else {
            S.container = container;
        }

        S.awake = true;

        if (pendingStats && !S.stats.length) {
            S.stats = pendingStats;
            drawCounter(S);
        }

        // 每次回到首页都同步真实播放姿态（在曲库里点的播放也算数）
        if (!S.busy) {
            if (isPlaying()) {
                S.needle = "down";
                S.cur.armAngle = S.cur.armAngleT = grooveAngle(playProgress());
                S.cur.armLift = S.cur.armLiftT = 0;
                ledOn(S);
            } else {
                // 没在播：无论暂停还是收针停止，针都待在臂架上
                S.needle = "parked";
                S.cur.armAngle = S.cur.armAngleT = S.armPark;
                S.cur.armLift = S.cur.armLiftT = 0;
                ledOff(S);
            }
        }

        // 从工作区返回：镜头从推进位拉回
        if (hasDived) {
            hasDived = false;
            S.camOffsetTgt.theta = 0;
            S.camOffsetTgt.phi = 0;
            S.camOffsetTgt.radius = 0;
        }

        if (!S.statTimer && S.stats.length > 1) {
            S.statTimer = setInterval(() => {
                if (!S.awake) return;
                S.statIdx = (S.statIdx + 1) % S.stats.length;
                drawCounter(S);
            }, 4000);
        }

        startLoop(S);
    }

    function sleep() {
        if (!S) return;
        S.awake = false;
        if (S.raf) cancelAnimationFrame(S.raf);
        S.raf = 0;
    }

    function react(view) {
        if (!S || !S.awake) return;
        if (view === "all") {
            S.cur.boost = 1.2;
        } else if (view === "clips" && !S.busy) {
            S.cur.armLiftT = 0.09;
            setTimeout(() => { S.cur.armLiftT = 0; }, 200);
            setTimeout(() => { S.cur.armLiftT = 0.09; }, 420);
            setTimeout(() => { S.cur.armLiftT = 0; }, 640);
        } else if (view === "albums") {
            const start = performance.now();
            const hue = () => {
                const p = (performance.now() - start) / 1500;
                if (p >= 1 || !S) { if (S) S.sheen.color.setHex(0xfff2dd); return; }
                S.sheen.color.setHSL(p, 0.65, 0.72);
                requestAnimationFrame(hue);
            };
            hue();
        } else if (view === "recent") {
            S.cur.flash = 1;
            if (S.stats.length > 1) {
                S.statIdx = (S.statIdx + 1) % S.stats.length;
                drawCounter(S);
            }
        }
    }

    function setProgram({ counts } = {}) {
        if (!counts) return;
        const stats = [
            { label: "TRACKS", value: counts.tracks ?? 0 },
            { label: "CLIPS", value: counts.clips ?? 0 },
            { label: "ALBUMS", value: counts.albums ?? 0 },
            { label: "TAGS", value: counts.tags ?? 0 }
        ];
        if (S) {
            S.stats = stats;
            drawCounter(S);
            if (!S.statTimer && S.awake) {
                S.statTimer = setInterval(() => {
                    if (!S.awake) return;
                    S.statIdx = (S.statIdx + 1) % S.stats.length;
                    drawCounter(S);
                }, 4000);
            }
        } else {
            pendingStats = stats;
        }
    }

    function setTrack({ coverArt, grooveUrl, title, artist } = {}) {
        /* 曲目信息窗：title/artist 任一显式给出（含空串）就刷新盖子上的显示；
           唱机还没挂载时先记着，mount 时补上。 */
        if (title !== undefined || artist !== undefined) {
            lastTrackInfo = { title: title || "", artist: artist || "" };
            if (S) {
                S.trackTitle = lastTrackInfo.title;
                S.trackArtist = lastTrackInfo.artist;
                drawLidHud(S);
                if (reducedMotion.matches) frame(S);
            }
        }
        if (coverArt) {
            miniSetCover(coverArt);
            const img = new Image();
            img.onload = () => {
                if (S) { S.coverImg = img; drawRecord(S); if (reducedMotion.matches) frame(S); }
            };
            img.src = coverArt;
        }
        if (grooveUrl && window.Waveform) {
            window.Waveform.getPeaks(grooveUrl, 720)
                .then(peaks => {
                    if (S) { S.peaks = peaks; drawRecord(S); }
                })
                .catch(() => {});
        }
    }

    function applyTheme(state) {
        const isSky = state.theme === "sky";
        if (isSky) {
            // 粉色系云雾机：黑色哑光机身、黑唱臂、粉透彩胶、盘沿氛围灯、全透明盖
            state.plinthMat.map = null;
            state.plinthMat.color.setHex(0x1b1b1f);
            state.plinthMat.roughness = .5;
            state.plinthMat.metalness = .15;
            state.groundMat.opacity = 0.16;
            state.armMat.color.setHex(0x1b1b1f);
            state.armMat.metalness = .55;
            state.armMat.roughness = .5;
            state.recordMat.transparent = true;
            state.recordMat.opacity = .88;
            state.recordMat.roughness = .3;
            state.recordMat.metalness = .05;
            state.recordMat.emissive.setHex(0xff9db8);
            state.recordMat.emissiveIntensity = .42;
            state.recordSideMat.color.setHex(0xe79fb8);
            state.glowLight.intensity = .55;
            state.glowRing.visible = true;
            state.platterFeltMat.color.setHex(0x241a1c);
            state.topPlateMat.color.setHex(0x141318);
            state.topPlateMat.metalness = .2;
            // 云雾：真玻璃盖——transmission 满 1 折射身后机器；
            // 轮廓线和高光带辅助读形；空背景处采样清屏色，调成照片的淡蓝白
            state.lidMat.transparent = false;
            state.lidMat.opacity = 1;
            state.lidMat.transmission = 1;
            state.lidMat.color.setHex(0xffffff);
            state.lidMat.specularColor.setHex(0xfff5ee);
            state.lidMat.envMapIntensity = 1.2;
            // 关掉唱片高光点光源：它会在全透盖上反出一个晃眼的亮圆斑；
            // 粉胶的光泽由自发光 + 盘沿灯环负责
            state.sheen.intensity = 0;
            state.lidEdge.visible = false;
            state.lidOutline.visible = true;
            state.lidSheen.visible = true;
            state.lidBadge.visible = false;
            state.renderer.setClearColor(0xe9f1f5, 0);
            // 与背景照片的桌面视角对齐：镜头放平一点、机位压低一点
            state.camBase.phi = 1.3;
            state.camTarget.y = 1.2;
            state.keyLight.color.setHex(0xffe8dc);
            state.keyLight.intensity = 1.15;
        } else {
            state.plinthMat.map = makeWoodTexture(true);
            state.plinthMat.color.setHex(0xffffff);
            state.plinthMat.roughness = .5;
            state.plinthMat.metalness = .08;
            state.groundMat.opacity = 0.26;
            state.armMat.color.setHex(0xd9d4c4);
            state.armMat.metalness = .95;
            state.armMat.roughness = .24;
            state.recordMat.transparent = false;
            state.recordMat.opacity = 1;
            state.recordMat.roughness = .3;
            state.recordMat.metalness = .2;
            state.recordMat.emissive.setHex(0x000000);
            state.recordMat.emissiveIntensity = 0;
            state.recordSideMat.color.setHex(0x0a0a0c);
            state.glowLight.intensity = 0;
            state.glowRing.visible = false;
            state.platterFeltMat.color.setHex(0x1a191c);
            state.topPlateMat.color.setHex(0x17151a);
            state.topPlateMat.metalness = .25;
            // 档案室：烟熏玻璃盖
            state.lidMat.transparent = true;
            state.lidMat.transmission = 0;
            state.lidMat.opacity = .22;
            state.lidMat.color.setHex(0x2a221c);
            state.lidMat.specularColor.setHex(0xffe0b0);
            state.sheen.intensity = 0.55;
            state.lidEdge.visible = true;
            state.lidOutline.visible = false;
            state.lidSheen.visible = false;
            state.lidBadge.visible = true;
            state.lidMat.envMapIntensity = 1.4;
            state.renderer.setClearColor(0x000000, 0);
            state.camBase.phi = 1.2;
            state.camTarget.y = 0.9;
            state.keyLight.color.setHex(0xffd9a0);
            state.keyLight.intensity = 1.35;
        }
        state.plinthMat.needsUpdate = true;
        state.groundMat.needsUpdate = true;
        state.armMat.needsUpdate = true;
        state.recordMat.needsUpdate = true;
        state.recordSideMat.needsUpdate = true;
        state.lidMat.needsUpdate = true;
        state.lidEdgeMat.needsUpdate = true;
        state.frontMat.map = drawFrontPanel(state);
        state.frontMat.needsUpdate = true;
        drawRecord(state);
        drawCounter(state);
        drawLidHud(state);
        if (reducedMotion.matches) frame(state);
    }

    function setTheme(theme) {
        if (!S) return;
        S.theme = theme === "sky" ? "sky" : "archive";
        applyTheme(S);
    }

    function diveIn(cb) {
        if (!S || !S.awake || reducedMotion.matches) { cb(); return; }
        hasDived = true;
        S.camOffsetTgt.phi = -0.22;
        S.camOffsetTgt.radius = -4.4;
        setTimeout(cb, 850);
    }

    /* 换唱片：针回架 → 旧片升起滑出 → 换标签/波形 → 新片落下。
       cb 在新片落稳后触发（调用方决定何时落针出声）。
       keepRecord：调用方已经用 DOM 做了「飞片上机」，旧片原地换纹理，
       不再升起滑出——否则用户会看着刚飞到的唱片又自己飞走。 */
    function swapRecord(track = {}, cb, { keepRecord = false } = {}) {
        if (!S) { cb?.(); return; }
        if (reducedMotion.matches || !S.awake) {
            setTrack(track);
            S.needle = "parked";
            S.demo = false;
            ledOff(S);
            cb?.();
            return;
        }
        S.busy = true;
        S.demo = false;
        const needPark = S.needle !== "parked";
        const t0 = needPark ? 450 : 80;

        if (needPark) {
            S.cur.armLiftT = 0.16;
            S.cur.armAngleT = S.armPark;
        }

        if (keepRecord) {
            /* DOM 飞片约 0.7s 落到转盘，贴着落点换纹理、转不停 */
            setTimeout(() => {
                S.coverImg = null;
                if (track.coverArt) {
                    setTrack(track);
                } else {
                    setTrack({ grooveUrl: track.grooveUrl, title: track.title, artist: track.artist });
                    drawRecord(S);
                    miniSetCover(null);
                }
            }, t0 + 560);
            setTimeout(() => {
                S.needle = "parked";
                S.cur.armLiftT = 0;
                ledOff(S);
                S.busy = false;
                cb?.();
            }, t0 + 700);
            return;
        }

        // 旧片升起并向左滑出
        setTimeout(() => {
            S.cur.recordYT = 1.5;
            S.cur.recordXT = -4.5;
        }, t0);

        // 旧片飞出后换纹理，新片从上方落回转盘
        setTimeout(() => {
            // 无封面的曲目要回到默认标签，别让上一张的封面留着
            S.coverImg = null;
            if (track.coverArt) {
                setTrack(track);
            } else {
                setTrack({ grooveUrl: track.grooveUrl, title: track.title, artist: track.artist });
                drawRecord(S);
                miniSetCover(null);
            }
            S.cur.spin = 0;
            S.cur.recordXT = 0;
            S.cur.recordYT = 0;
        }, t0 + 650);

        setTimeout(() => {
            S.needle = "parked";
            S.cur.armLiftT = 0;
            ledOff(S);
            S.cur.speedT = 0;
            S.busy = false;
            cb?.();
        }, t0 + 1250);
    }

    /* 落针（对外）：把针落到当前进度并回调（一般用来启动音频）。 */
    function dropNeedle(cb) {
        if (S) {
            needleDropSequence(S, cb);
        } else {
            cb?.();
        }
    }

    /* ---------------- 播放状态监听 ---------------- */

    if (audio) {
        audio.addEventListener("play", () => {
            playing = true;
            miniKick();
            // 应用侧直接起的播放：针若还没落，补一段落针编排
            if (S && S.awake && S.needle !== "down") {
                needleDropSequence(S);
            } else if (S) {
                ledOn(S);
            }
        });
        audio.addEventListener("pause", () => {
            playing = false;
            miniKick();
            // 停止 = 收针回架、转盘缓停（暂停也只有这一种姿态）
            if (S && S.awake && S.needle !== "parked") needleParkSequence(S);
        });
        audio.addEventListener("ended", () => {
            playing = false;
            miniKick();
            // 播完自动回臂、转盘缓停
            if (S && S.awake && S.needle === "down") needleParkSequence(S);
        });
        // 卸载曲目（关闭播放器）：针回架，盖子上的曲目窗也撤掉
        audio.addEventListener("emptied", () => {
            lastTrackInfo = { title: "", artist: "" };
            if (!S) return;
            S.trackTitle = "";
            S.trackArtist = "";
            drawLidHud(S);
            if (S.awake && S.needle !== "parked") needleParkSequence(S);
        });
    }

    document.addEventListener("visibilitychange", () => {
        if (!document.hidden && S && S.awake) startLoop(S);
    });

    reducedMotion.addEventListener("change", () => {
        miniStatic();
        if (S && S.awake) startLoop(S);
    });

    miniDrawDisc();
    miniStatic();

    /* 环绕机位（首页 360° 按钮）：delta 为弧度增量 */
    function orbit(delta) {
        if (!S) return;
        S.camOffsetTgt.theta += delta;
    }

    /* 转盘中心的屏幕坐标（clientX/Y）：唱片墙「飞片上机」的落点。 */
    function platterScreenPoint() {
        if (!S || !S.renderer) return null;
        const rect = S.renderer.domElement.getBoundingClientRect();
        if (!rect.width || !rect.height) return null;
        const v = S.platterPos.clone().project(S.camera);
        return {
            x: rect.left + (v.x * 0.5 + 0.5) * rect.width,
            y: rect.top + (-v.y * 0.5 + 0.5) * rect.height
        };
    }

    /* 空落针（没装片就点唱臂/唱片）时由应用侧接管选片；
       处理器返回 true 表示已开始装片，false 则回落到演示空转。 */
    function setEmptyDropHandler(fn) {
        emptyDropHandler = typeof fn === "function" ? fn : null;
    }

    window.Turntable = {
        mount,
        sleep,
        react,
        setProgram,
        setTrack,
        setTheme,
        diveIn,
        swapRecord,
        dropNeedle,
        orbit,
        platterScreenPoint,
        setEmptyDropHandler
    };
})();
