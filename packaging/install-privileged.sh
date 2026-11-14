#!/usr/bin/env bash
# Установка привилегированной части omaxy. Запускать из СВОЕГО терминала:
#   cd ~/omaxy && cargo build --release && sudo ./packaging/install-privileged.sh
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "запусти через sudo"; exit 1; }
cd "$(dirname "$0")/.."

BIN=target/release/omaxy
[ -x "$BIN" ] || { echo "нет $BIN — сначала cargo build --release (без sudo)"; exit 1; }

# 1) xray-core
pacman -S --needed --noconfirm xray || true

# 2) бинарь
install -Dm755 "$BIN" /usr/local/bin/omaxy

# 3) systemd unit (сокет /tmp/happd.sock, как у Happ)
install -Dm644 packaging/omaxyd.service /etc/systemd/system/omaxyd.service

# 4) если работает оригинальный happd — убрать конфликт сокета
if systemctl is-active --quiet happd.service; then
  echo "ВНИМАНИЕ: остановлен оригинальный happd (общий сокет /tmp/happd.sock)"
  systemctl disable --now happd.service || true
fi

systemctl daemon-reload
systemctl enable --now omaxyd.service
systemctl --no-pager status omaxyd.service | head -8
echo "OK. Пользовательская часть: ./packaging/install-user.sh"
