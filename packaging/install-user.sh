#!/usr/bin/env bash
# Пользовательская часть omaxy: CLI в PATH + модуль панели Omarchy. Без sudo.
set -euo pipefail
cd "$(dirname "$0")/.."

BIN_SRC=target/release/omaxy
[ -x "$BIN_SRC" ] || BIN_SRC=target/debug/omaxy
mkdir -p ~/.local/bin
install -m755 "$BIN_SRC" ~/.local/bin/omaxy

# QML-модуль панели (копия плагина в конфиг, переименован по id)
DEST=~/.config/omarchy/plugins/mikkato.omaxy
mkdir -p "$DEST"
cp plugin/manifest.json plugin/Widget.qml plugin/Panel.qml "$DEST/"

if command -v omarchy >/dev/null; then
  omarchy plugin validate "$DEST" >/dev/null 2>&1 && echo "плагин валиден" || echo "ВНИМАНИЕ: validate не прошёл"
  omarchy plugin enable mikkato.omaxy 2>/dev/null || true
fi
echo "OK. Перезапусти шелл: omarchy restart shell (или omarchy-restart-shell)"
echo "Добавь сервер: omaxy server add '<vless://...>' && omaxy connect"
