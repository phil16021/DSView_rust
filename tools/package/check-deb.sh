#!/bin/bash
# Installs the .deb in DIR on a clean Ubuntu container and checks that
# DSViewR and dsviewr-cli start with the bundled Python:
# - dsviewr-cli lists the decoders, captures the demo and decodes it;
# - DSViewR runs for 20 s under Xvfb.
#
# usage: tools/package/check-deb.sh DIR [IMAGE...]   (default ubuntu:24.04)
set -euo pipefail
DIR="$(realpath "$1")"
shift
IMAGES=("$@")
[ ${#IMAGES[@]} -gt 0 ] || IMAGES=(ubuntu:24.04)

for image in "${IMAGES[@]}"; do
    echo "=== $image"
    docker run --rm -v "$DIR:/deb:ro" "$image" bash -euc '
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq /deb/*.deb xvfb xauth libgl1-mesa-dri >/dev/null
        cd /tmp
        dsviewr-cli decoders 2>/dev/null | grep -qw uart
        dsviewr-cli capture --device "demo logic" --samples 100000 --out demo.dsl 2>/dev/null
        n="$(dsviewr-cli decode demo.dsl --decoder i2c:scl=0,sda=1 2>/dev/null | wc -l)"
        echo "dsviewr-cli: $n I2C annotations"
        [ "$n" -gt 0 ]
        run() {   # NAME COMMAND...: must still run after 20 s
            local name="$1" rc=0
            shift
            timeout 20 "$@" >"$name.log" 2>&1 || rc=$?
            if [ "$rc" -ne 124 ]; then
                echo "$name exited with $rc:"
                tail -n 20 "$name.log"
                exit 1
            fi
            if grep -qi "python start-up\|Fatal Python\|No module named" "$name.log"; then
                echo "$name: Python error:"
                grep -i "python\|module" "$name.log" | head -n 5
                exit 1
            fi
            echo "$name: running after 20 s"
        }
        run DSViewR xvfb-run -a DSViewR
        echo "deb OK"'
done
