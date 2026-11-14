import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Commons as Commons
import qs.Ui

// Панель omaxy: статус, список серверов (клик — подключиться), in/out.
// Данные — `omaxy status --json`, действия — вызовы omaxy CLI.
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

  readonly property string cli: Quickshell.env("HOME") + "/.local/bin/omaxy"
  readonly property bool daemonUp: state.daemon === true
  readonly property bool connected: state.connected === true
  readonly property var servers: state.servers || []
  readonly property var subs: state.subscriptions || []
  readonly property var active: state.active || ({})
  readonly property color fg: root.bar ? root.bar.foreground : Commons.Color.foreground

  readonly property string statusText: !daemonUp ? "демон выключен"
    : connected ? "подключено" : "выключено"
  readonly property color statusColor: !daemonUp
    ? Qt.rgba(0.95, 0.4, 0.35, 0.9)
    : connected ? Commons.Color.accent : Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.55)

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
  }

  onOpenedChanged: {
    if (opened) refresh()
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
    contentWidth: card.fittedContentWidth(Style.space(320))
    contentHeight: card.fittedContentHeight(col.implicitHeight)

    Column {
      id: col
      anchors.fill: parent
      spacing: Style.space(6)

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

      // ── серверы
      Column {
        width: parent.width
        spacing: 0

        Repeater {
          model: root.servers

          Item {
            required property var modelData
            width: col.width
            height: Style.space(22)
            readonly property bool cur: modelData.active === true ||
              (root.active.id !== undefined && root.active.id === modelData.id)

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
              width: parent.width - protoText.width - Style.space(16)
            }

            Text {
              id: protoText
              anchors.right: parent.right
              anchors.rightMargin: Style.space(6)
              anchors.verticalCenter: parent.verticalCenter
              text: modelData.proto
              color: Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.55)
              font.family: Style.font.family
              font.pixelSize: Style.font.caption
            }

            HoverHandler { id: rowHover }
            TapHandler {
              onTapped: if (!parent.cur && !root.busy) root.act(["connect", parent.modelData.id])
            }
          }
        }

        Text {
          visible: root.servers.length === 0
          width: parent.width
          text: "нет серверов — добавь ссылку ниже"
          color: Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.5)
          font.family: Style.font.family
          font.pixelSize: Style.font.caption
          elide: Text.ElideRight
        }
      }

      // ── in/out
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

        Text {
          anchors.right: parent.right
          anchors.left: mainBtn.right
          anchors.leftMargin: Style.space(8)
          anchors.verticalCenter: parent.verticalCenter
          horizontalAlignment: Text.AlignRight
          elide: Text.ElideRight
          text: root.busy ? root.action
            : root.lastError !== "" ? root.lastError
            : (!root.daemonUp ? "демон не запущен"
               : root.connected && root.active.name ? root.active.name : "—")
          color: Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.65)
          font.family: Style.font.family
          font.pixelSize: Style.font.caption
        }
      }

      // ── запуск демона (если не поднят): два нажатия = подтверждение,
      //    далее polkit-диалог (или systemctl без пароля, если юнит уже установлен)
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
    }
  }
}
