#!/usr/bin/env bash
# deliver — construit, signe, installe et archive une livraison de l'app, puis met à jour
# app_sources.conf. Une commande à la place du rituel manuel (build → bundle → sign → install →
# copie de livraison → app_sources.conf).
#
# Usage : tools/deliver.sh [--version 0.21.1] [--no-build] [--no-install]
# Sortie : /Applications/Artcraft/<App>.app, copie dans /Volumes/Madson/ArtCraft/<slug>/, sauvegarde
#          de l'app remplacée dans _old/, et les lignes à coller dans la note de port du vault.
set -uo pipefail
export PATH="/Volumes/Data/rust/cargo/bin:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"
export RUSTUP_HOME="${RUSTUP_HOME:-/Volumes/Data/rust/rustup}"
export CARGO_HOME="${CARGO_HOME:-/Volumes/Data/rust/cargo}"

APP="${APP:-FilmCraft}"
SLUG="${SLUG:-filmcraft}"
BUNDLE_ID="${BUNDLE_ID:-ai.storyteller.filmcraft}"
BIN="${BIN:-filmcraft}"
CARGO_PKG="${CARGO_PKG:-filmcraft}"
FEATURES="${FEATURES:-}"
BUILD_CMD="${BUILD_CMD:-cargo build --release -p filmcraft -p filmcraft-cli}"
APPS_DIR="${APPS_DIR:-/Applications/Artcraft}"
DELIVERY_DIR="${DELIVERY_DIR:-/Volumes/Madson/ArtCraft/$SLUG}"
OLD_DIR="${OLD_DIR:-/Volumes/Madson/ArtCraft/_old}"
SOURCES="${SOURCES:-/Volumes/Madson/ArtCraft/app_sources.conf}"
TEMPLATE="${TEMPLATE:-$APPS_DIR/$APP.app}"
TARGET_DIR="${CARGO_TARGET_DIR:-/Volumes/Data/work/filmcraft-port-017/target}"

VERSION=""; DO_BUILD=1; DO_INSTALL=1
for a in "$@"; do case "$a" in
  --version) shift; VERSION="$1" ;;
  --no-build) DO_BUILD=0 ;;
  --no-install) DO_INSTALL=0 ;;
  -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
esac; done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"; cd "$ROOT"
log() { printf '%s %s\n' "$(date '+%H:%M:%S')" "$*"; }
die() { log "ERREUR: $*"; exit 1; }
[ -d "$TEMPLATE" ] || die "app modèle introuvable: $TEMPLATE (lance --no-install une première fois ou passe TEMPLATE=...)"
[ -n "$VERSION" ] || VERSION=$(awk '/^\[workspace.package\]/{f=1;next} f&&$1=="version"{gsub(/[" ]/,"",$3);print $3;exit}' Cargo.toml)
[ -n "$VERSION" ] || die "version inconnue"
SHA=$(git rev-parse --short=8 HEAD)
LOCAL="${VERSION}-madson1"
export CARGO_TARGET_DIR="$TARGET_DIR"
log "$APP $LOCAL (commit $SHA)"

if [ "$DO_BUILD" = 1 ]; then
  log "build"; CRAFT_FONTS_DIR="${CRAFT_FONTS_DIR:-/Volumes/Data/work/craft-fonts}" $BUILD_CMD >/tmp/deliver-build.log 2>&1 \
    || { tail -20 /tmp/deliver-build.log; die "build"; }
fi
[ -x "$TARGET_DIR/release/$BIN" ] || die "binaire absent: $TARGET_DIR/release/$BIN"

STAGE=$(mktemp -d "${TMPDIR:-/tmp}/deliver-XXXXXX"); trap 'rm -rf "$STAGE"' EXIT
APP_PATH="$STAGE/$APP.app"
log "bundle"; ditto "$TEMPLATE" "$APP_PATH" || die "copie du modèle"
cp "$TARGET_DIR/release/$BIN" "$APP_PATH/Contents/MacOS/$APP" || die "copie du binaire"
# Les extensions livrées avec le fork : le binaire les cherche dans `../Resources/extensions-bundled`.
if [ -d "$ROOT/extensions-bundled" ]; then
  mkdir -p "$APP_PATH/Contents/Resources"
  rm -rf "$APP_PATH/Contents/Resources/extensions-bundled"
  ditto "$ROOT/extensions-bundled" "$APP_PATH/Contents/Resources/extensions-bundled" || die "extensions-bundled"
  log "extensions embarquées: $(ls "$ROOT/extensions-bundled" | wc -l | tr -d ' ')"
