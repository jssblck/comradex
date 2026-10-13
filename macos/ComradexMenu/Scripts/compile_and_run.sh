#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
APP_NAME=ComradexMenu
APP="$ROOT/${APP_NAME}.app"

# Match this checkout's build only, so an installed copy keeps running. WidgetKit keeps
# extension processes alive, and a stale one would keep serving the old widgets.
pkill -f "$APP/Contents/MacOS/${APP_NAME}" 2>/dev/null || true
pkill -f "$APP/Contents/PlugIns/ComradexWidgets.appex/Contents/MacOS/ComradexWidgets" 2>/dev/null || true
if [[ "${1:-}" == "--test" ]]; then
  (cd "$ROOT" && swift test)
fi
# Ad-hoc unless APP_IDENTITY is set; the widget needs a team signature for its data.
# macOS caches widget descriptions per bundle version, so every development build gets a
# new build number; otherwise the gallery keeps showing the previous build's widgets.
BUILD_NUMBER=${BUILD_NUMBER:-$(date +%s)} MENU_BAR_APP=1 "$ROOT/Scripts/package_app.sh" release
open "$APP"

for _ in {1..10}; do
  if pgrep -f "$APP/Contents/MacOS/${APP_NAME}" >/dev/null 2>&1; then
    echo "OK: $APP_NAME is running."
    exit 0
  fi
  sleep 0.4
done
echo "ERROR: $APP_NAME exited immediately." >&2
exit 1
