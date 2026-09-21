#!/bin/bash
# 预览截图：frontend/.preview/shot.sh "<hash 参数>" <输出.png> [虚拟时间 ms] [宽x高]
set -e
cd "$(dirname "$0")/.."

# 由 index.html 生成 preview.html（注入 mock）
sed 's|<script src="vendor/three.min.js">|<script src=".preview/mock-tauri.js"></script>\n    <script src="vendor/three.min.js">|' index.html > preview.html

PORT=8931
if ! curl -s "http://127.0.0.1:$PORT/preview.html" >/dev/null 2>&1; then
    (python3 -m http.server $PORT >/dev/null 2>&1 &) 
    sleep 1
fi

HASH="${1:-}"
OUT="${2:-/tmp/mm-shot.png}"
BUDGET="${3:-9000}"
SIZE="${4:-1440,900}"

"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
    --headless=new --disable-gpu --hide-scrollbars \
    --window-size="$SIZE" \
    --virtual-time-budget="$BUDGET" \
    --screenshot="$OUT" \
    "http://127.0.0.1:$PORT/preview.html${HASH}" 2>/dev/null

echo "$OUT"
