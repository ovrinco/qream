pragma ComponentBehavior: Bound
import QtQuick
import Quickshell
import ".."

// One viewer of a daemon pane: attached while shown, drawn from the daemon's damage events as
// rows of styled runs on a monospace grid, with the cursor and a private history offset. Keys and
// paste go to this pane only, as `term.pane.input`; the window types nothing of its own.
FocusScope {
    id: term
    property string pane: ""
    property string task: ""
    onTaskChanged: lines.clear()
    readonly property bool live: visible && pane.length > 0 && Backend.connected && Backend.build.length > 0
    property string viewer: ""
    property bool attaching: false
    // Optional identity fields from newer engines; older attach replies remain valid.
    property var paneGeneration: null
    property string paneStartedAt: ""
    property var generation: null
    property string startedAt: ""
    property int attachEpoch: 0
    property int retryStep: 0
    readonly property bool stale: !Backend.connected || Backend.stale

    function identityChanged(data) {
        const changed = (data.generation !== undefined && generation !== null && data.generation !== generation)
            || (data.started_at !== undefined && startedAt.length > 0 && data.started_at !== startedAt);
        if (data.generation !== undefined)
            generation = data.generation;
        if (data.started_at !== undefined)
            startedAt = data.started_at;
        return changed;
    }
    function scheduleRetry() {
        if (!live || viewer.length > 0 || attaching || retryTimer.running)
            return;
        retryTimer.interval = [250, 500, 1000, 2000][Math.min(retryStep, 3)];
        retryStep = Math.min(retryStep + 1, 3);
        retryTimer.start();
    }
    function reattach() {
        detach();
        retryStep = 0;
        attach();
    }
    function adoptIdentity() {
        const data = {};
        if (paneGeneration !== null)
            data.generation = paneGeneration;
        if (paneStartedAt.length > 0)
            data.started_at = paneStartedAt;
        if (identityChanged(data))
            reattach();
    }
    onPaneGenerationChanged: adoptIdentity()
    onPaneStartedAtChanged: adoptIdentity()
    onStaleChanged: {
        if (stale)
            detach();
        retryStep = 0;
        scheduleRetry();
    }
    property int offset: 0
    property int scrollbackLen: 0
    property var cursor: ({
            x: 0,
            y: 0,
            visible: false
        })
    property var modes: ({})
    // The reading place kept while hidden: offset and history length at detach.
    property int savedOffset: 0
    property int savedHistory: 0
    // Frame time of full-screen updates: event stamp to the frame on screen, and apply to frame.
    property double pendingAt: 0
    property double pendingApply: 0
    property real lastFrameMs: 0
    property real worstFrameMs: 0
    property real worstStampMs: 0
    property real lastApplyMs: 0
    property int fullFrames: 0

    readonly property int size: Backend.paneSize(pane)
    readonly property real cellW: metrics.advanceWidth("M")
    readonly property real cellH: Math.ceil(metrics.height)
    readonly property int cols: Math.max(2, Math.floor((width - 12) / cellW))
    readonly property int rowCount: Math.max(1, Math.floor(height / cellH))

    clip: true
    activeFocusOnTab: true

    FontMetrics {
        id: metrics
        font.family: Backend.termFamily
        font.pixelSize: term.size
    }

    ListModel {
        id: lines
    }

    function parseAt(at) {
        // RFC 3339 with up to nine fraction digits; JavaScript reads three.
        const m = String(at || "").match(/^(.*T\d\d:\d\d:\d\d)(\.\d+)?(.*)$/);
        if (!m)
            return Date.now();
        return Date.parse(m[1] + (m[2] ? m[2].slice(0, 4) : "") + m[3]);
    }
    // The words of one shown row, for checks and automation.
    function rowText(y) {
        if (y < 0 || y >= lines.count)
            return "";
        return JSON.parse(lines.get(y).runs).map(r => r.text).join("").replace(/\s+$/, "");
    }
    function setRow(row) {
        const runs = JSON.stringify(row.runs);
        if (row.y < lines.count) {
            if (lines.get(row.y).runs !== runs)
                lines.setProperty(row.y, "runs", runs);
        } else {
            while (lines.count < row.y)
                lines.append({
                    runs: "[]"
                });
            lines.append({
                runs: runs
            });
        }
    }
    function full(screen) {
        const t0 = Date.now();
        while (lines.count > screen.rows.length)
            lines.remove(lines.count - 1);
        for (let i = 0; i < screen.rows.length; i++)
            setRow(screen.rows[i]);
        cursor = screen.cursor;
        scrollbackLen = screen.scrollback_len;
        if (screen.modes)
            modes = screen.modes;
        pendingApply = t0;
    }
    function damage(data, at) {
        if (identityChanged(data)) {
            reattach();
            return; // A new generation needs its full attach frame before damage.
        }
        const t0 = Date.now();
        if (data.reset)
            while (lines.count > data.rows.length)
                lines.remove(lines.count - 1);
        for (let i = 0; i < data.rows.length; i++)
            setRow(data.rows[i]);
        cursor = data.cursor;
        scrollbackLen = data.scrollback_len;
        // A full-screen update: a reset, or nine rows in ten redrawn.
        if (data.reset || data.rows.length * 10 >= rowCount * 9) {
            pendingAt = parseAt(at);
            pendingApply = t0;
        }
    }
    function frameSwapped() {
        if (pendingAt > 0) {
            const now = Date.now();
            lastFrameMs = now - pendingAt;
            lastApplyMs = now - pendingApply;
            worstFrameMs = Math.max(worstFrameMs, lastApplyMs);
            worstStampMs = Math.max(worstStampMs, lastFrameMs);
            fullFrames += 1;
            pendingAt = 0;
        }
    }

    function attach() {
        if (viewer.length > 0 || attaching || !live)
            return;
        retryTimer.stop();
        attaching = true;
        const epoch = ++attachEpoch;
        const want = pane;
        Backend.request("term.pane.attach", {
            pane: want,
            cols: cols,
            rows: rowCount
        }, (ok, r) => {
            // A late reply belongs to its original pane/connection, never the current selection.
            if (epoch !== term.attachEpoch || want !== term.pane || !term.live) {
                if (ok)
                    Backend.request("term.pane.detach", { viewer: r.viewer });
                return;
            }
            term.attaching = false;
            if (!ok) {
                term.scheduleRetry(); // Includes timeout and offline replies.
                return;
            }
            if (term.identityChanged(r)) {
                Backend.request("term.pane.detach", { viewer: r.viewer });
                term.reattach();
                return;
            }
            term.retryStep = 0;
            term.viewer = r.viewer;
            Backend.addViewer(term.viewer, term);
            term.full(r.screen);
            term.resize();
            // Back to the place the reader left, moved by the history that arrived meanwhile.
            if (term.savedOffset > 0)
                term.scrollTo(term.savedOffset + Math.max(0, r.screen.scrollback_len - term.savedHistory));
        }, 2000);
    }
    function detach() {
        attachEpoch += 1;
        attaching = false;
        retryTimer.stop();
        if (viewer.length === 0)
            return;
        savedOffset = offset;
        savedHistory = scrollbackLen;
        Backend.request("term.pane.detach", {
            viewer: viewer
        });
        Backend.dropViewer(viewer);
        viewer = "";
    }
    function resize() {
        if (viewer.length > 0)
            Backend.request("term.pane.resize", {
                viewer: viewer,
                cols: cols,
                rows: rowCount
            });
    }
    function scrollTo(n) {
        if (viewer.length === 0)
            return;
        const want = Math.max(0, Math.min(scrollbackLen, Math.round(n)));
        if (want === offset)
            return;
        offset = want;
        Backend.request("term.pane.scroll", {
            viewer: viewer,
            offset: want
        }, (ok, r) => {
            if (!ok)
                return;
            term.offset = r.offset;
            term.full(r.screen);
        });
    }

    function send(text) {
        if (text.length === 0 || pane.length === 0)
            return;
        Backend.request("term.pane.input", {
            pane: pane,
            bytes_b64: Qt.btoa(unescape(encodeURIComponent(text)))
        });
    }
    function paste(text) {
        if (!text)
            return;
        send(modes.bracketed_paste ? "\x1b[200~" + text + "\x1b[201~" : text);
    }
    // Keys as a terminal sends them, honouring the pane's cursor mode.
    function encode(event) {
        const ctrl = event.modifiers & Qt.ControlModifier, alt = event.modifiers & Qt.AltModifier;
        const app = modes.application_cursor === true;
        const csi = (c) => (app ? "\x1bO" : "\x1b[") + c;
        let s = "";
        switch (event.key) {
        case Qt.Key_Return:
        case Qt.Key_Enter:
            s = "\r";
            break;
        case Qt.Key_Backspace:
            s = ctrl ? "\x08" : "\x7f";
            break;
        case Qt.Key_Tab:
            s = "\t";
            break;
        case Qt.Key_Backtab:
            s = "\x1b[Z";
            break;
        case Qt.Key_Escape:
            s = "\x1b";
            break;
        case Qt.Key_Up:
            s = csi("A");
            break;
        case Qt.Key_Down:
            s = csi("B");
            break;
        case Qt.Key_Right:
            s = csi("C");
            break;
        case Qt.Key_Left:
            s = csi("D");
            break;
        case Qt.Key_Home:
            s = csi("H");
            break;
        case Qt.Key_End:
            s = csi("F");
            break;
        case Qt.Key_Insert:
            s = "\x1b[2~";
            break;
        case Qt.Key_Delete:
            s = "\x1b[3~";
            break;
        case Qt.Key_PageUp:
            s = "\x1b[5~";
            break;
        case Qt.Key_PageDown:
            s = "\x1b[6~";
            break;
        default:
            if (ctrl && event.key >= Qt.Key_A && event.key <= Qt.Key_Z)
                s = String.fromCharCode(event.key - Qt.Key_A + 1);
            else if (ctrl && event.key === Qt.Key_Space)
                s = "\x00";
            else
                s = event.text;
        }
        return s.length > 0 && alt ? "\x1b" + s : s;
    }

    Keys.onPressed: event => {
        const shift = event.modifiers & Qt.ShiftModifier, ctrl = event.modifiers & Qt.ControlModifier;
        if (shift && event.key === Qt.Key_PageUp) {
            scrollTo(offset + rowCount - 1);
        } else if (shift && event.key === Qt.Key_PageDown) {
            scrollTo(offset - rowCount + 1);
        } else if ((ctrl && shift && event.key === Qt.Key_V) || (shift && event.key === Qt.Key_Insert)) {
            paste(Quickshell.clipboardText);
        } else {
            const s = encode(event);
            if (s.length === 0)
                return;
            if (offset > 0)
                scrollTo(0);
            send(s);
        }
        event.accepted = true;
    }

    onLiveChanged: live ? attach() : detach()
    onPaneChanged: {
        detach();
        savedOffset = 0;
        offset = 0;
        generation = null;
        startedAt = "";
        retryStep = 0;
        // A replacement pane keeps the previous frame until its full attach reply.
        adoptIdentity();
        attach();
    }
    onColsChanged: resizeTimer.restart()
    onRowCountChanged: resizeTimer.restart()
    onActiveFocusChanged: if (activeFocus)
        Backend.focusedPane = pane
    Component.onCompleted: attach()
    Component.onDestruction: detach()

    // Failed attaches retry after 250, 500, 1000, 2000 ms, then every 2000 ms.
    Timer {
        id: retryTimer
        onTriggered: term.attach()
    }
    Connections {
        target: Backend
        function onRefreshed() {
            term.reattach();
        }
        function onReconnected() {
            term.reattach();
        }
    }
    Connections {
        target: term.Window.window
        function onFrameSwapped() {
            term.frameSwapped();
        }
    }
    // Resize once the drag rests 100 ms.
    Timer {
        id: resizeTimer
        interval: 100
        onTriggered: term.resize()
    }

    Rectangle {
        anchors.fill: parent
        color: Theme.sunken
    }

    Item {
        id: grid
        x: 6
        width: term.cols * term.cellW
        height: term.rowCount * term.cellH
        opacity: term.stale ? 0.45 : 1

        Repeater {
            model: lines
            delegate: Row {
                id: line
                required property string runs
                required property int index
                readonly property var parts: JSON.parse(runs)
                y: index * term.cellH
                height: term.cellH
                Repeater {
                    model: line.parts
                    delegate: Rectangle {
                        id: run
                        required property var modelData
                        readonly property bool inverse: modelData.fg === "default_background"
                        width: modelData.text.length * term.cellW
                        height: term.cellH
                        color: Theme.indexedBackground(modelData.bg)
                        Text {
                            text: run.modelData.text
                            color: Theme.indexedForeground(run.modelData.fg, run.modelData.bold)
                            font.family: Backend.termFamily
                            font.pixelSize: term.size
                            font.bold: run.modelData.bold
                            font.italic: run.modelData.italic
                            font.underline: run.modelData.underline
                            textFormat: Text.PlainText
                        }
                    }
                }
            }
        }

        Rectangle {
            visible: term.cursor.visible && term.offset === 0
            x: term.cursor.x * term.cellW
            y: term.cursor.y * term.cellH
            width: term.cellW
            height: term.cellH
            color: term.activeFocus ? Theme.foreground : "transparent"
            opacity: term.activeFocus ? 0.6 : 1
            border { width: 1; color: Theme.foreground }
        }
    }

    // History bar: where this viewer reads in its pane's history.
    Rectangle {
        visible: term.scrollbackLen > 0
        anchors.right: parent.right
        width: 3
        radius: 1
        color: Theme.quiet
        opacity: 0.5
        readonly property real total: term.scrollbackLen + term.rowCount
        height: Math.max(24, term.height * term.rowCount / total)
        y: (term.height - height) * (1 - term.offset / Math.max(1, term.scrollbackLen))
    }

    MouseArea {
        anchors.fill: parent
        acceptedButtons: Qt.LeftButton | Qt.MiddleButton
        cursorShape: Qt.IBeamCursor
        onPressed: mouse => {
            term.forceActiveFocus();
            if (mouse.button === Qt.MiddleButton)
                term.paste(Quickshell.clipboardText);
        }
        onWheel: wheel => {
            const steps = Math.round(wheel.angleDelta.y / 40);
            if (term.modes.alternate_screen) {
                // A full-screen program reads the wheel itself when it asked for the mouse.
                if (term.modes.mouse !== "none" && term.modes.mouse_encoding === "sgr") {
                    const c = Math.floor((wheel.x - 6) / term.cellW) + 1, r = Math.floor(wheel.y / term.cellH) + 1;
                    term.send(("\x1b[<" + (steps > 0 ? 64 : 65) + ";" + c + ";" + r + "M").repeat(Math.abs(steps)));
                }
                return;
            }
            term.scrollTo(term.offset + steps);
        }
    }

    Rectangle {
        visible: term.offset > 0
        anchors.fill: newest
        radius: Theme.radius
        color: Theme.surface
    }
    Button {
        id: newest
        visible: term.offset > 0
        anchors { right: parent.right; bottom: parent.bottom; margins: 8 }
        word: "Newest"
        onClicked: term.scrollTo(0)
    }

    Text {
        visible: term.stale && lines.count > 0
        anchors { left: parent.left; bottom: parent.bottom; margins: 8 }
        text: Words.stale
        color: Theme.quiet
        font { family: Theme.font; pixelSize: Theme.small }
    }
}
