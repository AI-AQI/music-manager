#!/bin/bash
# Installs a downloaded macOS DMG for local testing.
# This script does not make an unsigned application trusted by macOS.

set -euo pipefail

readonly app_name='声场档案.app'
readonly default_destination="/Applications/${app_name}"

usage() {
  cat <<'EOF'
用法：bash scripts/install-macos.sh /完整路径/声场档案_xxx_(aarch64|x64).dmg [--replace]

将 DMG 中的「声场档案」复制到 /Applications，并仅移除该应用的
com.apple.quarantine 下载隔离标记。脚本支持 Apple Silicon 和 Intel Mac，
并会检查所选 DMG 的架构。首次会要求输入 macOS 管理员密码。

--replace  若 Applications 中已有「声场档案」，先将其移到废纸篓再安装。
EOF
}

fail() {
  echo "错误：$*" >&2
  exit 1
}

[[ $# -ge 1 && $# -le 2 ]] || { usage; exit 64; }
[[ "$1" != '--help' && "$1" != '-h' ]] || { usage; exit 0; }

dmg_path="$1"
replace_existing=false
if [[ $# -eq 2 ]]; then
  [[ "$2" == '--replace' ]] || { usage; exit 64; }
  replace_existing=true
fi

[[ -f "$dmg_path" ]] || fail "找不到 DMG：$dmg_path"
[[ "$dmg_path" == *.dmg ]] || fail "请选择 .dmg 文件"
[[ "$(uname -s)" == 'Darwin' ]] || fail '此脚本只能在 macOS 上运行'

machine_arch="$(uname -m)"
case "$machine_arch" in
  arm64)
    [[ "$dmg_path" == *_aarch64.dmg || "$dmg_path" == *_universal.dmg ]] \
      || fail 'Apple Silicon Mac 请使用文件名含 aarch64 或 universal 的 DMG'
    ;;
  x86_64)
    [[ "$dmg_path" == *_x64.dmg || "$dmg_path" == *_universal.dmg ]] \
      || fail 'Intel Mac 请使用文件名含 x64 或 universal 的 DMG'
    ;;
  *)
    fail "不支持的 Mac 架构：$machine_arch"
    ;;
esac

# 浏览器下载的 DMG 带有隔离标记；先对输入文件移除它，以免复制时继承。
xattr -d com.apple.quarantine "$dmg_path" 2>/dev/null || true

mount_point=''
cleanup() {
  if [[ -n "$mount_point" && -d "$mount_point" ]]; then
    hdiutil detach "$mount_point" -quiet 2>/dev/null || true
  fi
}
trap cleanup EXIT

echo '正在检查并挂载 DMG…'
hdiutil verify "$dmg_path" >/dev/null || fail 'DMG 校验失败，请重新下载'
mount_point="$(hdiutil attach -readonly -nobrowse "$dmg_path" | awk '/\/Volumes\// {print $3; exit}')"
[[ -n "$mount_point" && -d "$mount_point" ]] || fail '无法挂载 DMG'

source_app="${mount_point}/${app_name}"
[[ -d "$source_app" ]] || fail "DMG 中未找到 ${app_name}"

if [[ -e "$default_destination" ]]; then
  if [[ "$replace_existing" != true ]]; then
    fail "${default_destination} 已存在；确认替换后重新运行并加 --replace"
  fi
  echo '正在将旧版本移到废纸篓…'
  osascript -e 'tell application "Finder" to delete POSIX file "/Applications/声场档案.app"' \
    || fail '无法移除旧版本，请手动移到废纸篓后重试'
fi

echo '正在复制到 Applications（可能需要管理员密码）…'
sudo ditto "$source_app" "$default_destination"
sudo xattr -dr com.apple.quarantine "$default_destination"

echo '安装完成，正在启动「声场档案」。'
open "$default_destination"
