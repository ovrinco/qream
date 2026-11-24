pragma ComponentBehavior: Bound
import QtQuick
import ".."

// The orchestrator's seat as a column docked beside the campaign panels: a header like a
// campaign header acting on the seat's own binding, the empty seat's one act, Invoke, and
// the conversation in words with the terminal as a toggle. Every word is the engine's.
Rectangle {
    id: column
    // The seat binds to the campaign the operator chose: the one picked in this header, else the
    // window's selection; only a registered campaign still running counts, never the first panel.
    readonly property string campaign: column.registered(Backend.seatCampaign) ? Backend.seatCampaign : (column.registered(Backend.selectedCampaign) ? Backend.selectedCampaign : "")
    readonly property string campaignTitle: campaign ? (Backend.campaign(campaign) || {}).title || campaign.replace(/[-_]/g, " ") : ""
    property var seat: null
    property double seatHeard: 0
    property int seatAge: 0
    property var harnesses: []
    property string viewWord: "Chat"
    property string confirming: ""
    property bool invoking: false
    property bool resuming: false
    property string kind: ""
    property string model: ""
    property string effort: ""
    property string resumeId: ""
    property var recent: []
    property string stamp: ""
    property int shownTotal: 0
    property string screenText: ""
    property bool busy: false
    property string notice: ""

    readonly property string word: seat ? seat.word : ""
    readonly property var binding: seat ? seat.binding : null
    readonly property bool empty: !seat || word === "Empty" || word === "Stopped"
    readonly property bool agent: !!binding && !!binding.harness && !empty
    readonly property var chosen: harnesses.find(h => h.kind === kind) || null
    // A side is chosen in Settings; the drag handle sits on the inner edge.
    readonly property bool onLeft: Backend.orchestratorSide === "Left"

    color: Theme.background
    border { width: 1; color: Theme.border }
    clip: true

    // A session's first line in words: a path keeps only its last name, as nowhere here shows paths.
    function plain(line) {
        return (line || "").replace(/(?:~|\/)?(?:[\w.@-]*\/)+([\w.@-]*)/g, "$1");
    }
    function registered(name) {
        return !!name && Backend.live.some(c => c.name === name);
    }
    function say(line) {
        notice = line;
        noticeTimer.restart();
    }
    function wordColour(w) {
        if (w === "Working")
            return Theme.blue;
        if (w.indexOf("Blocked") === 0)
            return Theme.red;
        if (w === "Idle")
            return Theme.yellow;
        if (w === "Shell" || w === "Starting")
            return Theme.cyan;
        return Theme.muted;
    }
    function refresh() {
        if (!campaign) {
            seat = null;
            return;
        }
        const asked = campaign;
        Backend.request("seat.get", {
            name: asked
        }, (ok, result) => {
            if (!ok || asked !== column.campaign)
                return;
            const was = column.binding ? column.binding.conversation : null;
            column.seat = result;
            column.seatHeard = Date.now();
            column.seatAge = 0;
            const now = result.binding ? result.binding.conversation : null;
            if (was !== now)
                column.resetChat();
        });
    }
    function openSeat() {
        if (!campaign)
            return;
        busy = true;
        Backend.request("seat.open", {
            name: campaign
        }, (ok, result) => {
            column.busy = false;
            if (!ok) {
                column.say(result.message || "");
                return;
            }
            column.seat = result;
            column.seatHeard = Date.now();
            column.resetChat();
            column.viewWord = "Terminal";
            column.readScreen();
        });
    }
    function startInvoke() {
        invoking = true;
        Backend.request("seat.harnesses", {}, (ok, result) => {
            if (!ok)
                return;
            column.harnesses = result.filter(h => h.installed);
            if (!column.chosen && column.harnesses.length > 0)
                column.pickHarness(column.harnesses[0].kind);
        });
    }
    function pickHarness(k) {
        kind = k;
        const h = harnesses.find(x => x.kind === k);
        model = h && h.models.length > 0 ? h.models[0] : "";
        effort = h && h.efforts.length > 0 ? h.efforts[Math.min(1, h.efforts.length - 1)] : "";
        resumeId = "";
        recent = [];
        if (resuming)
            loadRecent();
    }
    function loadRecent() {
        Backend.request("sessions.recent", {
            kind: kind,
            limit: 20
        }, (ok, result) => {
            if (ok)
                column.recent = result;
        });
    }
    function invoke() {
        if (!campaign)
            return;
        const args = {
            name: campaign,
            kind: kind
        };
        if (model)
            args.model = model;
        if (effort)
            args.effort = effort;
        if (resuming && resumeId.trim().length > 0)
            args.resume = resumeId.trim();
        busy = true;
        Backend.request("seat.invoke", args, (ok, result) => {
            column.busy = false;
            if (!ok) {
                column.say(result.message || "");
                return;
            }
            column.seat = result;
            column.invoking = false;
            column.resuming = false;
            column.resetChat();
            column.viewWord = "Chat";
        });
    }
    function control(action) {
        Backend.request("campaign.control", {
            name: campaign,
            action: action,
            target: "orchestrator"
        }, (ok, result) => {
            if (!ok) {
                column.say(result.message || "");
                return;
            }
            column.seat = result;
            column.seatHeard = Date.now();
        });
    }
    function resetChat() {
        turns.clear();
        shownTotal = 0;
        stamp = "";
    }
    function readChat() {
        if (!agent)
            return;
        Backend.request("conversation.get", {
            name: campaign,
            stamp: stamp
        }, (ok, result) => {
            if (!ok || result.unchanged)
                return;
            column.stamp = result.stamp || "";
            const total = result.total || 0;
            const list = result.turns || [];
            if (total < column.shownTotal) {
                turns.clear();
                column.shownTotal = 0;
            }
            const fresh = column.shownTotal === 0 ? list : list.slice(Math.max(0, list.length - (total - column.shownTotal)));
            const atEnd = chat.atYEnd || turns.count === 0;
            for (let i = 0; i < fresh.length; i++)
                turns.append({
                    role: fresh[i].role,
                    words: fresh[i].text,
                    at: fresh[i].at
                });
            column.shownTotal = total;
            // The reader's place holds while output arrives; only a reader at the end follows it.
            if (atEnd)
                chat.positionViewAtEnd();
        });
    }
    function readScreen() {
        if (!binding || empty)
            return;
        Backend.request("term.pane.text", {
            pane: binding.pane,
            lines: 2000
        }, (ok, result) => {
            if (!ok || result.text === column.screenText)
                return;
            const atEnd = screen.atYEnd || column.screenText.length === 0;
            const keep = screen.contentY;
            column.screenText = result.text;
            if (atEnd)
                screen.contentY = Math.max(0, screen.contentHeight - screen.height);
            else
                screen.contentY = keep;
        });
    }
    function encode(text) {
        return Qt.btoa(unescape(encodeURIComponent(text)));
    }
    // The operator's own typing goes to the seat's pane as keystrokes; Enter follows a moment
    // later so a harness does not take it as part of a paste.
    property string pendingPane: ""
    function send(text) {
        if (!binding || empty || text.length === 0)
            return;
        const pane = binding.pane;
        Backend.request("term.pane.input", {
            pane: pane,
            bytes_b64: encode(text)
        }, (ok, result) => {
            if (!ok) {
                column.say(result.message || "");
                return;
            }
            entry.text = "";
            column.pendingPane = pane;
            enterTimer.restart();
        });
    }
    Timer {
        id: enterTimer
        interval: 150
        onTriggered: Backend.request("term.pane.input", {
            pane: column.pendingPane,
            bytes_b64: column.encode("\r")
        })
    }
    // The campaigns whose orchestrator is active now (campaign up, seat held by a harness), in
    // arrival order: the only ones the header's list offers (OD-2026-10-12-09-R01).
    property var seated: []
    property bool listOpen: false
    function readSeated() {
        const up = Backend.live.filter(c => c.status === "up");
        const found = {};
        let left = up.length;
        if (left === 0) {
            seated = [];
            return;
        }
        up.forEach(c => Backend.request("seat.get", {
                name: c.name
            }, (ok, result) => {
                if (ok && result && result.binding && result.binding.harness && result.word !== "Empty" && result.word !== "Stopped")
                    found[c.name] = true;
                left -= 1;
                if (left === 0)
                    column.seated = up.filter(u => found[u.name]).map(u => ({
                                name: u.name,
                                title: u.title || u.name.replace(/[-_]/g, " ")
                            }));
            }));
    }
    function pickCampaign(name) {
        listOpen = false;
        if (name === campaign)
            return;
        Backend.seatCampaign = name;
        seat = null;
        resetChat();
        refresh();
    }
    Timer {
        interval: 3000
        running: column.visible
        repeat: true
        triggeredOnStart: true
        onTriggered: column.readSeated()
    }

    onCampaignChanged: refresh()
    Component.onCompleted: refresh()

    Timer {
        interval: 1000
        running: column.visible
        repeat: true
        onTriggered: {
            column.refresh();
            if (column.seatHeard > 0)
                column.seatAge = Math.floor((Date.now() - column.seatHeard) / 1000);
        }
    }
    Timer {
        interval: 700
        running: column.visible && !column.empty
        repeat: true
        onTriggered: column.viewWord === "Chat" && column.agent ? column.readChat() : column.readScreen()
    }
    Timer {
        id: confirmTimer
        interval: 4000
        onTriggered: column.confirming = ""
    }
    Timer {
        id: noticeTimer
        interval: 5000
        onTriggered: column.notice = ""
    }

    ListModel { id: turns }

    // Header: like a campaign header, acting on the seat's binding only.
    Rectangle {
        id: head
        anchors { left: parent.left; right: parent.right; top: parent.top; margins: 1 }
        height: Math.max(34, line.implicitHeight + 12)
        color: Theme.surface

        Flow {
            id: line
            anchors { left: parent.left; leftMargin: 10; right: parent.right; rightMargin: 10; verticalCenter: parent.verticalCenter }
            spacing: 10
            Text {
                text: "Orchestrator"
                color: Theme.foreground
                font { family: Theme.font; pixelSize: Theme.normal; weight: Font.DemiBold }
            }
            MouseArea {
                id: campaignPick
                width: campaignWord.implicitWidth
                height: campaignWord.implicitHeight
                hoverEnabled: true
                cursorShape: column.seated.length > 0 ? Qt.PointingHandCursor : Qt.ArrowCursor
                onClicked: if (column.seated.length > 0)
                    column.listOpen = !column.listOpen
                Text {
                    id: campaignWord
                    text: (column.campaignTitle || (column.seated.length > 0 ? "Choose a campaign" : Words.noCampaign)) + (column.seated.length > 0 ? "  ▾" : "")
                    color: campaignPick.containsMouse && column.seated.length > 0 ? Theme.foreground : Theme.quiet
                    font { family: Theme.font; pixelSize: Theme.normal }
                }
            }
            Text {
                visible: column.agent
                text: column.seat && column.seat.harness_words ? column.seat.harness_words + (column.binding.model ? " · " + column.binding.model : "") + (column.binding.effort ? " · " + column.binding.effort : "") : ""
                color: Theme.quiet
                font { family: Theme.font; pixelSize: Theme.normal }
            }
            Text {
                visible: column.word.length > 0
                text: column.word
                color: column.wordColour(column.word)
                font { family: Theme.font; pixelSize: Theme.normal; weight: Font.DemiBold }
            }
            Text {
                visible: (column.seatHeard > 0 && column.seatAge >= 15) || Backend.stale
                text: Words.age(Math.max(column.seatAge, Backend.ageSeconds))
                color: Theme.quiet
                font { family: Theme.font; pixelSize: Theme.small }
            }
            Row {
                spacing: 6
                visible: !column.empty
                Repeater {
                    model: ["Interrupt", column.word === "Paused" ? "Release" : "Pause", "Kill"]
                    delegate: Button {
                        required property string modelData
                        word: column.confirming === modelData ? "Confirm" : modelData
                        tone: modelData === "Kill" ? Theme.red : Theme.foreground
                        hint: modelData + " the orchestrator"
                        onClicked: {
                            if (modelData === "Kill" && column.confirming !== "Kill") {
                                column.confirming = "Kill";
                                confirmTimer.restart();
                                return;
                            }
                            column.confirming = "";
                            column.control(modelData.toLowerCase());
                        }
                    }
                }
            }
        }
    }

    // The empty seat: one act.
    Column {
        anchors.centerIn: parent
        width: parent.width - 48
        spacing: 10
        visible: column.empty
        Button {
            anchors.horizontalCenter: parent.horizontalCenter
            visible: !!column.campaign
            word: "Open the orchestrator's terminal"
            enabledWord: Backend.connected && !column.busy
            onClicked: if (enabledWord)
                column.openSeat()
        }
        Text {
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.WordWrap
            visible: !column.campaign
            text: Backend.live.length > 0 ? "No campaign chosen: choose one in the header to seat its orchestrator" : "No campaign yet: register one to seat its orchestrator"
            color: Theme.quiet
            font { family: Theme.font; pixelSize: Theme.small }
        }
        Text {
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.WordWrap
            visible: !Backend.connected
            text: Words.lost
            color: Theme.quiet
            font { family: Theme.font; pixelSize: Theme.small }
        }
    }

    // The open seat: the view bar, Invoke while it is a plain shell, the view, the entry box.
    Item {
        id: body
        anchors { left: parent.left; right: parent.right; top: head.bottom; bottom: parent.bottom; margins: 8 }
        visible: !column.empty && !!column.campaign

        Row {
            id: viewBar
            anchors { right: parent.right; top: parent.top }
            spacing: 4
            Button {
                visible: column.word === "Shell"
                word: "Invoke"
                selected: column.invoking
                onClicked: column.invoking ? column.invoking = false : column.startInvoke()
            }
            Repeater {
                model: ["Chat", "Terminal"]
                delegate: Button {
                    required property string modelData
                    word: modelData
                    selected: column.viewWord === modelData
                    onClicked: {
                        column.viewWord = modelData;
                        if (modelData === "Terminal")
                            column.readScreen();
                        else
                            column.readChat();
                    }
                }
            }
        }

        // Invoke: harness (installed only), model, effort and a session to resume.
        Rectangle {
            id: invokeBox
            anchors { left: parent.left; right: parent.right; top: viewBar.bottom; topMargin: 6 }
            visible: column.invoking && column.word === "Shell"
            height: visible ? choices.implicitHeight + 16 : 0
            color: Theme.surface
            radius: Theme.radius
            border { width: 1; color: Theme.border }
            Column {
                id: choices
                anchors { left: parent.left; right: parent.right; top: parent.top; margins: 8 }
                spacing: 8
                Flow {
                    width: parent.width
                    spacing: 6
                    Repeater {
                        model: column.harnesses
                        delegate: Button {
                            required property var modelData
                            word: modelData.words
                            selected: column.kind === modelData.kind
                            onClicked: column.pickHarness(modelData.kind)
                        }
                    }
                }
                Flow {
                    width: parent.width
                    spacing: 6
                    visible: !!column.chosen && column.chosen.models.length > 0
                    Repeater {
                        model: column.chosen ? column.chosen.models : []
                        delegate: Button {
                            required property string modelData
                            word: modelData
                            selected: column.model === modelData
                            onClicked: column.model = modelData
                        }
                    }
                }
                Flow {
                    width: parent.width
                    spacing: 6
                    visible: !!column.chosen && column.chosen.efforts.length > 0
                    Repeater {
                        model: column.chosen ? column.chosen.efforts : []
                        delegate: Button {
                            required property string modelData
                            word: modelData
                            selected: column.effort === modelData
                            onClicked: column.effort = modelData
                        }
                    }
                }
                Button {
                    visible: !!column.chosen && column.chosen.resumes
                    word: "Resume a session"
                    selected: column.resuming
                    onClicked: {
                        column.resuming = !column.resuming;
                        if (column.resuming)
                            column.loadRecent();
                    }
                }
                ListView {
                    id: recentList
                    visible: column.resuming
                    width: parent.width
                    height: visible ? Math.min(contentHeight, 200) : 0
                    clip: true
                    boundsBehavior: Flickable.StopAtBounds
                    model: column.recent
                    delegate: MouseArea {
                        id: recentRow
                        required property var modelData
                        width: recentList.width
                        height: recentWords.implicitHeight + 8
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: column.resumeId = modelData.id
                        Rectangle {
                            anchors.fill: parent
                            radius: Theme.radius
                            color: column.resumeId === recentRow.modelData.id || recentRow.containsMouse ? Theme.hover : "transparent"
                        }
                        Text {
                            id: recentWords
                            anchors { left: parent.left; right: parent.right; verticalCenter: parent.verticalCenter; leftMargin: 6; rightMargin: 6 }
                            text: Words.hhmm(recentRow.modelData.at) + "  " + column.plain(recentRow.modelData.first_line)
                            elide: Text.ElideRight
                            color: Theme.foreground
                            font { family: Theme.font; pixelSize: Theme.small }
                        }
                    }
                }
                Rectangle {
                    visible: column.resuming
                    width: parent.width
                    height: 26
                    radius: Theme.radius
                    color: Theme.sunken
                    border { width: 1; color: Theme.border }
                    TextInput {
                        id: idEntry
                        anchors { fill: parent; leftMargin: 6; rightMargin: 6 }
                        verticalAlignment: TextInput.AlignVCenter
                        text: column.resumeId
                        onTextEdited: column.resumeId = text
                        clip: true
                        color: Theme.foreground
                        font { family: Theme.mono; pixelSize: Theme.small }
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            visible: idEntry.text.length === 0
                            text: "Or type a session id"
                            color: Theme.quiet
                            font { family: Theme.font; pixelSize: Theme.small }
                        }
                    }
                }
                Button {
                    word: "Invoke"
                    enabledWord: !!column.chosen && !column.busy && (!column.resuming || column.resumeId.trim().length > 0)
                    onClicked: if (enabledWord)
                        column.invoke()
                }
            }
        }

        Rectangle {
            id: frame
            anchors { left: parent.left; right: parent.right; top: invokeBox.visible ? invokeBox.bottom : viewBar.bottom; topMargin: 6; bottom: entryBox.top; bottomMargin: 6 }
            color: Theme.sunken
            radius: Theme.radius
            border { width: 1; color: Theme.border }
            clip: true

            // Chat: the turns in words; long lines wrap; the place holds while output arrives.
            ListView {
                id: chat
                anchors { fill: parent; margins: 8 }
                visible: column.viewWord === "Chat"
                model: turns
                spacing: 10
                clip: true
                flickableDirection: Flickable.VerticalFlick
                boundsBehavior: Flickable.StopAtBounds
                delegate: Column {
                    id: turn
                    required property string role
                    required property string words
                    width: chat.width
                    spacing: 2
                    Text {
                        visible: turn.role !== "tool"
                        text: turn.role === "user" ? "You" : "Orchestrator"
                        color: turn.role === "user" ? Theme.citium : Theme.quiet
                        font { family: Theme.font; pixelSize: Theme.small; weight: Font.DemiBold }
                    }
                    Text {
                        width: parent.width
                        text: turn.words
                        wrapMode: Text.Wrap
                        textFormat: Text.PlainText
                        color: turn.role === "tool" ? Theme.quiet : Theme.foreground
                        font { family: Theme.font; pixelSize: turn.role === "tool" ? Theme.small : Theme.normal; italic: turn.role === "tool" }
                    }
                }
            }
            Text {
                anchors.centerIn: parent
                width: parent.width - 32
                horizontalAlignment: Text.AlignHCenter
                wrapMode: Text.WordWrap
                visible: column.viewWord === "Chat" && turns.count === 0
                text: column.agent ? "The conversation shows here once it has words" : "No orchestrator yet: Invoke one, or use the terminal"
                color: Theme.quiet
                font { family: Theme.font; pixelSize: Theme.small }
            }

            // Terminal: the pane's text until the pane renderer joins this frame.
            Flickable {
                id: screen
                anchors { fill: parent; margins: 8 }
                visible: column.viewWord === "Terminal"
                clip: true
                contentWidth: width
                contentHeight: screenWords.implicitHeight
                flickableDirection: Flickable.VerticalFlick
                boundsBehavior: Flickable.StopAtBounds
                Text {
                    id: screenWords
                    width: screen.width
                    text: column.screenText
                    wrapMode: Text.WrapAnywhere
                    textFormat: Text.PlainText
                    color: Theme.foreground
                    font { family: Theme.mono; pixelSize: 13 }
                }
            }

            Button {
                anchors { right: parent.right; bottom: parent.bottom; margins: 8 }
                visible: column.viewWord === "Chat" ? (turns.count > 0 && !chat.atYEnd) : (column.screenText.length > 0 && !screen.atYEnd)
                word: "Newest"
                onClicked: column.viewWord === "Chat" ? chat.positionViewAtEnd() : screen.contentY = Math.max(0, screen.contentHeight - screen.height)
            }
        }

        // Reading and typing share this view: the box under the conversation or the terminal.
        Rectangle {
            id: entryBox
            anchors { left: parent.left; right: parent.right; bottom: noticeLine.top }
            height: Math.max(30, entry.contentHeight + 12)
            radius: Theme.radius
            color: Theme.surface
            border { width: 1; color: entry.activeFocus ? Theme.citium : Theme.border }
            TextEdit {
                id: entry
                anchors { fill: parent; margins: 6 }
                wrapMode: TextEdit.Wrap
                color: Theme.foreground
                font { family: column.viewWord === "Terminal" ? Theme.mono : Theme.font; pixelSize: Theme.normal }
                Keys.onReturnPressed: event => {
                    if (event.modifiers & Qt.ShiftModifier) {
                        event.accepted = false;
                        return;
                    }
                    column.send(entry.text);
                }
                Text {
                    visible: entry.text.length === 0 && !entry.activeFocus
                    text: column.agent && column.viewWord === "Chat" ? "Write to the orchestrator" : "Type a command"
                    color: Theme.quiet
                    font: entry.font
                }
            }
        }
        Text {
            id: noticeLine
            anchors { left: parent.left; right: parent.right; bottom: parent.bottom }
            visible: text.length > 0
            height: visible ? contentHeight + 4 : 0
            verticalAlignment: Text.AlignBottom
            text: column.notice
            wrapMode: Text.WordWrap
            color: Theme.quiet
            font { family: Theme.font; pixelSize: Theme.small }
        }
    }

    // The column's own width, dragged at its inner edge and kept across restarts.
    MouseArea {
        id: grip
        anchors { top: parent.top; bottom: parent.bottom }
        x: column.onLeft ? column.width - width : 0
        width: 6
        cursorShape: Qt.SplitHCursor
        property real startX: 0
        onPressed: mouse => startX = mouse.x
        onPositionChanged: mouse => {
            if (!pressed)
                return;
            const delta = column.onLeft ? mouse.x - startX : startX - mouse.x;
            Backend.seatWidth = Math.max(300, Math.min(1000, Backend.seatWidth + delta));
        }
    }

    // Automation for checks and captures, called through the window's IPC handler.
    function scrollBy(pixels) {
        const view = viewWord === "Chat" ? chat : screen;
        view.contentY = Math.max(0, Math.min(view.contentHeight - view.height, view.contentY + pixels));
        return view.contentY;
    }
    function position() {
        const view = viewWord === "Chat" ? chat : screen;
        return JSON.stringify({
            view: viewWord,
            contentY: view.contentY,
            atEnd: view.atYEnd,
            turns: turns.count,
            word: word,
            pane: binding ? binding.pane : "",
            notice: notice
        });
    }
    function choose(k, m, e, resume) {
        startInvoke();
        kind = k;
        model = m;
        effort = e;
        resuming = resume.length > 0;
        resumeId = resume;
        if (resuming)
            loadRecent();
    }

    // The header's list of campaigns with an active orchestrator; a click elsewhere closes it.
    MouseArea {
        anchors.fill: parent
        visible: column.listOpen
        z: 9
        onClicked: column.listOpen = false
    }
    Rectangle {
        id: campaignList
        visible: column.listOpen && column.seated.length > 0
        z: 10
        x: 10
        y: head.y + head.height + 2
        width: Math.min(column.width - 20, Math.max(220, listColumn.implicitWidth + 2))
        height: listColumn.implicitHeight + 2
        radius: Theme.radius
        color: Theme.surface
        border { width: 1; color: Theme.border }
        Column {
            id: listColumn
            x: 1
            y: 1
            width: parent.width - 2
            Repeater {
                model: column.seated
                delegate: MouseArea {
                    id: campaignEntry
                    required property var modelData
                    width: listColumn.width
                    implicitWidth: entryWord.implicitWidth + 24
                    height: entryWord.implicitHeight + 12
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: column.pickCampaign(campaignEntry.modelData.name)
                    Rectangle {
                        anchors.fill: parent
                        color: campaignEntry.containsMouse || campaignEntry.modelData.name === column.campaign ? Theme.hover : "transparent"
                    }
                    Text {
                        id: entryWord
                        anchors { left: parent.left; leftMargin: 12; verticalCenter: parent.verticalCenter }
                        text: campaignEntry.modelData.title
                        color: campaignEntry.modelData.name === column.campaign ? Theme.foreground : Theme.quiet
                        font { family: Theme.font; pixelSize: Theme.normal; weight: campaignEntry.modelData.name === column.campaign ? Font.DemiBold : Font.Normal }
                    }
                }
            }
        }
    }
}
