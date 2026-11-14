# Maintainer: mikkato <mikkato@users.noreply.github.com>
pkgname=omaxy
pkgver=0.1.0
pkgrel=1
pkgdesc="Open Xray proxy client for Omarchy/Hyprland: TUN daemon (happd-wire-compatible), CLI, QML panel module, GUI"
arch=('x86_64')
url="https://github.com/mikkato/omaxy"
license=('MIT')
depends=('glibc' 'xray')
makedepends=('rust' 'cargo')
optdepends=('polkit: privilege elevation')
install=packaging/omaxy.install
source=("$pkgname-$pkgver.tar.gz::https://github.com/mikkato/$pkgname/archive/refs/tags/v$pkgver.tar.gz")
sha256sums=('SKIP')

build() {
  cd "$srcdir/$pkgname-$pkgver"
  cargo build --release
}

package() {
  cd "$srcdir/$pkgname-$pkgver"
  install -Dm755 target/release/omaxy "$pkgdir/usr/local/bin/omaxy"
  [ -f target/release/omaxy-gui ] && install -Dm755 target/release/omaxy-gui "$pkgdir/usr/local/bin/omaxy-gui"
  install -Dm644 packaging/omaxyd.service "$pkgdir/usr/lib/systemd/system/omaxyd.service"
  install -Dm644 packaging/omaxy-gui.desktop "$pkgdir/usr/share/applications/omaxy-gui.desktop" 2>/dev/null || true
  # QML-модуль панели Omarchy
  install -Dm644 plugin/manifest.json "$pkgdir/usr/share/omaxy/plugin/manifest.json"
  install -Dm644 plugin/Widget.qml "$pkgdir/usr/share/omaxy/plugin/Widget.qml"
  install -Dm644 plugin/Panel.qml "$pkgdir/usr/share/omaxy/plugin/Panel.qml"
}
