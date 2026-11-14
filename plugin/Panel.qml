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
  property var prevSample: null

  // пинги: id -> {ms: number|-1, busy: bool}
  property var pings: ({})
  property var pingQueue: []
  property bool pingBusy: false
  property string pingCurrent: ""

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
          text: "⚡ пинг"
          fontSize: Style.font.bodySmall
          foreground: root.fg
          accent: Commons.Color.accent
          bordered: true
          horizontalPadding: Style.space(8)
          verticalPadding: Style.space(2)
          enabled: !root.pingBusy && root.servers.length > 0
          // пинг активного сервера; если не выбран — первого видимого
          onClicked: {
            var id = root.active.id
            if (id === undefined && root.servers.length > 0) id = root.servers[0].id
            if (id !== undefined) root.pingServer(id)
          }
        }

        Text {
          anchors.right: parent.right
          anchors.left: pingActiveBtn.right
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

      // ── трафик: две круговые диаграммы (доля приёма/передачи)
      Row {
        visible: root.daemonUp
        width: parent.width
        spacing: Style.space(16)

        TrafficDial {
          id: dialRx
          label: "Приём"
          color: root.rxColor
          foreground: root.fg
          fraction: root.rxTotal + root.txTotal > 0
            ? root.rxTotal / (root.rxTotal + root.txTotal) : 0
          line1: root.fmtBpsFull(root.rxRate)
          line2: "всего " + root.fmtBytes(root.rxTotal)
        }

        TrafficDial {
          id: dialTx
          label: "Передача"
          color: root.txColor
          foreground: root.fg
          fraction: root.rxTotal + root.txTotal > 0
            ? root.txTotal / (root.rxTotal + root.txTotal) : 0
          line1: root.fmtBpsFull(root.txRate)
          line2: "всего " + root.fmtBytes(root.txTotal)
        }

        // подписки — справом столбцом той же строки
        Column {
          width: Math.max(Style.space(90),
            parent.width - dialRx.implicitWidth - dialTx.implicitWidth - 2 * Style.space(16))
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

      // ── серверы: компактный прокручиваемый список с пингом
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
  component TrafficDial: Item {
    id: dial
    property string label: ""
    property color color: Commons.Color.accent
    property color foreground: Commons.Color.foreground
    property real fraction: 0
    property string line1: ""
    property string line2: ""
    readonly property real dialSize: Style.space(46)
    implicitWidth: Math.max(dialSize, rowTxt.implicitWidth)
    implicitHeight: dialSize + Style.space(2) + rowTxt.implicitHeight

    onFractionChanged: canvas.requestPaint()
    onColorChanged: canvas.requestPaint()

    Canvas {
      id: canvas
      width: dial.dialSize
      height: dial.dialSize
      onPaint: {
        var ctx = getContext("2d")
        ctx.clearRect(0, 0, width, height)
        var lw = width * 0.16
        var r = (width - lw) / 2
        var cx = width / 2
        ctx.beginPath()
        ctx.arc(cx, cx, r, 0, 2 * Math.PI)
        ctx.lineWidth = lw
        ctx.strokeStyle = Qt.rgba(dial.foreground.r, dial.foreground.g, dial.foreground.b, 0.14)
        ctx.stroke()
        if (dial.fraction > 0.003) {
          ctx.beginPath()
          var a0 = -Math.PI / 2
          ctx.arc(cx, cx, r, a0, a0 + 2 * Math.PI * Math.min(1, dial.fraction))
          ctx.lineWidth = lw
          ctx.lineCap = "round"
          ctx.strokeStyle = Qt.rgba(dial.color.r, dial.color.g, dial.color.b, 0.95)
          ctx.stroke()
        }
      }
    }

    Text {
      anchors.centerIn: canvas
      text: Math.round(dial.fraction * 100) + "%"
      color: dial.color
      font.family: Style.font.family
      font.pixelSize: Style.font.caption
      font.bold: true
    }

    Column {
      id: rowTxt
      anchors.top: canvas.bottom
      anchors.topMargin: Style.space(2)
      anchors.horizontalCenter: parent.horizontalCenter
      spacing: 0
      Text {
        text: dial.label + " · " + dial.line1
        color: dial.foreground
        font.family: Style.font.family
        font.pixelSize: Style.font.caption
      }
      Text {
        text: dial.line2
        color: Qt.rgba(dial.foreground.r, dial.foreground.g, dial.foreground.b, 0.55)
        font.family: Style.font.family
        font.pixelSize: Style.font.caption
      }
    }
  }
}