fi
PL="$APP_PATH/Contents/Info.plist"
set_plist() { /usr/libexec/PlistBuddy -c "Set :$1 $2" "$PL" 2>/dev/null || /usr/libexec/PlistBuddy -c "Add :$1 string $2" "$PL"; }
set_plist CFBundleVersion "$VERSION"
set_plist CFBundleShortVersionString "$VERSION"
set_plist "${APP}Version" "$VERSION"
set_plist "${APP}LocalBuild" "$LOCAL"
set_plist "${APP}BuildCommit" "$SHA"
[ "$(/usr/libexec/PlistBuddy -c "Print :CFBundleIdentifier" "$PL")" = "$BUNDLE_ID" ] || die "identifiant de bundle inattendu"
xattr -cr "$APP_PATH" 2>/dev/null || true
IDENTITY="${MACOS_SIGN_IDENTITY:--}"
log "signature ($IDENTITY)"; codesign --force --deep --sign "$IDENTITY" "$APP_PATH" >/dev/null 2>&1 || die "codesign"
codesign --verify --strict --deep "$APP_PATH" || die "vérification de signature"

mkdir -p "$DELIVERY_DIR" "$OLD_DIR"
DELIVERED="$DELIVERY_DIR/$APP-${LOCAL}.app"
log "copie de livraison: $DELIVERED"; ditto "$APP_PATH" "$DELIVERED" || die "ditto livraison"

if [ "$DO_INSTALL" = 1 ]; then
  if [ -d "$APPS_DIR/$APP.app" ]; then
    OLDVER=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APPS_DIR/$APP.app/Contents/Info.plist" 2>/dev/null | tr -cd '[:alnum:].-' )
    [ -n "$OLDVER" ] || OLDVER=inconnu
    OLD="$OLD_DIR/$APP-upstream-$OLDVER-avant-madson-$(date +%Y%m%d).app"
    [ -e "$OLD" ] || { log "sauvegarde: $OLD"; ditto "$APPS_DIR/$APP.app" "$OLD"; }
  fi
  rm -rf "$APPS_DIR/$APP.app"
  ditto "$DELIVERED" "$APPS_DIR/$APP.app" || die "installation"
  codesign --verify --strict --deep "$APPS_DIR/$APP.app" || die "signature de l'app installée"
  log "installé: $APPS_DIR/$APP.app"
fi

if [ -f "$SOURCES" ]; then
  python3 - "$SOURCES" "$SLUG" "$APP" "$DELIVERED" "$LOCAL" "$SHA" <<'PY'
import sys, re, pathlib
src, slug, app, delivered, local, sha = sys.argv[1:7]
p = pathlib.Path(src); lines = p.read_text().splitlines()
key_line = f"{app}={delivered}"
note = [key_line,
        f"# {app} {local} livré le {__import__('datetime').date.today().isoformat()} depuis tools/deliver.sh (commit {sha})",
        f"# Sauvegarde de l'upstream précédent : _old/{app}-upstream-*-avant-madson-*.app"]
out, replaced = [], False
for l in lines:
    if re.match(rf"^{app}=", l):
        out.extend(note); replaced = True
    else:
        out.append(l)
if not replaced: out.extend(note)
p.write_text("\n".join(out) + "\n")
print("app_sources.conf mis à jour" if replaced else "app_sources.conf complété")
PY
fi

log "termine"
echo
echo "À coller dans la note de port :"
echo "| Bundle | \`$APP-$LOCAL.app\` — \`$BUNDLE_ID\`, \`$LOCAL\`, commit \`$SHA\`, signature $( [ "$IDENTITY" = "-" ] && echo ad-hoc || echo "$IDENTITY") vérifiée |"
echo "| Installée | \`$APPS_DIR/$APP.app\` |"
echo "| Livrée | \`$DELIVERED\` |"
