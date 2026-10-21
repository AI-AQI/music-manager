#!/bin/bash
# Compatibility entry point. All replacement and recovery is owned by the GUI installer.
set -euo pipefail
usage() {
  echo '用法：bash install-macos.sh /path/music-manager_VERSION_ARCH.dmg [--replace]'
  echo '打开图形化安装与更新窗口；--replace 为兼容旧命令保留，不会直接删除旧版。'
}
[[ $# -ge 1 && $# -le 2 ]] || { usage; exit 64; }
[[ "$1" != '--help' && "$1" != '-h' ]] || { usage; exit 0; }
[[ $# == 1 || "$2" == '--replace' ]] || { usage; exit 64; }
[[ "$(uname -s)" == 'Darwin' ]] || { echo '此安装入口仅适用于 macOS。' >&2; exit 1; }
[[ -f "$1" && "$1" == *.dmg ]] || { echo '找不到所选 DMG，请重新下载后再试。' >&2; exit 1; }
mount_point="$(mktemp -d "${TMPDIR:-/tmp}/music-manager-mount.XXXXXX")"
mounted=false
cleanup() {
  if [[ "$mounted" == true ]]; then hdiutil detach "$mount_point" -quiet 2>/dev/null || true; fi
  rmdir "$mount_point" 2>/dev/null || true
}
trap cleanup EXIT
hdiutil verify "$1" >/dev/null || { echo 'DMG 校验失败，请重新下载。' >&2; exit 1; }
hdiutil attach -readonly -nobrowse -mountpoint "$mount_point" "$1" >/dev/null
mounted=true
installer="$mount_point/安装与更新声场档案.app"
if [[ ! -d "$installer" ]]; then
  echo '这个旧安装包没有图形化更新入口。请下载含“安装与更新声场档案”的新安装包。当前应用和音乐库未被替换。' >&2
  exit 1
fi
codesign --verify --deep --strict "$installer" || { echo '安装程序校验失败，请重新下载。' >&2; exit 1; }
echo '正在打开“安装与更新声场档案”；请在窗口内继续操作。'
# Wait for the installer to exit before detaching its embedded application payload.
open -n -W "$installer"
