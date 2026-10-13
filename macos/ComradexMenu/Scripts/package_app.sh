#!/usr/bin/env bash
set -euo pipefail

CONF=${1:-release}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

APP_NAME=ComradexMenu
DISPLAY_NAME="Comradex Menu"
BUNDLE_ID=${BUNDLE_ID:-com.nicosuave.comradex.menu}
WIDGET_NAME=ComradexWidgets
WIDGET_BUNDLE_ID="${BUNDLE_ID}.widgets"
URL_SCHEME=${URL_SCHEME:-comradex-menu}
MACOS_MIN_VERSION=14.0
MENU_BAR_APP=1
SIGNING_MODE=${SIGNING_MODE:-}
APP_IDENTITY=${APP_IDENTITY:-}
TEAM_ID=${TEAM_ID:-}
source "$ROOT/version.env"

ADHOC=0
if [[ "$SIGNING_MODE" == "adhoc" || -z "$APP_IDENTITY" ]]; then
  ADHOC=1
fi

# The widget reads usage that the menu app publishes to a shared app group
# container. Team-prefixed groups need no provisioning profile outside the App
# Store, but they need the signing team, so ad-hoc builds omit the group.
if [[ $ADHOC -eq 0 && -z "$TEAM_ID" ]]; then
  TEAM_ID=$(security find-certificate -c "$APP_IDENTITY" -p \
    | openssl x509 -noout -subject -nameopt multiline \
    | sed -n 's/^ *organizationalUnitName *= *//p' | head -n 1)
  if [[ -z "$TEAM_ID" ]]; then
    echo "ERROR: Could not determine the team ID for $APP_IDENTITY; set TEAM_ID" >&2
    exit 1
  fi
fi
APP_GROUP=""
if [[ $ADHOC -eq 0 ]]; then
  APP_GROUP="${TEAM_ID}.com.nicosuave.comradex"
fi

ARCH_LIST=( ${ARCHES:-$(uname -m)} )
BINARIES=()
WIDGET_BINARIES=()
for ARCH in "${ARCH_LIST[@]}"; do
  SCRATCH_PATH="$ROOT/.build/architectures/$ARCH"
  swift build -c "$CONF" --arch "$ARCH" --scratch-path "$SCRATCH_PATH"
  BIN_DIR=$(swift build -c "$CONF" --arch "$ARCH" \
    --scratch-path "$SCRATCH_PATH" --show-bin-path)
  for EXECUTABLE in "$APP_NAME" "$WIDGET_NAME"; do
    if [[ ! -f "$BIN_DIR/$EXECUTABLE" ]]; then
      echo "ERROR: Missing $ARCH binary at $BIN_DIR/$EXECUTABLE" >&2
      exit 1
    fi
  done
  BINARIES+=("$BIN_DIR/$APP_NAME")
  WIDGET_BINARIES+=("$BIN_DIR/$WIDGET_NAME")
done

APP="$ROOT/${APP_NAME}.app"
WIDGET="$APP/Contents/PlugIns/${WIDGET_NAME}.appex"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$WIDGET/Contents/MacOS"
cp "$ROOT/Sources/ComradexMenu/Resources/comradex-logo.svg" "$APP/Contents/Resources/"

# Usage: combine_binaries OUTPUT BINARY...
combine_binaries() {
  local output="$1"
  shift
  if [[ $# -gt 1 ]]; then
    lipo -create "$@" -output "$output"
  else
    cp "$1" "$output"
  fi
  chmod +x "$output"
  local actual_arches
  actual_arches=$(lipo -archs "$output")
  for ARCH in "${ARCH_LIST[@]}"; do
    if [[ " $actual_arches " != *" $ARCH "* ]]; then
      echo "ERROR: $output is missing $ARCH (has: $actual_arches)" >&2
      exit 1
    fi
  done
}
combine_binaries "$APP/Contents/MacOS/$APP_NAME" "${BINARIES[@]}"
combine_binaries "$WIDGET/Contents/MacOS/$WIDGET_NAME" "${WIDGET_BINARIES[@]}"

APP_GROUP_PLIST=""
if [[ -n "$APP_GROUP" ]]; then
  APP_GROUP_PLIST="<key>ComradexAppGroup</key><string>${APP_GROUP}</string>"
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>${DISPLAY_NAME}</string>
  <key>CFBundleDisplayName</key><string>${DISPLAY_NAME}</string>
  <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
  <key>CFBundleExecutable</key><string>${APP_NAME}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>NSPrincipalClass</key><string>NSApplication</string>
  <key>CFBundleShortVersionString</key><string>${MARKETING_VERSION}</string>
  <key>CFBundleVersion</key><string>${BUILD_NUMBER}</string>
  <key>LSMinimumSystemVersion</key><string>${MACOS_MIN_VERSION}</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
  <key>CFBundleURLTypes</key><array><dict>
    <key>CFBundleURLName</key><string>${BUNDLE_ID}</string>
    <key>CFBundleURLSchemes</key><array><string>${URL_SCHEME}</string></array>
  </dict></array>
  <key>ComradexURLScheme</key><string>${URL_SCHEME}</string>
  ${APP_GROUP_PLIST}
</dict></plist>
PLIST

cat > "$WIDGET/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>${WIDGET_NAME}</string>
  <key>CFBundleDisplayName</key><string>Comradex</string>
  <key>CFBundleIdentifier</key><string>${WIDGET_BUNDLE_ID}</string>
  <key>CFBundleExecutable</key><string>${WIDGET_NAME}</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundlePackageType</key><string>XPC!</string>
  <key>CFBundleShortVersionString</key><string>${MARKETING_VERSION}</string>
  <key>CFBundleVersion</key><string>${BUILD_NUMBER}</string>
  <key>LSMinimumSystemVersion</key><string>${MACOS_MIN_VERSION}</string>
  <key>NSExtension</key><dict>
    <key>NSExtensionPointIdentifier</key><string>com.apple.widgetkit-extension</string>
  </dict>
  <key>ComradexURLScheme</key><string>${URL_SCHEME}</string>
  ${APP_GROUP_PLIST}
</dict></plist>
PLIST

ENTITLEMENTS_DIR="$ROOT/.build/entitlements"
mkdir -p "$ENTITLEMENTS_DIR"
APP_GROUP_ENTITLEMENT=""
if [[ -n "$APP_GROUP" ]]; then
  APP_GROUP_ENTITLEMENT="<key>com.apple.security.application-groups</key><array><string>${APP_GROUP}</string></array>"
fi
# WidgetKit loads only sandboxed extensions.
cat > "$ENTITLEMENTS_DIR/widget.entitlements" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>com.apple.security.app-sandbox</key><true/>
  ${APP_GROUP_ENTITLEMENT}
</dict></plist>
PLIST
cat > "$ENTITLEMENTS_DIR/app.entitlements" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  ${APP_GROUP_ENTITLEMENT}
</dict></plist>
PLIST

if [[ $ADHOC -eq 1 ]]; then
  CODESIGN_ARGS=(--force --sign -)
else
  CODESIGN_ARGS=(--force --timestamp --options runtime --sign "$APP_IDENTITY")
fi
xattr -cr "$APP"
# Sign inside-out: the extension first, then the app that seals it.
codesign "${CODESIGN_ARGS[@]}" --entitlements "$ENTITLEMENTS_DIR/widget.entitlements" "$WIDGET"
codesign "${CODESIGN_ARGS[@]}" --entitlements "$ENTITLEMENTS_DIR/app.entitlements" "$APP"
echo "Created $APP (MENU_BAR_APP=$MENU_BAR_APP, app group: ${APP_GROUP:-none})"
