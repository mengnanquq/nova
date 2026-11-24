#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# 起一个真实的 Jellyfin 用于阶段 2 的集成测试（不是 mock）。
#
#   scripts/jellyfin-dev.sh up      启动、完成初始化、建 Movies 库并扫描，打印 NOVA_JELLYFIN_* 环境变量
#   scripts/jellyfin-dev.sh env     只打印环境变量（容器需已在跑）
#   scripts/jellyfin-dev.sh down    停止并保留数据
#   scripts/jellyfin-dev.sh reset   停止并删除数据（下次 up 会重新初始化）
#
# 数据目录：.dev/jellyfin（已 gitignore）。端口默认 18096，避开用户自己的 8096。
# 容器以当前 uid/gid 运行，避免 .dev 下出现 root 所有、删不掉的配置文件。

set -euo pipefail

NAME=nova-jellyfin
PORT="${NOVA_JELLYFIN_PORT:-18096}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA="$ROOT/.dev/jellyfin"
MEDIA="$ROOT/target/testdata"
USER_NAME="nova"
PASSWORD="nova-dev-pass"
LIBRARY="Movies"
BASE="http://127.0.0.1:$PORT"
IMAGE="jellyfin/jellyfin:latest"

auth_header() {
  echo "Authorization: MediaBrowser Client=\"NovaDev\", Device=\"ci\", DeviceId=\"nova-dev\", Version=\"0.1.0\""
}

wait_ready() {
  # 不要用日志判断：`docker start` 之后旧日志里就有 "Startup complete"，
  # 而服务器稳定运行一段时间后那条日志又会滚出尾部，两头都会骗人。
  # 可靠信号是 HTTP：启动中 `/Users` 会返回 503（Kestrel 还没接管），就绪后是 401/200。
  local public users
  for _ in $(seq 1 180); do
    public=$(curl -fsS --max-time 2 "$BASE/System/Info/Public" 2>/dev/null || true)
    if python3 -c 'import sys,json; sys.exit(0 if json.load(sys.stdin).get("Version") else 1)' <<<"$public" 2>/dev/null; then
      users=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 2 "$BASE/Users" 2>/dev/null || echo 000)
      case "$users" in
        503|000|"") : ;;
        *) return 0 ;;
      esac
    fi
    sleep 1
  done
  echo "Jellyfin 180s 内没有就绪，看 docker logs $NAME" >&2
  return 1
}

# GET /Startup/User 会触发 _userManager.InitializeAsync() 创建首个用户；
# POST /Startup/User 只负责给它改名和设密码（用户不存在时返回 404）。
# 顺序错了就会卡在 "404 Not Found" —— 这是 Jellyfin 12 的行为。
bootstrap() {
  local completed
  completed=$(curl -fsS "$BASE/System/Info/Public" |
    python3 -c 'import sys,json;print(json.load(sys.stdin).get("StartupWizardCompleted",False))')

  if [ "$completed" != "True" ]; then
    curl -fsS "$BASE/Startup/User" -H "$(auth_header)" >/dev/null
    curl -fsS -X POST "$BASE/Startup/User" \
      -H "$(auth_header)" -H 'Content-Type: application/json' \
      -d "{\"Name\":\"$USER_NAME\",\"Password\":\"$PASSWORD\"}" >/dev/null
    curl -fsS -X POST "$BASE/Startup/Complete" -H "$(auth_header)" >/dev/null
  fi

  token=$(authenticate) || {
    echo "认证失败：用户 $USER_NAME 不存在或密码不对。试 scripts/jellyfin-dev.sh reset" >&2
    return 1
  }

  # 建 Movies 库指向已生成的测试视频；已存在时接口报错，忽略即可。
  curl -fsS -X POST \
    "$BASE/Library/VirtualFolders?name=$LIBRARY&collectionType=movies&refreshLibrary=true&paths=$MEDIA" \
    -H "Authorization: MediaBrowser Token=\"$token\"" >/dev/null 2>&1 || true

  # 等扫描出条目（最多 60s）。
  for _ in $(seq 1 60); do
    local count
    count=$(curl -fsS "$BASE/Items?Recursive=true&IncludeItemTypes=Movie&Limit=1" \
      -H "Authorization: MediaBrowser Token=\"$token\"" |
      python3 -c 'import sys,json;print(json.load(sys.stdin).get("TotalRecordCount",0))' 2>/dev/null || echo 0)
    [ "$count" -gt 0 ] && break
    curl -fsS -X POST "$BASE/Library/Refresh" -H "Authorization: MediaBrowser Token=\"$token\"" >/dev/null 2>&1 || true
    sleep 1
  done
}

authenticate() {
  curl -fsS -X POST "$BASE/Users/AuthenticateByName" \
    -H "$(auth_header)" -H 'Content-Type: application/json' \
    -d "{\"Username\":\"$USER_NAME\",\"Pw\":\"$PASSWORD\"}" |
    python3 -c 'import sys,json;print(json.load(sys.stdin)["AccessToken"])'
}

print_env() {
  cat <<EOF
export NOVA_JELLYFIN_URL=$BASE
export NOVA_JELLYFIN_USER=$USER_NAME
export NOVA_JELLYFIN_PASS=$PASSWORD
export NOVA_JELLYFIN_LIBRARY=$LIBRARY
EOF
}

# 容器以 root 启动过一次就会在 .dev 下留下宿主删不掉的 root 文件，
# 所以清理借同镜像跑一次 root 的 rm。
wipe_data() {
  rm -rf "$DATA" 2>/dev/null || true
  if [ -d "$DATA" ]; then
    docker run --rm -v "$ROOT/.dev:/d" --entrypoint sh "$IMAGE" -c 'rm -rf /d/jellyfin'
  fi
}

case "${1:-up}" in
  up)
    command -v docker >/dev/null || { echo "需要 docker" >&2; exit 69; }
    if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
      echo "拉取 $IMAGE ..." >&2
      docker pull "$IMAGE" >/dev/null
    fi
    mkdir -p "$DATA/config" "$DATA/cache" "$MEDIA"
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    docker run -d --name "$NAME" \
      --user "$(id -u):$(id -g)" \
      -p "127.0.0.1:$PORT:8096" \
      -v "$DATA/config:/config" \
      -v "$DATA/cache:/cache" \
      -v "$MEDIA:/media:ro" \
      -e JELLYFIN_PublishedServerUrl="$BASE" \
      "$IMAGE" >/dev/null
    wait_ready
    bootstrap
    print_env
    ;;
  env)
    wait_ready
    print_env
    ;;
  down)
    docker stop "$NAME" >/dev/null 2>&1 || true
    echo "已停止（数据保留在 $DATA）"
    ;;
  reset)
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    wipe_data
    echo "已删除容器与 $DATA"
    ;;
  *)
    echo "用法: $0 {up|env|down|reset}" >&2
    exit 64
    ;;
esac
