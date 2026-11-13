#!/usr/bin/env bash
# port-check — essai de port de la dernière version upstream, en local.
#
# Pourquoi en local : les runners GitHub macOS sont refusés sur ce compte (tout job est annulé
# en quelques secondes, sans étape — vérifié le 13/11/2026). Ce script tourne sur le vrai Mac, avec
# le vrai toolchain ; GitHub ne sert qu'à lire les tags et à ouvrir PR/issue.
#
# Usage :
#   tools/port-check.sh                # dernier tag upstream (releases GitHub)
#   tools/port-check.sh v0.21.1        # un tag précis
#   tools/port-check.sh --dry          # merge à blanc seulement, ni gates ni PR
set -uo pipefail
export PATH="/Volumes/Data/rust/cargo/bin:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"
export RUSTUP_HOME="${RUSTUP_HOME:-/Volumes/Data/rust/rustup}"
export CARGO_HOME="${CARGO_HOME:-/Volumes/Data/rust/cargo}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"; cd "$ROOT"
APP="${APP:-filmcraft}"
UPSTREAM="${UPSTREAM:-storytold/filmcraft}"
BRANCH="${BRANCH:-madson}"
CHECK_CMD="${CHECK_CMD:-cargo check -p filmcraft}"
CLIPPY_CMD="${CLIPPY_CMD:-cargo clippy -p lightcraft-engine --features faces,autoseg -- -D warnings && cargo clippy -p lightcraft-ui-egui -p lightcraft-cli -- -D warnings}"
TEST_CMD="${TEST_CMD:-cargo test -p filmcraft-project --lib}"
DRY=0; TAG=""
for a in "$@"; do case "$a" in --dry) DRY=1 ;; -h|--help) sed -n '2,12p' "$0"; exit 0 ;; *) TAG="$a" ;; esac; done

log() { printf '%s %s\n' "$(date '+%H:%M:%S')" "$*"; }
die() { log "ERREUR: $*"; exit 1; }
command -v git >/dev/null || die "git absent"
command -v cargo >/dev/null || die "cargo absent ($PATH)"

log "dépôt $ROOT (branche $BRANCH), upstream $UPSTREAM"
git remote get-url upstream >/dev/null 2>&1 || git remote add upstream "https://github.com/${UPSTREAM}.git"
git fetch upstream --tags --quiet || die "fetch upstream"
git config rerere.enabled true; git config merge.conflictStyle zdiff3
[ -z "$TAG" ] && TAG=$(gh release view --repo "$UPSTREAM" --json tagName -q .tagName 2>/dev/null || git tag --sort=-v:refname | head -1)
[ -n "$TAG" ] || die "aucun tag upstream"
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null || die "tag $TAG inconnu"
log "cible $TAG"
if git merge-base --is-ancestor "$TAG" "$BRANCH"; then log "$TAG déjà dans $BRANCH — rien à faire"; exit 0; fi

WORK=$(mktemp -d "${TMPDIR:-/tmp}/port-check-XXXXXX")
git worktree add --detach "$WORK" "$BRANCH" --quiet || die "worktree add"
cleanup() { git worktree remove --force "$WORK" >/dev/null 2>&1 || true; rm -rf "$WORK"; }
trap cleanup EXIT
cd "$WORK" || die "cd"

log "merge à blanc de $TAG"
if ! git merge --no-commit --no-ff "$TAG" >/dev/null 2>&1; then
  CONFLICTS=$(git diff --name-only --diff-filter=U); N=$(printf '%s\n' "$CONFLICTS" | grep -c . || true)
  log "CONFLITS: $N"
  mkdir -p "$ROOT/.port-check"; REPORT="$ROOT/.port-check/${TAG//\//_}-conflits.txt"
  { echo "port-check $APP $TAG — $(date '+%Y-%m-%d %H:%M')"; echo; printf '%s\n' "$CONFLICTS"; } > "$REPORT"
  log "rapport: $REPORT"
  REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || true)
  if [ -n "$REPO" ]; then
    TITLE="Port $TAG : $N fichiers en conflit"
    EXISTS=$(gh issue list --repo "$REPO" --search "$TITLE in:title" --json number -q '.[0].number' 2>/dev/null || true)
    [ -n "$EXISTS" ] || gh issue create --repo "$REPO" --title "$TITLE" --body-file "$REPORT" >/dev/null 2>&1 || true
    log "issue: $TITLE"
  fi
  exit 2
fi
log "merge PROPRE"
[ "$DRY" = 1 ] && { log "--dry: fin (gates non lancées)"; exit 0; }

gate_failed=0
log "gates"
(cargo fmt --check) >"$WORK/gate-fmt.log" 2>&1 || gate_failed=1
( eval "$CHECK_CMD" ) >"$WORK/gate-check.log" 2>&1 || gate_failed=1
( eval "$CLIPPY_CMD" ) >"$WORK/gate-clippy.log" 2>&1 || gate_failed=1
( eval "$TEST_CMD" ) >"$WORK/gate-test.log" 2>&1 || gate_failed=1
if [ "$gate_failed" = 1 ]; then
  mkdir -p "$ROOT/.port-check"
  for g in fmt check clippy test; do [ -s "$WORK/gate-$g.log" ] && cp "$WORK/gate-$g.log" "$ROOT/.port-check/${TAG//\//_}-$g.log"; done
  log "GATES EN ÉCHEC — rapports dans $ROOT/.port-check/"
  exit 3
fi
log "gates vertes"

git add -A
MSG="Port ${TAG#v}: merge upstream $TAG (port-check, gates vertes)"
git -c user.name="madson-port-bot" -c user.email="madson-port-bot@users.noreply.github.com" commit -q -m "$MSG" || die "commit"
git push -q origin "HEAD:refs/heads/port-bot/$TAG" || die "push port-bot/$TAG"
REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || true)
if [ -n "$REPO" ]; then
  gh pr list --repo "$REPO" --head "port-bot/$TAG" --json number -q '.[0].number' 2>/dev/null | grep -q . \
    || gh pr create --repo "$REPO" --base "$BRANCH" --head "port-bot/$TAG" \
        --title "Port $TAG : merge upstream (gates vertes)" \
        --body "Merge à blanc + fmt/check/clippy/tests verts sur la machine de Madson (port-check local). Relire docs/port-*.md avant de merger." >/dev/null 2>&1 || true
  log "PR: port-bot/$TAG -> $BRANCH"
fi
log "OK"
