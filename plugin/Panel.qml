import QtQuick
import QtQuick.Controls
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Commons as Commons
import qs.Ui

// Панель omaxy: управление сверху, трафик (две круговые диаграммы),
// прокручиваемый список серверов с пингом. Данные — `omaxy status --json`.
Panel {
  id: root
  moduleName: "mikkato.omaxy"
  ipcTarget: "mikkato.omaxy"
  manageIpc: false

  property var anchorItem: null
  property var hostWidget: null
  readonly property var barIdentity: hostWidget || root

  property var state: ({})
  property bool busy: false
  property string action: ""
  property string lastError: ""
  property bool confirmAdd: false
  property bool confirmDaemon: false

  // скорость, Б/с — из дельты кумулятивных счётчиков TUN
  property real rxRate: 0
  property real txRate: 0
  property var rxHist: []
  property var txHist: []
  property var prevSample: null

  // пинги: id -> {ms: number|-1, busy: bool}
  property var pings: ({})
  property var pingQueue: []
  property bool pingBusy: false
  property string pingCurrent: ""
  property bool pingAllBusy: false

  // настройки прокси (омaxy Settings) — черновик для редактирования
  property bool cfgOpen: false
  property string cfgRoute: "direct-local"
  property bool cfgTun: true
  property string cfgSocks: "10808"
  property string cfgHttp: "10809"
  property string cfgDns: ""
  property string cfgAddr: ""
  property string cfgGw: ""
  property bool confirmApply: false
  property bool confirmStopDaemon: false
  property real now: Date.now() / 1000

  readonly property string cli: Quickshell.env("HOME") + "/.local/bin/omaxy"
  readonly property bool daemonUp: state.daemon === true
  readonly property bool connected: state.connected === true
  readonly property var servers: state.servers || []
  readonly property var subs: state.subscriptions || []
  readonly property var active: state.active || ({})
  readonly property var traffic: state.traffic || ({})
  readonly property real rxTotal: traffic.rx || 0
  readonly property real txTotal: traffic.tx || 0
  readonly property color fg: root.bar ? root.bar.foreground : Commons.Color.foreground
  readonly property color rxColor: Commons.Color.accent
  readonly property color txColor: Qt.rgba(0.95, 0.68, 0.30, 0.95)

  readonly property string statusText: !daemonUp ? "демон выключен"
    : connected ? "подключено" : "выключено"
  readonly property color statusColor: !daemonUp
    ? Qt.rgba(0.95, 0.4, 0.35, 0.9)
    : connected ? Commons.Color.accent : Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.55)

  function fmtBps(n) {
    if (n >= 1048576) return (n / 1048576).toFixed(1) + "М"
    if (n >= 1024) return (n / 1024).toFixed(0) + "К"
    return Math.round(n) + ""
  }
  function fmtMB(n) {
    if (n >= 1073741824) return (n / 1073741824).toFixed(2) + " ГБ"
    if (n >= 1048576) return (n / 1048576).toFixed(1) + " МБ"
    if (n >= 1024) return (n / 1024).toFixed(0) + " КБ"
    return Math.round(n) + " Б"
  }
  function fmtUptime(sinceSec) {
    var s = Math.max(0, Math.floor(root.now - sinceSec))
    var d = Math.floor(s / 86400), h = Math.floor(s % 86400 / 3600)
    var m = Math.floor(s % 3600 / 60)
    if (d > 0) return d + "д " + h + "ч"
    if (h > 0) return h + "ч " + m + "м"
    return m + "м " + (s % 60) + "с"
  }
  function fmtBpsPad(n) {
    var v
    if (n >= 1048576) v = (n / 1048576).toFixed(1)
    else if (n >= 1024) v = String(Math.round(n / 1024))
    else v = String(Math.round(n))
    while (v.length < 3) v = "0" + v
    return v
  }
  function unitBps(n) {
    if (n >= 1048576) return "М"
    if (n >= 1024) return "К"
    return "Б"
  }
  function fmtBpsFull(n) {
    return fmtBps(n) + " Б/с"
  }
  function fmtBytes(n) {
    if (n >= 1073741824) return (n / 1073741824).toFixed(2) + " ГиБ"
    if (n >= 1048576) return (n / 1048576).toFixed(1) + " МиБ"
    if (n >= 1024) return (n / 1024).toFixed(0) + " КиБ"
    return Math.round(n) + " Б"
  }

  function resetConfirms() {
    root.confirmAdd = false
    root.confirmDaemon = false
    root.confirmStopDaemon = false
  }

  function addUri() {
    var t = uriInput.text.trim()
    if (t === "" || busy) return
    if (!confirmAdd) {
      confirmAdd = true
      addConfirmTimer.restart()
      return
    }
    // http(s)-ссылка = подписка (subacone-like), остальное = ссылка сервера
    var isSub = t.indexOf("http://") === 0 || t.indexOf("https://") === 0
    act(isSub ? ["subscribe", "add", t] : ["server", "add", t])
    uriInput.text = ""
  }

  function setPing(id, obj) {
    var p = Object.assign({}, root.pings)
    p[id] = obj
    root.pings = p
  }

  // пинг всего списка: все строки в «…», кнопка гаснет до конца цикла
  function pingAll() {
    if (root.pingAllBusy) return
    root.pingAllBusy = true
    var p = {}
    for (var i = 0; i < root.servers.length; i++) p[root.servers[i].id] = { busy: true, ms: -1 }
    root.pings = p
    procPingAll.running = true
  }

  function applyPingAll(text) {
    var arr = []
    try { arr = JSON.parse(String(text || "[]")) } catch (e) { }
    var p = Object.assign({}, root.pings)
    for (var i = 0; i < arr.length; i++)
      p[arr[i].id] = { busy: false, ms: arr[i].ok === true ? arr[i].min_ms : -1 }
    root.pings = p
    root.pingAllBusy = false
  }

  function pingServer(id) {
    if (root.pingBusy && root.pingCurrent === id) return
    root.pingQueue = root.pingQueue.concat([id])
    root.setPing(id, { busy: true, ms: -1 })
    pumpPing()
  }

  function pumpPing() {
    if (root.pingBusy || root.pingQueue.length === 0) return
    root.pingBusy = true
    root.pingCurrent = root.pingQueue[0]
    procPing.running = true
  }

  function fillCfg() {
    var s = root.state.settings || {}
    cfgRoute = s.route_mode || "direct-local"
    cfgTun = s.tun !== false
    cfgSocks = String(s.socks_port || 10808)
    cfgHttp = String(s.http_port || 10809)
    cfgDns = (s.dns || []).join(", ")
    cfgAddr = s.tun_address || ""
    cfgGw = s.tun_gateway || ""
    confirmApply = false
  }

  function applyCfg() {
    if (!confirmApply) {
      confirmApply = true
      applyConfirmTimer.restart()
      return
    }
    var dns = cfgDns.split(",").map(function (x) { return x.trim() }).filter(Boolean).join(",")
    act(["config", "set",
      "route_mode", cfgRoute,
      "tun", cfgTun ? "true" : "false",
      "socks_port", cfgSocks,
      "http_port", cfgHttp,
      "dns", dns,
      "tun_address", cfgAddr,
      "tun_gateway", cfgGw])
  }

  function refresh() { if (!procState.running) procState.running = true }

  // красная ошибка не должна висеть вечно
  onLastErrorChanged: if (lastError !== "") errTimer.restart()
  Timer {
    id: errTimer
    interval: 6000
    onTriggered: root.lastError = ""
  }

  function act(args) {
    if (busy) return
    busy = true
    lastError = ""
    resetConfirms()
    action = String(args[0] || "")
    procAct.command = [root.cli].concat(args)
    procAct.running = true
  }

  function applyState(text) {
    try { root.state = JSON.parse(String(text || "{}")) } catch (e) { return }
    sampleTraffic()
  }

  function sampleTraffic() {
    var now = Date.now() / 1000
    var t = root.traffic
    if (!root.connected || !t || t.rx === undefined) {
      prevSample = null
      rxRate = 0
      txRate = 0
      rxHist = []
      txHist = []
      return
    }
    if (prevSample) {
      var dt = now - prevSample.t
      if (dt > 0.25) {
        rxRate = Math.max(0, (t.rx - prevSample.rx) / dt)
        txRate = Math.max(0, (t.tx - prevSample.tx) / dt)
        prevSample = { t: now, rx: t.rx, tx: t.tx }
      }
    } else {
      prevSample = { t: now, rx: t.rx, tx: t.tx }
    }
  }

  onOpenedChanged: {
    if (opened) {
      refresh()
      Qt.callLater(fillCfg)
      Qt.callLater(refreshTasks)
    }
  }

  Process {
    id: procState
    command: [root.cli, "status", "--json"]
    stdout: StdioCollector { onStreamFinished: root.applyState(text) }
  }

  Process {
    id: procAct
    stdout: StdioCollector {
      onStreamFinished: {
        root.busy = false
        root.action = ""
        root.refresh()
      }
    }
    stderr: StdioCollector {
      onStreamFinished: {
        var t = String(text || "").trim()
        if (t !== "") root.lastError = t
      }
    }
    onExited: (code) => {
      root.busy = false
      root.action = ""
      root.refresh()
    }
  }

  Process {
    id: procPing
    command: [root.cli, "ping", "--json", root.pingCurrent]
    stdout: StdioCollector {
      onStreamFinished: {
        var r = {}
        try { r = JSON.parse(String(text || "{}")) } catch (e) { }
        var ms = r.ok === true ? (r.min_ms !== undefined ? r.min_ms : -1) : -1
        root.setPing(root.pingCurrent, { busy: false, ms: ms })
      }
    }
    onExited: (code) => {
      root.pingBusy = false
      root.pingQueue = root.pingQueue.slice(1)
      if (root.pingQueue.length === 0) root.pingCurrent = ""
      root.pumpPing()
    }
  }

  // цикл пинга по всему списку (параллельно на стороне CLI)
  Process {
    id: procPingAll
    command: [root.cli, "ping", "--all", "--json"]
    stdout: StdioCollector { onStreamFinished: root.applyPingAll(text) }
    onExited: (code) => { root.pingAllBusy = false }
  }

  // ── обход туннеля: задачи и очередь действий к демону
  property var tasks: []
  property var taskApps: []
  property var bypassPids: []
  property var bypQueue: []
  property var bypCurrent: null

  function refreshTasks() {
    if (!procTasks.running) procTasks.running = true
    if (!procBypList.running) procBypList.running = true
  }

  function applyTasks(text) {
    var list = []
    try { list = JSON.parse(String(text || "[]")) } catch (e) { }
    var seen = {}, apps = []
    for (var i = 0; i < list.length; i++) {
      var c = list[i]
      if (!c.pid) continue
      var cls = c.initialClass || c.class || "?"
      if (seen[cls] === undefined) { seen[cls] = apps.length; apps.push({ cls: cls, pids: [] }) }
      apps[seen[cls]].pids.push(c.pid)
    }
    for (var k = 0; k < apps.length; k++) apps[k].count = apps[k].pids.length + " ок."
    root.taskApps = apps
  }

  function applyBypList(text) {
    var r = {}
    try { r = JSON.parse(String(text || "{}")) } catch (e) { }
    root.bypassPids = (r.pids || []).map(function (p) { return +p })
  }

  function bypEnqueue(op) {
    root.bypQueue = root.bypQueue.concat([op])
    if (root.bypCurrent === null) root.bypPump()
  }

  function bypPump() {
    if (root.bypQueue.length === 0) { root.bypCurrent = null; return }
    root.bypCurrent = root.bypQueue[0]
    procBypAct.running = true
  }

  Process {
    id: procTasks
    command: ["hyprctl", "-j", "clients"]
    stdout: StdioCollector { onStreamFinished: root.applyTasks(text) }
  }

  Process {
    id: procBypList
    command: [root.cli, "call", "bypass-list"]
    stdout: StdioCollector { onStreamFinished: root.applyBypList(text) }
  }

  Process {
    id: procBypAct
    readonly property var cur: root.bypCurrent
    command: cur && cur.op === "clear"
      ? [root.cli, "call", "bypass-clear"]
      : [root.cli, "call", "bypass-" + (cur ? cur.op : "list"),
         JSON.stringify({ pid: cur ? cur.pid : 0 })]
    onExited: (code) => {
      root.bypQueue = root.bypQueue.slice(1)
      if (!procBypList.running) procBypList.running = true
      Qt.callLater(root.bypPump)
    }
  }

  Timer {
    interval: 2500
    repeat: true
    running: root.opened
    triggeredOnStart: true
    onTriggered: root.refresh()
  }

  PopupCard {
    id: card
    anchorItem: root.anchorItem
    bar: root.bar
    owner: root.barIdentity
    open: root.opened
    contentWidth: card.fittedContentWidth(Style.space(340))
    contentHeight: card.fittedContentHeight(col.implicitHeight)

    Column {
      id: col
      anchors.fill: parent
      spacing: Style.space(8)

      // ── заголовок + пилюля статуса
      Item {
        width: parent.width
        height: Math.max(head.implicitHeight, pill.height)

        Text {
          id: head
          text: "Прокси · omaxy"
          color: root.fg
          font.family: Style.font.family
          font.pixelSize: Style.font.body
        }

        BorderSurface {
          id: pill
          anchors.right: parent.right
          anchors.verticalCenter: parent.verticalCenter
          width: statusLabel.implicitWidth + Style.space(10)
          height: Style.space(16)
          radius: height / 2
          color: "transparent"
          borderSpec: Border.controlSpec("normal", root.statusColor, root.statusColor)

          Text {
            id: statusLabel
            anchors.centerIn: parent
            text: root.statusText
            color: root.statusColor
            font.family: Style.font.family
            font.pixelSize: Style.font.caption
          }
        }
      }

      // ── информер подключённого сервера
      BorderSurface {
        visible: root.connected && root.active.name !== undefined
        width: parent.width
        height: visible ? infPad.implicitHeight + Style.space(12) : 0
        radius: Style.space(6)
        color: Qt.rgba(Commons.Color.accent.r, Commons.Color.accent.g,
                       Commons.Color.accent.b, 0.07)
        borderSpec: Border.controlSpec("normal",
          Qt.rgba(Commons.Color.accent.r, Commons.Color.accent.g,
                  Commons.Color.accent.b, 0.45),
          Qt.rgba(Commons.Color.accent.r, Commons.Color.accent.g,
                  Commons.Color.accent.b, 0.45))

        Row {
          id: infPad
          anchors.left: parent.left
          anchors.right: parent.right
          anchors.verticalCenter: parent.verticalCenter
          anchors.leftMargin: Style.space(10)
          anchors.rightMargin: Style.space(10)
          spacing: Style.space(8)

          Rectangle {
            id: infDot
            width: Style.space(7)
            height: width
            radius: height / 2
            anchors.verticalCenter: parent.verticalCenter
            color: Commons.Color.accent
            SequentialAnimation on opacity {
              running: root.connected
              loops: Animation.Infinite
              NumberAnimation { from: 1; to: 0.35; duration: 900 }
              NumberAnimation { from: 0.35; to: 1; duration: 900 }
            }
          }

          Column {
            id: infCol
            width: infPad.width - infDot.width - infPad.spacing
            spacing: 1

            Text {
              width: parent.width
              elide: Text.ElideRight
              text: "🟢 " + (root.active.name || "")
              color: root.fg
              font.family: Style.font.family
              font.pixelSize: Style.font.bodySmall
              font.bold: true
            }
            Text {
              width: parent.width
              elide: Text.ElideRight
              text: (root.active.address || "") + ":" + (root.active.port || "")
                + " · " + (root.active.proto || "")
                + (root.active.since ? " · на связи " + root.fmtUptime(root.active.since) : "")
                + " · ↓" + root.fmtMB(root.rxTotal) + " ↑" + root.fmtMB(root.txTotal)
              color: Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.6)
              font.family: Style.font.family
              font.pixelSize: Style.font.caption
            }
          }
        }
      }

      Timer {
        interval: 1000
        running: root.connected
        repeat: true
        onTriggered: {
          root.now = Date.now() / 1000
          root.rxHist = root.rxHist.concat([root.rxRate]).slice(-60)
          root.txHist = root.txHist.concat([root.txRate]).slice(-60)
        }
      }

      // ── органы управления (над списком)
      Button {
        visible: !root.daemonUp
        width: parent.width
        text: root.confirmDaemon
          ? "запустить демон от root — подтвердить?"
          : "⏻ запустить демон"
        fontSize: Style.font.bodySmall
        foreground: root.confirmDaemon ? Commons.Color.accent : root.fg
        accent: Commons.Color.accent
        bordered: true
        horizontalPadding: Style.space(10)
        verticalPadding: Style.space(2)
        enabled: !root.busy
        onClicked: {
          if (!root.confirmDaemon) {
            root.confirmDaemon = true
            daemonConfirmTimer.restart()
            return
          }
          root.act(["daemon", "ensure"])
        }
      }

      Timer {
        id: daemonConfirmTimer
        interval: 5000
        onTriggered: root.confirmDaemon = false
      }

      Item {
        width: parent.width
        height: Style.space(26)

        Button {
          id: mainBtn
          anchors.left: parent.left
          anchors.verticalCenter: parent.verticalCenter
          text: root.connected ? "Выключить" : "Включить"
          fontSize: Style.font.bodySmall
          foreground: root.fg
          accent: Commons.Color.accent
          bordered: true
          horizontalPadding: Style.space(10)
          verticalPadding: Style.space(2)
          enabled: !root.busy && root.daemonUp && root.servers.length > 0
          onClicked: root.act([root.connected ? "disconnect" : "connect"])
        }

        Button {
          id: pingActiveBtn
          anchors.left: mainBtn.right
          anchors.leftMargin: Style.space(6)
          anchors.verticalCenter: parent.verticalCenter
          text: root.pingAllBusy ? "⚡ пинг…" : "⚡ пинг"
          fontSize: Style.font.bodySmall
          foreground: root.fg
          accent: Commons.Color.accent
          bordered: true
          horizontalPadding: Style.space(8)
          verticalPadding: Style.space(2)
          // дезактивирована на всё время цикла пинга
          enabled: !root.pingAllBusy && !root.pingBusy && root.servers.length > 0
          onClicked: root.pingAll()
        }

        Button {
          id: stopDaemonBtn
          anchors.left: pingActiveBtn.right
          anchors.leftMargin: Style.space(6)
          anchors.verticalCenter: parent.verticalCenter
          visible: root.daemonUp
          text: root.confirmStopDaemon ? "стоп демон — подтвердить?" : "⏻ стоп"
          fontSize: Style.font.bodySmall
          foreground: root.confirmStopDaemon ? Qt.rgba(0.95, 0.4, 0.35, 1) : root.fg
          accent: Commons.Color.accent
          bordered: true
          horizontalPadding: Style.space(8)
          verticalPadding: Style.space(2)
          enabled: !root.busy
          onClicked: {
            if (!root.confirmStopDaemon) {
              root.confirmStopDaemon = true
              stopConfirmTimer.restart()
            } else {
              root.confirmStopDaemon = false
              root.act(["daemon", "stop"])
            }
          }
          Timer {
            id: stopConfirmTimer
            interval: 5000
            onTriggered: root.confirmStopDaemon = false
          }
        }

        Text {
          anchors.right: parent.right
          anchors.left: stopDaemonBtn.visible ? stopDaemonBtn.right : pingActiveBtn.right
          anchors.leftMargin: Style.space(8)
          anchors.verticalCenter: parent.verticalCenter
          horizontalAlignment: Text.AlignRight
          elide: Text.ElideRight
          text: root.busy ? root.action
            : root.lastError !== "" ? root.lastError
            : (!root.daemonUp ? "демон не запущен"
               : root.connected && root.active.name ? root.active.name : "—")
          color: root.lastError !== ""
            ? Qt.rgba(0.95, 0.4, 0.35, 0.9)
            : Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.65)
          font.family: Style.font.family
          font.pixelSize: Style.font.caption
        }
      }

      // ── зона ввода URI с подтверждением
      Row {
        width: parent.width
        spacing: Style.space(6)

        TextField {
          id: uriInput
          width: parent.width - uriAddBtn.implicitWidth - parent.spacing
          placeholderText: "ссылка vless://… или подписка https://…"
          font.pixelSize: Style.font.bodySmall
          foreground: root.fg
          accent: Commons.Color.accent
          verticalPadding: Style.space(2)
          onAccepted: root.addUri()
          onTextChanged: root.confirmAdd = false
        }

        Button {
          id: uriAddBtn
          text: root.confirmAdd ? "подтвердить?" : "Добавить"
          fontSize: Style.font.bodySmall
          foreground: root.confirmAdd ? Commons.Color.accent : root.fg
          accent: Commons.Color.accent
          bordered: true
          horizontalPadding: Style.space(10)
          verticalPadding: Style.space(2)
          enabled: !root.busy && uriInput.text.trim().length > 0
          onClicked: root.addUri()
        }
      }

      Timer {
        id: addConfirmTimer
        interval: 5000
        onTriggered: root.confirmAdd = false
      }

      // ── настройки прокси (Settings из исходников omaxy)
      Button {
        width: parent.width
        text: root.cfgOpen ? "⚙ настройки прокси ▴" : "⚙ настройки прокси ▾"
        fontSize: Style.font.bodySmall
        foreground: Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.75)
        accent: Commons.Color.accent
        bordered: true
        horizontalPadding: Style.space(10)
        verticalPadding: Style.space(2)
        onClicked: {
          root.cfgOpen = !root.cfgOpen
          if (root.cfgOpen) root.fillCfg()
        }
      }

      Column {
        visible: root.cfgOpen
        width: parent.width
        spacing: Style.space(6)

        // режим маршрутизации
        Row {
          width: parent.width
          spacing: Style.space(6)
          Repeater {
            model: [
              { key: "direct-local", label: "локальное напрямую" },
              { key: "global", label: "всё через прокси" }
            ]
            Button {
              required property var modelData
              width: (col.width - Style.space(6)) / 2
              text: (root.cfgRoute === modelData.key ? "● " : "○ ") + modelData.label
              fontSize: Style.font.caption
              foreground: root.cfgRoute === modelData.key
                ? Commons.Color.accent
                : Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.65)
              accent: Commons.Color.accent
              bordered: true
              horizontalPadding: Style.space(4)
              verticalPadding: Style.space(2)
              onClicked: root.cfgRoute = modelData.key
            }
          }
        }

        // режим портов + TUN
        Row {
          width: parent.width
          spacing: Style.space(6)

          Button {
            text: (root.cfgTun ? "● " : "○ ") + "TUN-режим"
            fontSize: Style.font.caption
            foreground: root.cfgTun ? Commons.Color.accent : Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.65)
            accent: Commons.Color.accent
            bordered: true
            verticalPadding: Style.space(2)
            onClicked: root.cfgTun = !root.cfgTun
          }

          Row {
            spacing: Style.space(4)
            Text {
              anchors.verticalCenter: parent.verticalCenter
              text: "socks"
              color: Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.55)
              font.pixelSize: Style.font.caption
              font.family: Style.font.family
            }
            TextField {
              width: Style.space(52)
              text: root.cfgSocks
              font.pixelSize: Style.font.caption
              verticalPadding: 0
              onTextChanged: root.cfgSocks = text
            }
            Text {
              anchors.verticalCenter: parent.verticalCenter
              text: "http"
              color: Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.55)
              font.pixelSize: Style.font.caption
              font.family: Style.font.family
            }
            TextField {
              width: Style.space(52)
              text: root.cfgHttp
              font.pixelSize: Style.font.caption
              verticalPadding: 0
              onTextChanged: root.cfgHttp = text
            }
          }
        }

        TextField {
          width: parent.width
          placeholderText: "DNS, через запятую"
          text: root.cfgDns
          font.pixelSize: Style.font.caption
          verticalPadding: Style.space(1)
          onTextChanged: root.cfgDns = text
        }

        Row {
          width: parent.width
          spacing: Style.space(6)
          TextField {
            width: (parent.width - Style.space(6)) / 2
            placeholderText: "адрес TUN (CIDR)"
            text: root.cfgAddr
            font.pixelSize: Style.font.caption
            verticalPadding: Style.space(1)
            onTextChanged: root.cfgAddr = text
          }
          TextField {
            width: (parent.width - Style.space(6)) / 2
            placeholderText: "шлюз TUN"
            text: root.cfgGw
            font.pixelSize: Style.font.caption
            verticalPadding: Style.space(1)
            onTextChanged: root.cfgGw = text
          }
        }

        Button {
          width: parent.width
          text: root.confirmApply
            ? "применить и перезапустить подключение — подтвердить?"
            : "Применить"
          fontSize: Style.font.bodySmall
          foreground: root.confirmApply ? Commons.Color.accent : root.fg
          accent: Commons.Color.accent
          bordered: true
          verticalPadding: Style.space(2)
          enabled: !root.busy
          onClicked: root.applyCfg()
        }

        Timer {
          id: applyConfirmTimer
          interval: 5000
          onTriggered: root.confirmApply = false
        }

        Text {
          width: parent.width
          wrapMode: Text.WordWrap
          text: root.cfgTun
            ? "TUN: весь трафик идёт через omaxy0; выкл — только SOCKS/HTTP на локальных портах"
            : " SOCKS " + root.cfgSocks + " · HTTP " + root.cfgHttp + " — приложения настраиваются вручную"
          color: Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.5)
          font.family: Style.font.family
          font.pixelSize: Style.font.caption
        }
      }

      // ── трафик: линейные графики скоростей
      Row {
        visible: root.daemonUp
        width: parent.width
        height: implicitHeight
        spacing: Style.space(14)

        TrafficGraph {
          id: graphRx
          label: "↓ приём"
          color: root.rxColor
          foreground: root.fg
          history: root.hostWidget && root.hostWidget.rxHist !== undefined
            ? root.hostWidget.rxHist : root.rxHist
          line1: "скор " + root.fmtBpsPad(root.rxRate) + root.unitBps(root.rxRate) + "/с"
          line2: "всег " + root.fmtBytes(root.rxTotal)
        }

        TrafficGraph {
          id: graphTx
          label: "↑ передача"
          color: root.txColor
          foreground: root.fg
          history: root.hostWidget && root.hostWidget.txHist !== undefined
            ? root.hostWidget.txHist : root.txHist
          line1: "скор " + root.fmtBpsPad(root.txRate) + root.unitBps(root.txRate) + "/с"
          line2: "всег " + root.fmtBytes(root.txTotal)
        }

        // подписки — справом столбцом той же строки
        Column {
          width: Math.max(Style.space(90),
            parent.width - graphRx.implicitWidth - graphTx.implicitWidth - 2 * Style.space(14))
          visible: root.subs.length > 0
          spacing: 0
          Text {
            text: "Подписки"
            color: Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.55)
            font.family: Style.font.family
            font.pixelSize: Style.font.caption
          }
          Repeater {
            model: root.subs
            Text {
              width: parent.width
              text: modelData.name + " · " + modelData.count
              color: Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.8)
              font.family: Style.font.family
              font.pixelSize: Style.font.caption
              elide: Text.ElideRight
            }
          }
          Button {
            visible: root.subs.length > 0
            text: "⟳ обновить"
            fontSize: Style.font.caption
            foreground: root.fg
            accent: Commons.Color.accent
            bordered: true
            horizontalPadding: Style.space(6)
            verticalPadding: 0
            enabled: !root.busy
            onClicked: root.act(["subscribe", "update"])
          }
        }
      }

      // ── обход туннеля: приложения из списка задач
      Row {
        visible: root.daemonUp && root.connected
        width: parent.width
        spacing: Style.space(8)
        Text {
            text: "Обход туннеля — приложения"
            anchors.verticalCenter: parent.verticalCenter
            color: Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.55)
            font.family: Style.font.family
            font.pixelSize: Style.font.caption
        }
        Button {
            text: "⟳"
            fontSize: Style.font.caption
            foreground: root.fg
            accent: Commons.Color.accent
            onClicked: root.refreshTasks()
        }
        Button {
            visible: root.bypassPids.length > 0
            text: "сбросить"
            fontSize: Style.font.caption
            foreground: root.fg
            accent: Commons.Color.accent
            onClicked: root.bypEnqueue({ op: "clear", pid: 0 })
        }
      }

      Flickable {
        visible: root.daemonUp && root.connected && root.tasks.length > 0
        width: parent.width
        height: Math.min(taskCol.implicitHeight, Style.space(140))
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        contentHeight: taskCol.implicitHeight
        contentWidth: width
        ScrollIndicator.vertical: ScrollIndicator { }

        Column {
          id: taskCol
          width: parent.width

          Repeater {
            model: root.taskApps

            Item {
              required property var modelData
              width: taskCol.width
              height: Style.space(20)
              readonly property bool on: modelData.pids.some(
                p => root.bypassPids.indexOf(p) >= 0)

              Row {
                anchors.fill: parent
                spacing: Style.space(6)
                Text {
                  anchors.verticalCenter: parent.verticalCenter
                  text: parent.on ? "◉" : "○"
                  color: parent.on ? Commons.Color.accent
                                   : Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.45)
                  font.pixelSize: Style.font.body
                }
                Text {
                  anchors.verticalCenter: parent.verticalCenter
                  width: taskCol.width - Style.space(40)
                  text: modelData.cls + "  ·  " + modelData.count
                  color: parent.on ? root.fg
                                   : Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.75)
                  font.family: Style.font.family
                  font.pixelSize: Style.font.caption
                  elide: Text.ElideRight
                }
              }

              MouseArea {
                anchors.fill: parent
                cursorShape: Qt.PointingHandCursor
                onClicked: {
                  if (parent.on)
                    root.bypEnqueue({ op: "remove", pid: modelData.pids[0] })
                  else
                    for (var i = 0; i < modelData.pids.length; i++)
                      root.bypEnqueue({ op: "add", pid: modelData.pids[i] })
                }
              }
            }
          }
        }
      }

      Text {
        visible: root.daemonUp && root.connected && root.bypassPids.length > 0
        width: parent.width
        wrapMode: Text.WordWrap
        text: "отмеченные приложения ходят напрямую, мимо туннеля (новые соединения)"
        color: Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.5)
        font.family: Style.font.family
        font.pixelSize: Style.font.caption
      }

      // ── серверы: компактный прокручиваемый список с пингом
      Item { width: parent.width; height: Style.space(6) }
      Text {
        text: "Серверы (" + root.servers.length + ")"
        visible: root.servers.length > 0
        color: Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.55)
        font.family: Style.font.family
        font.pixelSize: Style.font.caption
      }

      Flickable {
        id: serverFlick
        width: parent.width
        height: Math.min(listCol.implicitHeight, Style.space(170))
        visible: root.servers.length > 0
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        contentHeight: listCol.implicitHeight
        contentWidth: width
        ScrollIndicator.vertical: ScrollIndicator { }

        Column {
          id: listCol
          width: parent.width

          Repeater {
            model: root.servers

            Item {
              required property var modelData
              width: listCol.width
              height: Style.space(22)
              readonly property bool cur: modelData.active === true ||
                (root.active.id !== undefined && root.active.id === modelData.id)
              readonly property var ping: root.pings[modelData.id]

              Rectangle {
                anchors.fill: parent
                radius: Style.space(4)
                color: rowHover.hovered || parent.cur
                  ? Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.10) : "transparent"
              }

              Text {
                anchors.left: parent.left
                anchors.leftMargin: Style.space(6)
                anchors.verticalCenter: parent.verticalCenter
                text: (parent.cur ? "✓ " : "   ") + modelData.name
                color: root.fg
                font.family: Style.font.family
                font.pixelSize: Style.font.bodySmall
                elide: Text.ElideRight
                width: parent.width - rightRow.width - Style.space(16)
              }

              Row {
                id: rightRow
                anchors.right: parent.right
                anchors.rightMargin: Style.space(6)
                anchors.verticalCenter: parent.verticalCenter
                spacing: Style.space(6)

                // результат пинга напротив сервера
                Text {
                  anchors.verticalCenter: parent.verticalCenter
                  visible: parent.parent.ping !== undefined
                  text: {
                    var p = parent.parent.ping
                    if (p === undefined) return ""
                    if (p.busy === true) return "…"
                    if (p.ms < 0) return "×"
                    return p.ms + " мс"
                  }
                  color: {
                    var p = parent.parent.ping
                    if (p === undefined || p.busy === true) return Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.55)
                    if (p.ms < 0) return Qt.rgba(0.95, 0.4, 0.35, 0.9)
                    if (p.ms <= 80) return Qt.rgba(0.45, 0.85, 0.5, 0.95)
                    if (p.ms <= 200) return Qt.rgba(0.95, 0.75, 0.35, 0.95)
                    return Qt.rgba(0.95, 0.5, 0.35, 0.95)
                  }
                  font.family: Style.font.family
                  font.pixelSize: Style.font.caption
                }

                Text {
                  id: protoText
                  anchors.verticalCenter: parent.verticalCenter
                  text: modelData.proto
                  color: Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.55)
                  font.family: Style.font.family
                  font.pixelSize: Style.font.caption
                }

                // ⚡ пинг именно этого сервера
                Text {
                  anchors.verticalCenter: parent.verticalCenter
                  text: "⚡"
                  font.family: Style.font.family
                  font.pixelSize: Style.font.caption
                  color: pingHover.hovered
                    ? Commons.Color.accent
                    : Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.4)
                  HoverHandler {
                    id: pingHover
                    cursorShape: Qt.PointingHandCursor
                  }
                  TapHandler {
                    onTapped: root.pingServer(modelData.id)
                  }
                }
              }

              HoverHandler { id: rowHover }
              TapHandler {
                onTapped: if (!parent.cur && !root.busy) root.act(["connect", parent.modelData.id])
              }
            }
          }
        }
      }

      Text {
        visible: root.servers.length === 0
        width: parent.width
        text: "нет серверов — добавь ссылку выше"
        color: Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.5)
        font.family: Style.font.family
        font.pixelSize: Style.font.caption
        elide: Text.ElideRight
      }
    }
  }

  // ── круговая диаграмма трафика (пончик с долей)
  component TrafficGraph: Item {
    id: gr
    property string label: ""
    property color color: Commons.Color.accent
    property color foreground: Commons.Color.foreground
    property var history: []
    property string line1: ""
    property string line2: ""
    implicitWidth: Style.space(150)
    implicitHeight: gcap.implicitHeight + canvas.height + Style.space(2)
        + infoCol.implicitHeight
    // Row раскладывает по width/height, а Item без них имеет ширину 0 —
    // без этих привязок графики рисовались друг поверх друга
    width: implicitWidth
    height: implicitHeight

    onHistoryChanged: canvas.requestPaint()
    onColorChanged: canvas.requestPaint()

    Text {
      id: gcap
      text: gr.label
      color: Qt.rgba(gr.foreground.r, gr.foreground.g, gr.foreground.b, 0.55)
      font.family: Style.font.family
      font.pixelSize: Style.font.caption
    }

    Canvas {
      id: canvas
      anchors.top: gcap.bottom
      anchors.left: parent.left
      anchors.right: parent.right
      height: Style.space(22)
      onPaint: {
        var ctx = getContext("2d")
        ctx.clearRect(0, 0, width, height)
        // базовая линия видна всегда, даже без данных (как в llmctl-gui)
        ctx.strokeStyle = Qt.rgba(1, 1, 1, 0.15)
        ctx.lineWidth = 1
        ctx.beginPath()
        ctx.moveTo(0, height - 0.5)
        ctx.lineTo(width, height - 0.5)
        ctx.stroke()
        var h = gr.history
        if (!h || h.length < 2) { return }
        var mx = 1024
        for (var i = 0; i < h.length; i++) { if (h[i] > mx) mx = h[i] }
        var base = height - 1.5
        ctx.beginPath()
        ctx.moveTo(0, base)
        for (var j = 0; j < h.length; j++) {
          var x = j / (h.length - 1) * width
          var y = base - (h[j] / mx) * (height - 4)
          ctx.lineTo(x, y)
        }
        ctx.strokeStyle = Qt.rgba(gr.color.r, gr.color.g, gr.color.b, 0.95)
        ctx.lineWidth = 1.5
        ctx.lineJoin = "round"
        ctx.stroke()
        ctx.lineTo(width, base)
        ctx.lineTo(0, base)
        ctx.closePath()
        ctx.fillStyle = Qt.rgba(gr.color.r, gr.color.g, gr.color.b, 0.14)
        ctx.fill()
      }
    }

    Column {
      id: infoCol
      anchors.top: canvas.bottom
      anchors.topMargin: 1
      anchors.left: parent.left
      spacing: 0
      Text {
        text: gr.line1
        color: gr.foreground
        font.family: "monospace"
        font.pixelSize: Style.font.caption
      }
      Text {
        text: gr.line2
        color: Qt.rgba(gr.foreground.r, gr.foreground.g, gr.foreground.b, 0.55)
        font.family: "monospace"
        font.pixelSize: Style.font.caption
      }
    }
  }
}
