#!/bin/sh
set -eu

if [ "${COMFYUI_REQUIRE_GPU:-0}" = 1 ]; then
    python -c "import sys, torch; sys.exit(0 if torch.cuda.is_available() else 'COMFYUI_REQUIRE_GPU=1 but torch sees no GPU; refusing to fall back to the CPU')"
fi

exec "$@"
