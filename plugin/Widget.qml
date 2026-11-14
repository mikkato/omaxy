import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

// Индикатор omaxy-прокси в правой секции бара; клик — панель серверов.
// Состояние — `omaxy status --json` раз в 2 секунды. Рядом с иконкой —
// скорость: R (принято, Б/с) и S (отправлено, Б/с).
BarWidget {
  id: root
  moduleName: "mikkato.omaxy"

  property bool daemonUp: false
  property bool connected: false
  property string activeName: ""
  property string activeProto: ""
  property var servers: []
  property real rxRate: 0
  property real txRate: 0
  // история скоростей для линейных графиков панели (копится всегда, не только при открытой панели)
  property var rxHist: []
  property var txHist: []

  function pushHist(v: real) {
    root.rxHist = root.rxHist.concat([v]).slice(-120)
  }
  function pushHistTx(v: real) {
    root.txHist = root.txHist.concat([v]).slice(-120)
  }
  property var prevSample: null

  readonly property string cli: Quickshell.env("HOME") + "/.local/bin/omaxy"

  function fmtBps(n) {
    var v
    if (n >= 1048576) v = (n / 1048576).toFixed(1)
    else if (n >= 1024) v = String(Math.round(n / 1024))
    else v = String(Math.round(n))
    while (v.length < 3) v = "0" + v // лидирующие нули — фиксированная ширина
    return v
  }
  function unitBps(n) {
    if (n >= 1048576) return "М"
    if (n >= 1024) return "К"
    return "Б"
  }

  function sampleTraffic(s) {
    var now = Date.now() / 1000
    var t = s.traffic
    if (!root.connected || !t || t.rx === undefined) {
      prevSample = null
      if (rxHist.length > 0 && rxHist[rxHist.length - 1] === 0) return
      rxRate = 0
      txRate = 0
      pushHist(0)
      pushHistTx(0)
      return
    }
    if (prevSample) {
      var dt = now - prevSample.t
      if (dt > 0.25) {
        rxRate = Math.max(0, (t.rx - prevSample.rx) / dt)
        txRate = Math.max(0, (t.tx - prevSample.tx) / dt)
        prevSample = { t: now, rx: t.rx, tx: t.tx }
        pushHist(rxRate)
        pushHistTx(txRate)
      }
    } else {
      prevSample = { t: now, rx: t.rx, tx: t.tx }
    }
  }

  function applyJson(txt) {
    var s = {}
    try {
      s = JSON.parse(txt || "{}")
    } catch (e) {
      return
    }
    daemonUp = !!s.daemon
    connected = !!s.connected
    servers = s.servers || []
    var a = s.active || {}
    activeName = connected ? (a.name || "") : ""
    activeProto = connected ? (a.proto || "") : ""
    sampleTraffic(s)
  }

  readonly property string tip: {
    if (!daemonUp)
      return "omaxy: демон не запущен"
    if (connected)
      return "Прокси: " + activeName + " (" + activeProto + ")\n"
        + "R " + root.fmtBps(root.rxRate) + root.unitBps(root.rxRate) + "/с · S "
            + root.fmtBps(root.txRate) + root.unitBps(root.txRate) + "/с\n"
        + "клик — панель"
    return "Прокси выключен — клик выбирает сервер"
  }

  // ── контракт попапа для бара (как у omarchy.clock)
  readonly property bool opened: panelLoader.item ? panelLoader.item.opened === true : false
  readonly property bool popoutSwitchClosing: panelLoader.item
    ? panelLoader.item.popoutSwitchClosing === true : false

  function open() { if (panelLoader.item) panelLoader.item.open() }
  function close() { if (panelLoader.item) panelLoader.item.close() }
  function togglePanel() { if (panelLoader.item) panelLoader.item.toggle() }
  function closeForPopoutSwitch() {
    if (panelLoader.item) panelLoader.item.closeForPopoutSwitch()
  }

  function injectPanel() {
    var target = panelLoader.item
    if (!target) return
    if ("bar" in target) target.bar = root.bar
    if ("settings" in target) target.settings = root.settings
    if ("anchorItem" in target) target.anchorItem = button
    if ("hostWidget" in target) target.hostWidget = root
  }

  visible: true
  implicitWidth: row.implicitWidth
  implicitHeight: button.implicitHeight

  onBarChanged: injectPanel()
  onSettingsChanged: injectPanel()

  IpcHandler {
    target: "mikkato.omaxy"

    function refresh(): void { root.refresh() }
    function pingAll(): void {
      if (panelLoader.item && panelLoader.item.pingAll) panelLoader.item.pingAll()
    }
    function pingState(): string {
      var it = panelLoader.item
      if (!it || !it.pings) return "{}"
      return JSON.stringify(it.pings)
    }
    function open(): void { root.open() }
    function close(): void { root.close() }
    function show(): void { root.open() }
    function hide(): void { root.close() }
    function toggle(): void { root.togglePanel() }
    function toggleProxy(): void {
      if (root.connected) root.bar.run("uwsm-app -- " + root.cli + " disconnect")
      else root.bar.run("uwsm-app -- " + root.cli + " connect")
    }
  }

  function refresh() {
    if (!statusProc.running)
      statusProc.running = true
    if (root.opened && panelLoader.item && panelLoader.item.refresh)
      panelLoader.item.refresh()
  }

  Process {
    id: statusProc
    command: [root.cli, "status", "--json"]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: root.applyJson(text)
    }
  }

  Timer {
    interval: 2000
    running: true
    repeat: true
    triggeredOnStart: true
    onTriggered: root.refresh()
  }

  Loader {
    id: panelLoader
    active: true
    source: Qt.resolvedUrl("Panel.qml")
    visible: false
    onLoaded: {
      root.injectPanel()
      Qt.callLater(root.injectPanel)
    }
  }

  Row {
    id: row
    anchors.verticalCenter: parent.verticalCenter
    spacing: Style.space(4)

    BarIconButton {
      id: button
      bar: root.bar
      text: "\uf0ac"
      slotSize: Style.bar.statusSlot
      fontSize: Style.font.caption
      useActiveColor: true
      active: root.connected
      tooltipText: root.tip
      onPressed: function(b) {
        if (b === Qt.RightButton) root.toggleProxy()
        else root.togglePanel()
      }
    }

    // скорости R/S — только когда подключено
    Text {
      visible: root.connected
      anchors.verticalCenter: parent.verticalCenter
      text: "R " + root.fmtBps(root.rxRate) + root.unitBps(root.rxRate)
      color: Color.accent
      font.family: "monospace"
      font.pixelSize: Style.font.caption
    }

    Text {
      visible: root.connected
      anchors.verticalCenter: parent.verticalCenter
      text: "S " + root.fmtBps(root.txRate) + root.unitBps(root.txRate)
      color: Qt.rgba(0.95, 0.68, 0.30, 0.95)
      font.family: "monospace"
      font.pixelSize: Style.font.caption
    }
  }
}
