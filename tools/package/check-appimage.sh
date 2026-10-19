#!/bin/bash
# Runs both GUIs of the AppImage on clean Ubuntu containers (release R1):
# the Rust GUI and `--qt` must open their control socket under Xvfb and list
# the decoders with the bundled Python. The containers get the desktop
# libraries an AppImage takes from the host (AppImage's excludelist: GL, X11,
# xcb, fontconfig, freetype, harfbuzz, libusb, xkbcommon).
#
# usage: tools/package/check-appimage.sh APPIMAGE [IMAGE...]  (default ubuntu:24.04)
set -euo pipefail
APPIMAGE="$(realpath "$1")"
shift
IMAGES=("$@")
[ ${#IMAGES[@]} -gt 0 ] || IMAGES=(ubuntu:24.04)
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
(cd "$WORK" && "$APPIMAGE" --appimage-extract >/dev/null)

for image in "${IMAGES[@]}"; do
    echo "=== $image"
    docker run --rm -v "$WORK/squashfs-root:/app:ro" "$image" bash -euc '
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq xvfb xauth socat libgl1-mesa-dri \
            libegl1 libgl1 libglx0 libopengl0 libice6 libsm6 libx11-6 libx11-xcb1 libxcb1 \
            libxcb-cursor0 libxcb-icccm4 libxcb-image0 libxcb-keysyms1 libxcb-randr0 \
            libxcb-render-util0 libxcb-shape0 libxcb-xinerama0 libxcb-xkb1 \
            libxkbcommon0 libxkbcommon-x11-0 libxcursor1 libxi6 libexpat1 libfontconfig1 \
            libfreetype6 libharfbuzz0b libdbus-1-3 libusb-1.0-0 >/dev/null
        cd /tmp
        check() {   # NAME ARGS...: the socket answers and lists the decoders
            local name="$1"
            shift
            export DSVIEW_MCP_SOCKET="/tmp/$name.sock"
            xvfb-run -a /app/AppRun "$@" --mcp >"$name.log" 2>&1 &
            for _ in $(seq 120); do [ -S "$DSVIEW_MCP_SOCKET" ] && break; sleep 0.5; done
            local n
            n="$(echo "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"list_decoders\",\"params\":{}}" |
                socat -t 20 - "UNIX-CONNECT:$DSVIEW_MCP_SOCKET" 2>/dev/null | grep -o "\"id\":" | wc -l)"
            echo "$name: $n decoders"
            if [ "$n" -lt 100 ]; then
                tail -n 20 "$name.log"
                exit 1
            fi
        }
        check DSView
        check DSView-qt --qt
        echo "AppImage OK"'
done
