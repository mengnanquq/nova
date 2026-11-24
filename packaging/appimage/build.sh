#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Nova contributors
#
# 打一个自包含的 AppImage：dist/Nova-x86_64.AppImage
#
# 用法：
#   packaging/appimage/build.sh              # 编译 + 打包
#   NOVA_SKIP_BUILD=1 packaging/appimage/build.sh   # 复用已有 target/release/nova-desktop
#   NOVA_OUT_NAME=Foo-x86_64.AppImage ...           # 覆盖产物名（默认 Nova-x86_64.AppImage）
#
# 中间产物落在 .dev/appimage/（AppDir）与 .dev/cache/（appimagetool），都不进版本库。

set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd -- "$SCRIPT_DIR/../.." && pwd)

APP_NAME="Nova"
BIN_NAME="nova-desktop"
ARCH="${ARCH:-x86_64}"
OUT_NAME="${NOVA_OUT_NAME:-${APP_NAME}-${ARCH}.AppImage}"

WORK_DIR="$ROOT/.dev/appimage"
APPDIR="$WORK_DIR/AppDir"
CACHE_DIR="$ROOT/.dev/cache"
DIST_DIR="$ROOT/dist"
BIN="$ROOT/target/release/$BIN_NAME"
APPIMAGETOOL_APPIMAGE="$CACHE_DIR/appimagetool-$ARCH.AppImage"
APPIMAGETOOL_DIR="$CACHE_DIR/appimagetool-$ARCH"
APPIMAGETOOL="$APPIMAGETOOL_DIR/squashfs-root/AppRun"

