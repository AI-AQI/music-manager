/* 预览专用：模拟 Tauri 后端，让 frontend 能在纯浏览器里跑起来。 */
(function () {
    const now = Date.now();
    const DAY = 86400000;
    const mk = (id, name, album, artist, tagIds, days, dur) => ({
        id, name, fileName: name + ".mp3", path: "/Users/qi/Music/素材/" + name + ".mp3",
        album, albumSource: "metadata", artist, genre: "氛围", year: "2024",
        channels: "立体声", sampleRate: 48000, bitrate: 320,
        tags: [], tagIds, duration: dur, fileSize: 7 * 1024 * 1024,
        mimeType: "audio/mpeg", coverArt: "", createdAt: now - days * DAY, updatedAt: now - days * DAY
    });
    const music = [
        mk("m01", "晨光练习曲", "城市速写", "陆离", [4, 6], 2, 194.2),
        mk("m02", "Night Drive", "Midnight Tapes", "KV", [2], 5, 227.8),
        mk("m03", "雨后天台", "城市速写", "陆离", [4, 5], 9, 162.4),
        mk("m04", "长安夜", "东方纪行", "闻笛", [1, 5], 16, 251.3),
        mk("m05", "Glasshouse", "Midnight Tapes", "KV", [2, 8], 20, 203.0),
        mk("m06", "告别式", "东方纪行", "闻笛", [3], 26, 276.9),
        mk("m07", "Paper Moon", "Slow Cinema", "Aoba", [4, 7], 40, 234.1),
        mk("m08", "深海邮局", "Slow Cinema", "Aoba", [3, 2], 52, 301.7)
    ];
    const clips = [
        { id: "c01", musicId: "m01", name: "情绪渐起", start: 32.5, end: 78.0, tags: [], tagIds: [4], createdAt: now - DAY, updatedAt: now - DAY },
        { id: "c02", musicId: "m04", name: "鼓声入场", start: 12.0, end: 44.5, tags: [], tagIds: [1], createdAt: now - 3 * DAY, updatedAt: now - 3 * DAY }
    ];
    const categories = [
        { id: 1, name: "情绪" }, { id: 2, name: "用途" }, { id: 3, name: "节奏" }, { id: 4, name: "未分类" }
    ];
    const tags = [
        { id: 1, name: "庄重", categoryId: 1, category: "情绪", parentId: null, path: "庄重", musicCount: 2 },
        { id: 2, name: "悬疑", categoryId: 1, category: "情绪", parentId: null, path: "悬疑", musicCount: 2 },
        { id: 3, name: "悲伤", categoryId: 1, category: "情绪", parentId: null, path: "悲伤", musicCount: 1 },
        { id: 4, name: "治愈", categoryId: 1, category: "情绪", parentId: null, path: "治愈", musicCount: 3 },
        { id: 5, name: "剧情", categoryId: 2, category: "用途", parentId: null, path: "剧情", musicCount: 2 },
        { id: 6, name: "片头", categoryId: 2, category: "用途", parentId: null, path: "片头", musicCount: 1 },
        { id: 8, name: "快节奏", categoryId: 3, category: "节奏", parentId: null, path: "快节奏", musicCount: 1 }
    ];
    const candidates = [{ targetId: "m03", kind: "music" }];
    const albumTags = new Map([["城市速写", [4, 5]], ["Midnight Tapes", [2]]]);

    const svgCover = (seed, ch) => "data:image/svg+xml;utf8," + encodeURIComponent(
        `<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="hsl(${seed},52%,70%)"/><stop offset="1" stop-color="hsl(${(seed + 45) % 360},48%,40%)"/></linearGradient></defs><rect width="256" height="256" fill="url(#g)"/><text x="128" y="152" font-size="96" text-anchor="middle" fill="rgba(255,255,255,.88)" font-family="serif">${ch}</text></svg>`);

    async function invoke(cmd, args = {}) {
        switch (cmd) {
            case "init_db": return null;
            case "get_database_path": return "/Users/qi/Library/Application Support/com.musicmanager.app/library.db";
            case "sync_library_locations": return { updatedPaths: 0, updatedAlbums: 0 };
            case "list_music": return music;
            case "list_clips": return clips;
            case "list_clips_for_music": return clips.filter(c => c.musicId === args.musicId);
            case "get_music": return music.find(m => m.id === args.id) || null;
            case "get_clip": return clips.find(c => c.id === args.id) || null;
            case "get_library_counts": return { music: music.length, clips: clips.length };
            case "get_music_cover_arts":
                /* m01/m03/m04/m07 有封面，其余没有（测两种标签） */
                return (args.ids || [])
                    .filter(id => ["m01", "m03", "m04", "m07"].includes(id))
                    .map(id => ({ id, coverArt: svgCover((parseInt(id.slice(1), 10) * 47) % 360, music.find(m => m.id === id)?.name?.slice(0, 1) || "♪") }));
            case "list_tags": return tags;
            case "list_tag_category_records": return categories;
            case "list_tag_categories": return categories.map(c => c.name);
            case "list_candidates": return candidates;
            case "list_album_tags":
                return [...albumTags.entries()].map(([album, tagIds]) => ({ album, tagIds }));
            case "set_album_tags": {
                const ids = [...(args.tagIds || [])];
                (args.newTags || []).forEach(name => {
                    const id = Math.max(0, ...tags.map(t => t.id)) + 1;
                    tags.push({ id, name, categoryId: 4, category: "", parentId: null, path: name, musicCount: 0 });
                    ids.push(id);
                });
                albumTags.set(args.album, ids);
                return ids;
            }
            case "delete_album_tags":
                albumTags.delete(args.album);
                return null;
            case "file_exists": return true;
            case "find_existing_music_paths": return [];
            case "probe_file": return { exists: false };
            case "scan_import_sources": return [];
            case "get_audio_base_url": return null;
            default: return null;
        }
    }

    window.__TAURI__ = {
        core: { invoke, convertFileSrc: p => p },
        dialog: { ask: async () => false, message: async () => {}, open: async () => null },
        opener: { revealItemInDir: async () => {} },
        window: { getCurrentWindow: () => ({ onDragDropEvent: async () => () => {}, listen: async () => () => {} }) }
    };

    const params = new URLSearchParams(location.hash.slice(1));

    if (params.get("nothree")) {
        Object.defineProperty(window, "THREE", { get: () => undefined, set: () => {}, configurable: true });
    }

    /* 只关唱机、保留 THREE：单独调试唱片墙用 */
    if (params.get("nott")) {
        Object.defineProperty(window, "Turntable", { get: () => undefined, set: () => {}, configurable: true });
    }

    const MAX_RAF_FRAMES = Number(params.get("frames") || 60);
    let rafCount = 0;
    const nativeRaf = window.requestAnimationFrame.bind(window);
    window.requestAnimationFrame = cb => (++rafCount > MAX_RAF_FRAMES ? 0 : nativeRaf(cb));

    function whenContentReady(fn) {
        const timer = setInterval(() => {
            if (document.querySelector(".home-hero, .music-list, .music-grid, .empty-state")) {
                clearInterval(timer);
                fn();
            }
        }, 30);
    }

    window.addEventListener("load", () => {
        whenContentReady(() => {
            const theme = params.get("theme");
            if (theme && typeof applyTheme === "function") applyTheme(theme);
            const view = params.get("view");
            if (view && view !== "home") {
                if (params.get("dive") && typeof goToViewFromHome === "function") goToViewFromHome(view);
                else document.querySelector(`.nav-item[data-view="${view}"]`)?.click();
            }
            if (params.get("crate")) {
                setTimeout(() => document.querySelector('[data-home-action="crate"]')?.click(), 300);
            }
        });
    });
})();
