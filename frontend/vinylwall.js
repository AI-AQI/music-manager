/* =========================================================
   首页唱片墙（Three.js）：透明亚克力相框 + 黑胶
   一整块画布渲染所有挂帧：亚克力方板（四角金属螺丝）里
   一张真圆形黑胶，中心是圆形标签（有封面用封面，没封面
   用暖色纸标签 + 首字）。DOM 只管箭头、歌名、ON AIR 小签，
   帧的位置和尺寸直接读 DOM 槽位（.vinyl-box），布局随 CSS。
========================================================= */
(function () {

    const FOV = 26;
    const CAM_Z = 7;
    const SPIN = 3.49;             // 33⅓ RPM，与唱机一致
    const TILT_X = 0.18;           // hover 微倾幅度
    const TILT_Y = 0.24;

    const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");

    let S = null;

    /* ---------------- 纹理 ---------------- */

    let grooveTex = null;
    let shadowTex = null;
    const labelCache = new Map();    // id -> { canvas, tex, mat, item }
    const coverImgCache = new Map(); // id -> Image

    /* 黑胶盘面：近黑底 + 同心声槽 + 两束缎面高光弧 */
    function getGrooveTexture() {
        if (grooveTex) return grooveTex;
        const c = document.createElement("canvas");
        c.width = c.height = 512;
        const x = c.getContext("2d");
        x.fillStyle = "#161312";
        x.fillRect(0, 0, 512, 512);
        x.save();
        x.translate(256, 256);
        for (let r = 58; r < 252; r += 2) {
            x.strokeStyle = `rgba(255,255,255,${(0.04 + 0.045 * Math.abs(Math.sin(r * 0.63))).toFixed(3)})`;
            x.beginPath();
            x.arc(0, 0, r, 0, Math.PI * 2);
            x.stroke();
        }
        [[-0.7, 1], [2.25, 0.65]].forEach(([rot, k]) => {
            x.save();
            x.rotate(rot);
            for (let i = 0; i < 30; i++) {
                x.strokeStyle = `rgba(255,240,222,${(0.3 * k * (1 - i / 30)).toFixed(3)})`;
                x.beginPath();
                x.arc(0, 0, 66 + i * 6.2, 0, 1.2);
                x.stroke();
            }
            x.restore();
        });
        x.restore();
        grooveTex = new THREE.CanvasTexture(c);
        grooveTex.encoding = THREE.sRGBEncoding;
        grooveTex.anisotropy = 4;
        return grooveTex;
    }

    /* 挂在墙上的软阴影：圆角矩形渐变，贴在亚克力板右下 */
    function getShadowTexture() {
        if (shadowTex) return shadowTex;
        const c = document.createElement("canvas");
        c.width = c.height = 256;
        const x = c.getContext("2d");
        x.filter = "blur(10px)";
        x.fillStyle = "rgba(24,18,10,.18)";
        x.beginPath();
        x.roundRect(28, 28, 200, 200, 10);
        x.fill();
        shadowTex = new THREE.CanvasTexture(c);
        return shadowTex;
    }

    function getCoverImg(item, onload) {
        let img = coverImgCache.get(item.id);
        if (img === undefined && item.coverArt) {
            img = new Image();
            img.onload = onload;
            img.src = item.coverArt;
            coverImgCache.set(item.id, img);
        }
        return img && img.complete && img.naturalWidth ? img : null;
    }

    /* 圆形标签：封面圆形裁切，或暖色纸标签 + 首字；中央留轴孔 */
    function redrawLabel(entry) {
        const { canvas, item } = entry;
        const x = canvas.getContext("2d");
        const img = getCoverImg(item, () => redrawLabel(entry));
        x.clearRect(0, 0, 256, 256);
        x.save();
        x.beginPath();
        x.arc(128, 128, 128, 0, Math.PI * 2);
        x.clip();
        if (img) {
            const s = Math.min(img.naturalWidth, img.naturalHeight);
            x.drawImage(img,
                (img.naturalWidth - s) / 2, (img.naturalHeight - s) / 2, s, s,
                0, 0, 256, 256);
            const v = x.createRadialGradient(128, 128, 60, 128, 128, 128);
            v.addColorStop(0, "rgba(0,0,0,0)");
            v.addColorStop(1, "rgba(0,0,0,.38)");
            x.fillStyle = v;
            x.fillRect(0, 0, 256, 256);
        } else {
            const hue = [...(item.name || "♪")].reduce((h, ch) => h + ch.charCodeAt(0), 0) % 360;
            const g = x.createLinearGradient(0, 0, 256, 256);
            g.addColorStop(0, `hsl(${hue}, 48%, 66%)`);
            g.addColorStop(1, `hsl(${(hue + 30) % 360}, 45%, 48%)`);
            x.fillStyle = g;
            x.fillRect(0, 0, 256, 256);
            x.fillStyle = "rgba(255,252,244,.92)";
            x.font = "700 96px 'Songti SC', 'Noto Serif CJK SC', serif";
            x.textAlign = "center";
            x.textBaseline = "middle";
            x.fillText((item.name || "♪").slice(0, 1), 128, 136);
        }
        x.strokeStyle = "rgba(0,0,0,.35)";
        x.lineWidth = 3;
        x.beginPath();
        x.arc(128, 128, 124, 0, Math.PI * 2);
        x.stroke();
        x.restore();
        // 轴孔
        x.fillStyle = "#0b0908";
        x.beginPath();
        x.arc(128, 128, 13, 0, Math.PI * 2);
        x.fill();
        entry.tex.needsUpdate = true;
    }

    function getLabelMaterial(item) {
        let entry = labelCache.get(item.id);
        if (!entry) {
            const canvas = document.createElement("canvas");
            canvas.width = canvas.height = 256;
            const tex = new THREE.CanvasTexture(canvas);
            tex.encoding = THREE.sRGBEncoding;
            entry = {
                canvas,
                tex,
                item,
                mat: new THREE.MeshStandardMaterial({ map: tex, roughness: .55, metalness: .05 })
            };
            labelCache.set(item.id, entry);
        }
        entry.item = item; // 封面可能随后补拉到位
        redrawLabel(entry);
        return entry.mat;
    }

    /* 玻璃上的斜向反光带 */
    let sheenTex = null;
    function getSheenTexture() {
        if (sheenTex) return sheenTex;
        const c = document.createElement("canvas");
        c.width = c.height = 256;
        const x = c.getContext("2d");
        const g = x.createLinearGradient(30, 226, 226, 30);
        g.addColorStop(0, "rgba(255,255,255,0)");
        g.addColorStop(.38, "rgba(255,255,255,0)");
        g.addColorStop(.48, "rgba(255,255,255,.4)");
        g.addColorStop(.55, "rgba(255,255,255,.1)");
        g.addColorStop(.63, "rgba(255,255,255,0)");
        g.addColorStop(1, "rgba(255,255,255,0)");
        x.fillStyle = g;
        x.fillRect(0, 0, 256, 256);
        sheenTex = new THREE.CanvasTexture(c);
        return sheenTex;
    }

    /* ---------------- 共享几何/材质 ---------------- */

    let kit = null;
    function getKit() {
        if (kit) return kit;
        const acrylicGeo = new THREE.BoxGeometry(1, 1, 0.05);
        kit = {
            acrylicGeo,
            edgeGeo: new THREE.EdgesGeometry(new THREE.BoxGeometry(1.002, 1.002, 0.052)),
            discGeo: new THREE.CylinderGeometry(0.36, 0.36, 0.02, 72),
            labelGeo: new THREE.CircleGeometry(0.124, 48),
            postGeo: new THREE.CylinderGeometry(0.03, 0.03, 0.08, 24),
            capGeo: new THREE.CylinderGeometry(0.042, 0.042, 0.016, 24),
            shadowGeo: new THREE.PlaneGeometry(1.32, 1.32),
            sheenGeo: new THREE.PlaneGeometry(1, 1),
            sheenMat: new THREE.MeshBasicMaterial({
                map: getSheenTexture(), transparent: true, opacity: .55, depthWrite: false
            }),
            /* 亚克力：真玻璃。transmission 必须满 1——少一点，
               残余白色漫反射会把深色唱片洗白（实测结论） */
            acrylicMat: new THREE.MeshPhysicalMaterial({
                color: 0xffffff,
                metalness: 0,
                roughness: 0.03,
                transmission: 1,
                thickness: 0.06,
                ior: 1.49,
                clearcoat: 0.5,
                clearcoatRoughness: 0.2,
                envMapIntensity: 1.2
            }),
            edgeMat: new THREE.LineBasicMaterial({ color: 0x6b6357, transparent: true, opacity: 0.55 }),
            metalMat: new THREE.MeshStandardMaterial({
                color: 0xc9cacd, metalness: 1, roughness: 0.3, envMapIntensity: 1.3
            }),
            sideMat: new THREE.MeshStandardMaterial({ color: 0x1c1815, roughness: 0.35 }),
            grooveMat: new THREE.MeshStandardMaterial({
                map: getGrooveTexture(), roughness: 0.28, metalness: 0.05, envMapIntensity: 0.8
            }),
            shadowMat: new THREE.MeshBasicMaterial({
                map: getShadowTexture(), transparent: true, depthWrite: false
            }),
            holeMat: new THREE.MeshBasicMaterial({ color: 0x090807 })
        };
        return kit;
    }

    /* ---------------- 槽位 ---------------- */

    function buildSlot() {
        const k = getKit();
        const group = new THREE.Group();

        const shadow = new THREE.Mesh(k.shadowGeo, k.shadowMat);
        shadow.position.set(0.03, -0.04, -0.08);
        group.add(shadow);

        // 唱片（标签随盘一起转）
        const spin = new THREE.Group();
        const disc = new THREE.Mesh(k.discGeo, [k.sideMat, k.grooveMat, k.sideMat]);
        disc.rotation.x = Math.PI / 2;
        spin.add(disc);
        const label = new THREE.Mesh(k.labelGeo, k.holeMat);
        label.position.z = 0.012;
        spin.add(label);
        group.add(spin);

        // 罩在唱片前面的亚克力板 + 斜向高光带
        const acrylic = new THREE.Mesh(k.acrylicGeo, k.acrylicMat);
        acrylic.position.z = 0.045;
        group.add(acrylic);
        const edges = new THREE.LineSegments(k.edgeGeo, k.edgeMat);
        edges.position.z = 0.045;
        group.add(edges);
        const sheen = new THREE.Mesh(k.sheenGeo, k.sheenMat);
        sheen.position.z = 0.075;
        group.add(sheen);

        // 四角广告钉
        [[-1, -1], [1, -1], [-1, 1], [1, 1]].forEach(([sx, sy]) => {
            const post = new THREE.Mesh(k.postGeo, k.metalMat);
            post.rotation.x = Math.PI / 2;
            post.position.set(sx * 0.44, sy * 0.44, 0.07);
            group.add(post);
            const cap = new THREE.Mesh(k.capGeo, k.metalMat);
            cap.rotation.x = Math.PI / 2;
            cap.position.set(sx * 0.44, sy * 0.44, 0.115);
            group.add(cap);
        });

        group.visible = false;
        S.scene.add(group);
        return {
            group, spin, label, data: null, hasRect: false,
            tiltX: 0, tiltY: 0, tiltXT: 0, tiltYT: 0
        };
    }

    /* ---------------- 场景 ---------------- */

    function makeEnv(renderer, scene) {
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
        // 正前方的亮窗：亚克力的反射光源
        const front = x.createRadialGradient(512, 200, 8, 512, 200, 150);
        front.addColorStop(0, "rgba(255,246,230,.85)");
        front.addColorStop(1, "rgba(255,246,230,0)");
        x.fillStyle = front;
        x.fillRect(0, 0, 1024, 512);

        const tex = new THREE.CanvasTexture(c);
        tex.mapping = THREE.EquirectangularReflectionMapping;
        const pmrem = new THREE.PMREMGenerator(renderer);
        const env = pmrem.fromEquirectangular(tex).texture;
        pmrem.dispose();
        tex.dispose();
        scene.environment = env;
    }

    function build(container) {
        const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true });
        renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
        renderer.outputEncoding = THREE.sRGBEncoding;
        renderer.toneMapping = THREE.ACESFilmicToneMapping;
        renderer.toneMappingExposure = 1.05;
        // transmission 采样空背景处返回清屏色：给浅纸色，玻璃边缘才不会发黑
        renderer.setClearColor(0xf0efe9, 0);

        const scene = new THREE.Scene();
        makeEnv(renderer, scene);

        const camera = new THREE.PerspectiveCamera(FOV, 1, 0.1, 50);
        camera.position.set(0, 0, CAM_Z);
        camera.lookAt(0, 0, 0);

        scene.add(new THREE.HemisphereLight(0xfff2df, 0x4a3a26, 0.5));
        const key = new THREE.DirectionalLight(0xffe0b8, 0.85);
        key.position.set(-3, 4, 5);
        scene.add(key);
        const fill = new THREE.DirectionalLight(0xdfe8ff, 0.28);
        fill.position.set(3.5, 0.5, 4);
        scene.add(fill);

        const el = renderer.domElement;
        el.style.cssText =
            "position:absolute;inset:0;width:100%;height:100%;display:block;pointer-events:none;";

        S = {
            renderer, scene, camera, container,
            slots: [], awake: false, raf: 0,
            playingId: null, lastT: 0, cw: 0, ch: 0
        };
    }

    /* 位置尺寸跟 DOM 槽位走：CSS 怎么排，3D 就怎么挂 */
    function layout() {
        if (!S || !S.container) return;
        const w = S.container.clientWidth;
        const h = S.container.clientHeight;
        if (!w || !h) {
            S.slots.forEach(slot => { slot.hasRect = false; slot.group.visible = false; });
            return;
        }
        const dpr = Math.min(window.devicePixelRatio || 1, 2);
        const el = S.renderer.domElement;
        if (el.width !== Math.round(w * dpr) || el.height !== Math.round(h * dpr)) {
            S.renderer.setSize(w, h, false);
        }
        S.camera.aspect = w / h;
        S.camera.updateProjectionMatrix();

        const visH = 2 * Math.tan(FOV * Math.PI / 360) * CAM_Z;
        const visW = visH * (w / h);
        const wpp = visH / h; // 1 css px 对应的世界单位
        const wallRect = S.container.getBoundingClientRect();
        const boxes = S.container.querySelectorAll(".vinyl-box");

        S.slots.forEach((slot, i) => {
            const box = boxes[i];
            const r = box ? box.getBoundingClientRect() : null;
            if (!r || r.width < 8) {
                slot.hasRect = false;
                slot.group.visible = false;
                return;
            }
            const cx = r.left + r.width / 2 - wallRect.left;
            const cy = r.top + r.height / 2 - wallRect.top;
            slot.group.position.set((cx / w - 0.5) * visW, -(cy / h - 0.5) * visH, 0);
            slot.group.scale.setScalar(r.width * wpp);
            slot.hasRect = true;
            slot.group.visible = !!slot.data;
        });
    }

    /* ---------------- 渲染循环 ---------------- */

    function frame(t) {
        S.raf = 0;
        const dt = Math.min(0.05, (t - (S.lastT || t)) / 1000);
        S.lastT = t;

        // 尺寸变化（窗口缩放 / 媒体查询切换布局）就重排
        if (S.container &&
            (S.cw !== S.container.clientWidth || S.ch !== S.container.clientHeight)) {
            S.cw = S.container.clientWidth;
            S.ch = S.container.clientHeight;
            layout();
        }

        /* transmission 的空背景处采样清屏色：跟随主题，云雾=照片淡蓝白 */
        const sky = document.body.dataset.theme === "sky";
        if (sky !== S.skyClear) {
            S.skyClear = sky;
            S.renderer.setClearColor(sky ? 0xe9f1f5 : 0xf0efe9, 0);
        }

        S.slots.forEach(slot => {
            if (!slot.group.visible) return;
            if (slot.data && slot.data.id === S.playingId) {
                slot.spin.rotation.z -= SPIN * dt;
            }
            slot.tiltX += (slot.tiltXT - slot.tiltX) * 0.12;
            slot.tiltY += (slot.tiltYT - slot.tiltY) * 0.12;
            slot.group.rotation.x = slot.tiltX;
            slot.group.rotation.y = slot.tiltY;
        });

        S.renderer.render(S.scene, S.camera);

        if (S.awake && !document.hidden && !reducedMotion.matches) {
            S.raf = requestAnimationFrame(frame);
        }
    }

    function renderOnce() {
        if (S && !S.raf) S.raf = requestAnimationFrame(frame);
    }

    document.addEventListener("visibilitychange", () => {
        if (!document.hidden && S && S.awake) renderOnce();
    });

    /* ---------------- 对外 API ---------------- */

    function setSlots(data) {
        if (!S) return;
        while (S.slots.length < data.length) S.slots.push(buildSlot());
        S.slots.forEach((slot, i) => {
            const d = data[i] || null;
            slot.data = d;
            if (d) slot.label.material = getLabelMaterial(d);
            slot.group.visible = !!d && slot.hasRect;
        });
        layout();
        renderOnce();
    }

    function mount(container) {
        if (!window.THREE) return;
        if (!S) build(container);
        S.container = container;
        const el = S.renderer.domElement;
        if (el.parentElement !== container) container.appendChild(el);
        S.awake = true;
        S.cw = S.ch = 0; // 强制重排
        layout();
        renderOnce();
    }

    function sleep() {
        if (!S) return;
        S.awake = false;
        if (S.raf) {
            cancelAnimationFrame(S.raf);
            S.raf = 0;
        }
    }

    window.VinylWall = {
        mount,
        sleep,
        setSlots,
        setPlaying(id) {
            if (!S) return;
            S.playingId = id;
            renderOnce();
        },
        setTilt(i, nx, ny) {
            const slot = S && S.slots[i];
            if (!slot) return;
            slot.tiltYT = nx * TILT_Y;
            slot.tiltXT = ny * TILT_X;
            renderOnce();
        }
    };
})();
