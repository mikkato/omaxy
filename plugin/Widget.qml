import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

// Индикатор omaxy-прокси в правой секции бара; клик — панель серверов.
// Состояние — `omaxy status --json` раз в 5 секунд.
BarWidget {
  id: root
  moduleName: "mikkato.omaxy"

  property bool daemonUp: false
  property bool connected: false
  property string activeName: ""
  property string activeProto: ""
  property var servers: []

  readonly property string cli: Quickshell.env("HOME") + "/.local/bin/omaxy"

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
  }

  readonly property string tip: {
    if (!daemonUp)
      return "omaxy: демон не запущен"
    if (connected)
      return "Прокси: " + activeName + " (" + activeProto + ") — клик для панели"
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
  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  onBarChanged: injectPanel()
  onSettingsChanged: injectPanel()

  IpcHandler {
    target: "mikkato.omaxy"

    function refresh(): void { root.refresh() }
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
    interval: 5000
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

  BarIconButton {
    id: button
    anchors.fill: parent
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
}
