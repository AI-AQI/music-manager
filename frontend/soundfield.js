/* =========================================================
   声场 — WebGL 流体光背景
   独立于 app.js 运行，不依赖 Tauri。
   缓慢的环境光流动 + 鼠标聚光，不随音乐脉冲（避免频闪感）；
   首页（body.view-home）隐藏并暂停渲染，唱机直接落在纯色背景上。
   降级链：WebGL 不可用 → 保留 CSS 光斑；
   prefers-reduced-motion → 只渲一帧静态画面。
========================================================= */

(function () {
    "use strict";

    const canvas = document.getElementById("soundfield");
    if (!canvas) return;

    const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");

    function disable() {
        document.documentElement.classList.add("no-soundfield");
    }

    const gl = canvas.getContext("webgl", {
        alpha: true,
        antialias: false,
        depth: false,
        stencil: false,
        premultipliedAlpha: false,
        powerPreference: "low-power"
    });

    if (!gl) {
        disable();
        return;
    }

    /* ---------------- 着色器 ---------------- */

    const VERTEX_SRC = `
attribute vec2 a_pos;
void main() {
    gl_Position = vec4(a_pos, 0.0, 1.0);
}
`;

    const FRAGMENT_SRC = `
precision mediump float;

uniform vec2 u_res;
uniform float u_time;
uniform vec2 u_pointer;
uniform float u_strength;
uniform vec3 u_colA;
uniform vec3 u_colB;
uniform vec3 u_colC;

float hash(vec2 p) {
    p = fract(p * vec2(234.34, 435.345));
    p += dot(p, p + 34.23);
    return fract(p.x * p.y);
}

float noise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    float a = hash(i);
    float b = hash(i + vec2(1.0, 0.0));
    float c = hash(i + vec2(0.0, 1.0));
    float d = hash(i + vec2(1.0, 1.0));
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

float fbm(vec2 p) {
    float v = 0.0;
    float a = 0.5;
    for (int i = 0; i < 5; i++) {
        v += a * noise(p);
        p = p * 2.03 + vec2(11.3, 7.9);
        a *= 0.5;
    }
    return v;
}

void main() {
    vec2 p = (gl_FragCoord.xy - 0.5 * u_res) / min(u_res.x, u_res.y);

    float t = u_time * 0.06;

    vec2 q = vec2(
        fbm(p * 1.4 + t),
        fbm(p * 1.4 - t * 0.7)
    );
    vec2 r = vec2(
        fbm(p * 1.8 + 2.2 * q + vec2(1.7, 9.2) + t * 1.3),
        fbm(p * 1.8 + 2.2 * q + vec2(8.3, 2.8) - t)
    );
    float f = fbm(p * 1.6 + 2.0 * r);

    // 缓慢的呼吸感，固定节奏，不跟音乐脉冲
    float breathe = 0.82 + 0.18 * sin(u_time * 0.21);

    // 鼠标聚光
    vec2 m = (u_pointer * u_res - 0.5 * u_res) / min(u_res.x, u_res.y);
    float glow = exp(-length(p - m) * 2.6) * 0.30;

    float flow = smoothstep(0.30, 0.85, f) * 0.62
               + smoothstep(0.45, 0.95, length(q)) * 0.30;

    vec3 light = mix(u_colA, u_colB, clamp(r.x, 0.0, 1.0));
    light = mix(light, u_colC, clamp(glow * 1.2, 0.0, 1.0));

    float alpha = clamp(flow * 0.52 * breathe + glow * 0.6, 0.0, 1.0)
                * u_strength;

    gl_FragColor = vec4(light, alpha);
}
`;

    function compile(type, src) {
        const shader = gl.createShader(type);
        gl.shaderSource(shader, src);
        gl.compileShader(shader);
        if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
            console.warn("soundfield shader:", gl.getShaderInfoLog(shader));
            gl.deleteShader(shader);
            return null;
        }
        return shader;
    }

    const vertexShader = compile(gl.VERTEX_SHADER, VERTEX_SRC);
    const fragmentShader = compile(gl.FRAGMENT_SHADER, FRAGMENT_SRC);

    if (!vertexShader || !fragmentShader) {
        disable();
        return;
    }

    const program = gl.createProgram();
    gl.attachShader(program, vertexShader);
    gl.attachShader(program, fragmentShader);
    gl.linkProgram(program);

    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
        console.warn("soundfield link:", gl.getProgramInfoLog(program));
        disable();
        return;
    }

    gl.useProgram(program);

    const buffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);

    const posLocation = gl.getAttribLocation(program, "a_pos");
    gl.enableVertexAttribArray(posLocation);
    gl.vertexAttribPointer(posLocation, 2, gl.FLOAT, false, 0, 0);

    gl.enable(gl.BLEND);
    gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);

    const uniforms = {};
    ["u_res", "u_time", "u_pointer", "u_strength", "u_colA", "u_colB", "u_colC"]
        .forEach(name => { uniforms[name] = gl.getUniformLocation(program, name); });

    /* ---------------- 主题调色板 ---------------- */

    const PALETTES = {
        archive: {
            strength: 0.55,
            colA: [0.13, 0.46, 0.43],   // 青绿
            colB: [0.90, 0.32, 0.20],   // 朱红
            colC: [0.87, 0.70, 0.36]    // 沙金
        },
        sky: {
            strength: 0.42,
            colA: [0.37, 0.68, 0.76],   // 天青
            colB: [0.91, 0.58, 0.36],   // 晨光橙
            colC: [0.51, 0.47, 0.74]    // 暮紫
        }
    };

    let paletteTarget = PALETTES.archive;
    const paletteCurrent = {
        strength: PALETTES.archive.strength,
        colA: PALETTES.archive.colA.slice(),
        colB: PALETTES.archive.colB.slice(),
        colC: PALETTES.archive.colC.slice()
    };

    function syncPalette() {
        const base = PALETTES[document.body.dataset.theme] || PALETTES.archive;
        paletteTarget = {
            strength: base.strength,
            colA: base.colA,
            colB: base.colB,
            colC: base.colC
        };
    }

    function stepPalette() {
        const k = 0.06;
        paletteCurrent.strength += (paletteTarget.strength - paletteCurrent.strength) * k;
        ["colA", "colB", "colC"].forEach(key => {
            for (let i = 0; i < 3; i++) {
                paletteCurrent[key][i] += (paletteTarget[key][i] - paletteCurrent[key][i]) * k;
            }
        });
    }

    syncPalette();
    new MutationObserver(() => {
        syncPalette();
        start();
    }).observe(document.body, { attributes: true, attributeFilter: ["data-theme", "class"] });

    /* ---------------- 指针 ---------------- */

    const pointer = { x: 0.5, y: 0.42 };
    const pointerTarget = { x: 0.5, y: 0.42 };

    document.addEventListener("pointermove", event => {
        const rect = canvas.getBoundingClientRect();
        if (rect.width < 1 || rect.height < 1) return;
        pointerTarget.x = (event.clientX - rect.left) / rect.width;
        pointerTarget.y = 1 - (event.clientY - rect.top) / rect.height;
    }, { passive: true });

    /* ---------------- 渲染循环 ---------------- */

    function resize() {
        const scale = Math.min(window.devicePixelRatio || 1, 1.5) * 0.5;
        const width = Math.max(1, Math.round(canvas.clientWidth * scale));
        const height = Math.max(1, Math.round(canvas.clientHeight * scale));
        if (canvas.width !== width || canvas.height !== height) {
            canvas.width = width;
            canvas.height = height;
            gl.viewport(0, 0, width, height);
        }
    }

    window.addEventListener("resize", resize);

    function draw(time) {
        resize();
        stepPalette();

        gl.uniform2f(uniforms.u_res, canvas.width, canvas.height);
        gl.uniform1f(uniforms.u_time, time);
        gl.uniform2f(uniforms.u_pointer, pointer.x, pointer.y);
        gl.uniform1f(uniforms.u_strength, paletteCurrent.strength);
        gl.uniform3fv(uniforms.u_colA, paletteCurrent.colA);
        gl.uniform3fv(uniforms.u_colB, paletteCurrent.colB);
        gl.uniform3fv(uniforms.u_colC, paletteCurrent.colC);

        gl.clearColor(0, 0, 0, 0);
        gl.clear(gl.COLOR_BUFFER_BIT);
        gl.drawArrays(gl.TRIANGLES, 0, 3);
    }

    const startTime = performance.now();
    let rafId = 0;

    function frame(now) {
        rafId = 0;
        // 首页不显示声场背景，直接暂停循环
        if (document.body.classList.contains("view-home")) return;
        const t = (now - startTime) / 1000;

        pointer.x += (pointerTarget.x - pointer.x) * 0.07;
        pointer.y += (pointerTarget.y - pointer.y) * 0.07;

        draw(t);

        if (!document.hidden) rafId = requestAnimationFrame(frame);
    }

    function start() {
        if (!rafId && !reducedMotion.matches) rafId = requestAnimationFrame(frame);
    }

    document.addEventListener("visibilitychange", () => {
        if (!document.hidden) start();
    });

    canvas.addEventListener("webglcontextlost", event => {
        event.preventDefault();
        if (rafId) cancelAnimationFrame(rafId);
        rafId = 0;
        disable();
    });

    reducedMotion.addEventListener("change", () => {
        if (reducedMotion.matches) {
            if (rafId) cancelAnimationFrame(rafId);
            rafId = 0;
            draw(12);
        } else {
            start();
        }
    });

    // reduced-motion：只渲一帧静态画面，不启动循环
    if (reducedMotion.matches) {
        draw(12);
    } else {
        start();
    }
})();