log() { printf '==> %s\n' "$*" >&2; }
warn() { printf '!!  %s\n' "$*" >&2; }
die() { printf 'xx  %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# “必须由宿主提供”的库：显卡驱动 / 显示服务器 / 字体栈 / 音频服务 / libc ABI。
# 捆绑它们会在别的发行版上撞上驱动与 ABI 不匹配，症状从“黑屏”到“段错误”都有。
# 这里用 ERE 匹配库文件名（basename）。
# ---------------------------------------------------------------------------
HOST_LIBS_ERE='
^ld-linux|
^libc\.so|
^libm\.so|
^libpthread|
^libdl\.so|
^librt\.so|
^libgcc_s|
^libstdc\+\+|
^libGL|
^libEGL|
^libGLX|
^libGLdispatch|
^libOpenGL\.so|
^libOpenCL|
^libdrm|
^libgbm|
^libvulkan|
^libX11|
^libxcb|
^libwayland-|
^libxkbcommon|
^libfontconfig|
^libfreetype|
^libdbus-1|
^libsystemd|
^libudev|
^libasound|
^libpulse|
^libpipewire|
'

# 去掉空行与行尾的 `|`（否则模式里含空分支，会匹配所有库）
HOST_LIBS_RE=()
while IFS= read -r _pat; do
  _pat="${_pat%|}"
  [[ -n "$_pat" ]] && HOST_LIBS_RE+=("$_pat")
done <<<"$HOST_LIBS_ERE"
unset _pat

is_host_lib() {
  local base="$1" pat
  for pat in "${HOST_LIBS_RE[@]}"; do
    [[ "$base" =~ $pat ]] && return 0
  done
  return 1
}

# ---------------------------------------------------------------------------
# 1) 编译
# ---------------------------------------------------------------------------
if [[ "${NOVA_SKIP_BUILD:-0}" == "1" ]]; then
  log "跳过编译（NOVA_SKIP_BUILD=1）"
else
  log "编译 release 二进制：cargo build --release --locked -p nova-app"
  (cd "$ROOT" && cargo build --release --locked -p nova-app)
fi
[[ -x "$BIN" ]] || die "找不到可执行文件 $BIN（先跑 cargo build --release -p nova-app）"

# ---------------------------------------------------------------------------
# 2) 清空并组装 AppDir（幂等：每次都从零开始）
# ---------------------------------------------------------------------------
log "重建 AppDir：$APPDIR"
rm -rf "$APPDIR"
mkdir -p \
  "$APPDIR/usr/bin" \
  "$APPDIR/usr/lib" \
  "$APPDIR/usr/share/applications" \
  "$APPDIR/usr/share/icons/hicolor/scalable/apps" \
  "$APPDIR/usr/share/icons/hicolor/256x256/apps"

install -m 0755 "$BIN" "$APPDIR/usr/bin/$BIN_NAME"
install -m 0644 "$SCRIPT_DIR/nova.desktop" "$APPDIR/usr/share/applications/nova.desktop"
install -m 0644 "$SCRIPT_DIR/nova.desktop" "$APPDIR/nova.desktop"
install -m 0644 "$SCRIPT_DIR/nova.svg" "$APPDIR/usr/share/icons/hicolor/scalable/apps/nova.svg"
install -m 0644 "$SCRIPT_DIR/nova.png" "$APPDIR/usr/share/icons/hicolor/256x256/apps/nova.png"
# AppImage 规范：根目录的图标（进程/桌面集成用）+ .DirIcon
install -m 0644 "$SCRIPT_DIR/nova.svg" "$APPDIR/nova.svg"
install -m 0644 "$SCRIPT_DIR/nova.png" "$APPDIR/nova.png"
install -m 0644 "$SCRIPT_DIR/nova.png" "$APPDIR/.DirIcon"

# ---------------------------------------------------------------------------
# 3) 收集并捆绑依赖库（白名单之外的全部捆绑）
# ---------------------------------------------------------------------------
log "收集二进制依赖（ldd）"
mapfile -t LDD_LINES < <(ldd "$BIN" 2>/dev/null)

declare -a CANDIDATES=()
for line in "${LDD_LINES[@]}"; do
  # 形如：libmpv.so.2 => /usr/lib/libmpv.so.2 (0x00007f...)
  # 或：/lib64/ld-linux-x86-64.so.2 (0x00007f...)
  if [[ "$line" =~ ^[[:space:]]*([^[:space:]]+)[[:space:]]+=\>[[:space:]]+(/[^[:space:]]+) ]]; then
    CANDIDATES+=("${BASH_REMATCH[2]}")
  elif [[ "$line" =~ ^[[:space:]]*(/[^[:space:]]+) ]]; then
    CANDIDATES+=("${BASH_REMATCH[1]}")
  fi
done
[[ ${#CANDIDATES[@]} -gt 0 ]] || die "ldd 没有解析出任何依赖；二进制是否动态链接？"

BUNDLED=()
SKIPPED=()
declare -A SEEN=()
for path in "${CANDIDATES[@]}"; do
  base=$(basename "$path")
  # 有些条目不是真实文件（"not found"），直接跳过
  [[ -e "$path" ]] || continue
  [[ -n "${SEEN[$base]:-}" ]] && continue
  SEEN[$base]=1
  if is_host_lib "$base"; then
    SKIPPED+=("$base")
    continue
  fi
  # 只认系统库目录：/opt（Intel oneAPI OpenCL/SVML）、$HOME 等 LD_LIBRARY_PATH 泄漏进来的
  # 私有运行时一律不捆，否则会把某台机器的编译器运行时塞进包里。
  case "$path" in
    /usr/lib/*|/usr/lib64/*|/lib/*|/lib64/*) ;;
    *)
      warn "跳过非系统路径依赖：$path"
      SKIPPED+=("$base(非系统路径)")
      continue
      ;;
  esac
  # 解引用符号链接：AppDir 里落地成实体文件，文件名保留 soname
  cp -L --preserve=mode,timestamps "$path" "$APPDIR/usr/lib/$base"
  BUNDLED+=("$base")
done

log "捆绑 ${#BUNDLED[@]} 个库；按白名单跳过 ${#SKIPPED[@]} 个宿主库"
if [[ ${#BUNDLED[@]} -gt 0 ]]; then
  printf '    捆绑：%s\n' "$(printf '%s\n' "${BUNDLED[@]}" | sort | tr '\n' ' ')" >&2
fi

# 自查：AppDir 里不能出现白名单库（防止 ldd 之外的拷贝走漏）
LEAKED=()
for f in "$APPDIR/usr/lib"/*; do
  [[ -e "$f" ]] || continue
  base=$(basename "$f")
  is_host_lib "$base" && LEAKED+=("$base")
done
if [[ ${#LEAKED[@]} -gt 0 ]]; then
  die "AppDir 里混入了必须由宿主提供的库：${LEAKED[*]}"
fi

# ---------------------------------------------------------------------------
# 4) AppRun
# ---------------------------------------------------------------------------
cat >"$APPDIR/AppRun" <<'APPRUN'
#!/bin/sh
# AppImage 入口：优先用打包内的库，其余回落到宿主。
set -e
HERE=$(dirname "$(readlink -f "$0")")
APPDIR=${APPDIR:-$HERE}
export LD_LIBRARY_PATH="$APPDIR/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export APPDIR
exec "$APPDIR/usr/bin/nova-desktop" "$@"
APPRUN
chmod 0755 "$APPDIR/AppRun"

# ---------------------------------------------------------------------------
# 5) appimagetool（下载 + 解包，避免运行期依赖 FUSE）
# ---------------------------------------------------------------------------
if [[ ! -x "$APPIMAGETOOL" ]]; then
  mkdir -p "$CACHE_DIR"
  if [[ ! -f "$APPIMAGETOOL_APPIMAGE" ]]; then
    log "下载 appimagetool（GitHub releases/continuous）"
    curl -fL --retry 3 --connect-timeout 20 \
      -o "$APPIMAGETOOL_APPIMAGE.part" \
      "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage"
    mv "$APPIMAGETOOL_APPIMAGE.part" "$APPIMAGETOOL_APPIMAGE"
    chmod 0755 "$APPIMAGETOOL_APPIMAGE"
  fi
  log "解包 appimagetool 到 $APPIMAGETOOL_DIR"
  rm -rf "$APPIMAGETOOL_DIR"
  mkdir -p "$APPIMAGETOOL_DIR"
  ( cd "$APPIMAGETOOL_DIR" && "$APPIMAGETOOL_APPIMAGE" --appimage-extract >/dev/null )
  [[ -x "$APPIMAGETOOL" ]] || die "appimagetool 解包失败：$APPIMAGETOOL 不存在"
fi

# ---------------------------------------------------------------------------
# 6) 产出 AppImage
# ---------------------------------------------------------------------------
mkdir -p "$DIST_DIR"
rm -f "$DIST_DIR/$OUT_NAME"
log "打包：$DIST_DIR/$OUT_NAME"

# appimagetool 在 AppDir 里跑 strip；--no-appstream 跳过需要 appstreamcli 的元数据校验。
# APPIMAGE_EXTRACT_AND_RUN=1 让它内部调用的工具也不需要 FUSE。
(
  cd "$WORK_DIR"
  ARCH="$ARCH" APPIMAGE_EXTRACT_AND_RUN=1 \
    "$APPIMAGETOOL" --no-appstream --comp zstd "$APPDIR" "$DIST_DIR/$OUT_NAME"
)

chmod 0755 "$DIST_DIR/$OUT_NAME"
log "完成：$DIST_DIR/$OUT_NAME ($(du -h "$DIST_DIR/$OUT_NAME" | cut -f1))"
