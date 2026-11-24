pragma ComponentBehavior: Bound
import QtQuick
import Quickshell.Io
import ".."

// One campaign on Execution: its header (one truth from `states`), the wave tree and the
// terminal area. Every word and count is the engine's; the panel only lays them out.
Rectangle {
    id: panel
    property string name
    readonly property var cstate: Backend.states[name] || null
    readonly property string refusal: Backend.refusals[name] || ""
    property string selected: ""
    readonly property string selectedTitle: {
        const t = cstate && cstate.tasks ? cstate.tasks.find(t => t.task === selected) : null;
        return t ? Words.title(t) : "";
    }
    property string confirming: ""
    // A Prepared campaign's scope: the tracker rows and outcome groups built into it, in plain words.
    readonly property bool prepared: !!cstate && cstate.word === "prepared"
    property var scope: null
    property bool scopeAll: false
    function loadScope() {
        if (!prepared)
            return;
        Backend.request("campaign.scope", {
            campaign: name
        }, (ok, r) => scope = ok ? r : {
                items: [],
                message: "The tracker items of this campaign cannot be read now"
            });
    }
    onPreparedChanged: loadScope()
    // The tree keeps about thirty-four characters at the default size; a panel too narrow for the
    // tree and a terminal stacks them.
    readonly property real treeMin: treeChars.advanceWidth("abcdefghijklmnopqrstuvwxyzabcdefgh") + 16
    readonly property bool stacked: width < treeMin + 360
    FontMetrics { id: treeChars; font.family: Theme.font; font.pixelSize: Theme.normal }

    // How old what the window shows is: the time since the engine last answered. The controller's
    // last decision time is not freshness (a quiet controller over live workers read "1441 s ago").
    readonly property int age: cstate ? Backend.ageSeconds : 0

    color: Theme.background
    radius: Theme.radius
    border { width: 1; color: Theme.border }
    clip: true

    // Header: name, campaign word, wave, counts or Finished, close line, controls.
    Rectangle {
        id: head
        anchors { left: parent.left; right: parent.right; top: parent.top; margins: 1 }
        height: Math.max(34, line.implicitHeight + 12)
        color: Theme.surface
        radius: Theme.radius

        Flow {
            id: line
            anchors { left: parent.left; leftMargin: 10; right: parent.right; rightMargin: 10; verticalCenter: parent.verticalCenter }
            spacing: 10
            Text {
                text: (Backend.campaign(panel.name) || {}).title || panel.name
                color: Theme.foreground
                font { family: Theme.font; pixelSize: Theme.normal; weight: Font.DemiBold }
            }
            Text {
                visible: !!panel.cstate
                text: Words.campaign(panel.cstate)
                width: Math.min(implicitWidth, line.width)
                wrapMode: Text.WordWrap
                color: panel.cstate ? Words.campaignColour(panel.cstate.word) : Theme.muted
                font { family: Theme.font; pixelSize: Theme.normal; weight: Font.DemiBold }
            }
            Text {
                visible: !!panel.cstate && panel.cstate.current_wave > 0 && !!panel.cstate.last_wave
                text: panel.cstate ? "Wave " + panel.cstate.current_wave + " of " + panel.cstate.last_wave : ""
                color: Theme.quiet
                font { family: Theme.font; pixelSize: Theme.normal }
            }
            Repeater {
                model: panel.cstate && panel.cstate.word !== "finished" ? Words.taskOrder.filter(w => (panel.cstate.counts || {})[w] > 0) : []
                delegate: Text {
                    required property string modelData
                    text: panel.cstate.counts[modelData] + " " + Words.taskNames[modelData]
                    color: Words.taskColour(modelData)
                    font { family: Theme.font; pixelSize: Theme.normal }
                }
            }
            Text {
                visible: text.length > 0
                text: Words.closeLine(panel.cstate)
                width: Math.min(implicitWidth, line.width)
                wrapMode: Text.WordWrap
                color: Theme.quiet
                font { family: Theme.font; pixelSize: Theme.small }
                topPadding: 2
            }
            Text {
                visible: (!!panel.cstate && panel.age >= 15) || (!Backend.connected && Backend.lastHeard > 0)
                text: Words.age(panel.age)
                color: Theme.quiet
                font { family: Theme.font; pixelSize: Theme.small }
                topPadding: 2
            }
            Row {
                id: controls
                spacing: 6
                Repeater {
                    model: ["Interrupt", "Pause", "Kill"]
                    delegate: Button {
                        required property string modelData
                        readonly property string target: panel.selected ? "worker:" + panel.selected : "campaign"
                        readonly property string whom: panel.selected ? panel.selectedTitle : "the whole campaign"
                        word: panel.confirming === modelData ? "Confirm" : modelData
                        tone: modelData === "Kill" ? Theme.red : Theme.foreground
                        hint: modelData + " " + whom
                        onClicked: {
                            if (modelData === "Kill" && panel.confirming !== "Kill") {
                                panel.confirming = "Kill";
                                confirmTimer.restart();
                                return;
                            }
                            panel.confirming = "";
                            Backend.control(panel.name, modelData.toLowerCase(), target);
                        }
                    }
                }
                Button {
                    word: "Remove from view"
                    onClicked: Backend.removeFromView(panel.name)
                }
            }
        }

        Timer { id: confirmTimer; interval: 4000; onTriggered: panel.confirming = "" }
    }

    // Side panel: the wave tree with each task's word; its width is shared by every panel.
    Item {
        id: side
        anchors { left: parent.left; top: head.bottom; margins: 1 }
        width: panel.stacked ? panel.width - 2 : Math.max(panel.treeMin, Backend.sideWidth)
        height: panel.stacked ? Math.max(160, (panel.height - head.height) * 0.4) : panel.height - head.height - 2

        WaveTree {
            anchors { fill: parent; margins: 8 }
            cstate: panel.cstate
            campaign: (Backend.campaign(panel.name) || {}).title || panel.name
            selected: panel.selected
            // A click on a card always shows that worker; a second click never hides it again.
            onPicked: task => panel.selected = task
        }

        Text {
            anchors.centerIn: parent
            width: parent.width - 24
            visible: !panel.cstate
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.WordWrap
            text: panel.refusal || (Backend.connected ? "" : Words.lost)
            color: Theme.quiet
            font { family: Theme.font; pixelSize: Theme.small }
        }

        Rectangle { anchors { right: parent.right; top: parent.top; bottom: parent.bottom } width: 1; color: Theme.border }
        MouseArea {
            anchors { right: parent.right; top: parent.top; bottom: parent.bottom; rightMargin: -3 }
            width: 6
            cursorShape: Qt.SplitHCursor
            property real startX: 0
            onPressed: mouse => startX = mouse.x
            onPositionChanged: mouse => {
                if (pressed)
                    Backend.sideWidth = Math.max(180, Math.min(600, Backend.sideWidth + mouse.x - startX));
            }
        }
    }

    // Working agents as dots, three high then a new column.
    Grid {
        anchors { left: panel.stacked ? parent.left : side.right; leftMargin: 8; top: panel.stacked ? side.bottom : head.bottom; topMargin: panel.stacked ? 2 : 10 }
        flow: Grid.TopToBottom
        rows: 3
        spacing: 4
        Repeater {
            model: panel.cstate ? panel.cstate.tasks.filter(t => t.word === "working") : []
            delegate: Rectangle { width: 6; height: 6; radius: 3; color: Theme.blue }
        }
    }

    // Terminal area: the selected role's three views. Each view stays alive while hidden, so its
    // place holds across switches; the Terminal view detaches its viewer while hidden and comes back
    // to the same reading place.
    readonly property string viewWord: Backend.viewWords[name] || "Conversation"
    property string pane: ""
    property var paneGeneration: null
    property string paneStartedAt: ""
    property int resolveEpoch: 0
    // The task whose views show: the picked one, else the first working one.
    property string shownTask: ""
    readonly property string directivePath: {
        const t = cstate && cstate.tasks ? cstate.tasks.find(t => t.task === shownTask) : null;
        return t && t.directive ? t.directive : "";
    }
    readonly property string instruction: directiveFile.loaded ? directiveFile.text() : ""
    readonly property string instructionRefusal: ""
    FileView {
        id: directiveFile
        path: panel.directivePath
        watchChanges: true
        onFileChanged: reload()
    }
    property var turns: null

    function resolvePane() {
        const epoch = ++resolveEpoch;
        const c = Backend.campaign(name);
        if (!c || !c.session) {
            pane = "";
            return;
        }
        // A worker's pane is the agent pane launched for its task. The engine names the task on each
        // pane where it can; otherwise panes are matched to tasks in launch order (wave, then manifest
        // order), which is how the controller starts them. With nothing picked, the first working task.
        Backend.request("term.agent.list", {
            session: c.session
        }, (ok, agents) => {
            if (epoch !== panel.resolveEpoch || !ok || !Array.isArray(agents))
                return; // A failed read keeps the last pane and its frame.
            const tasks = panel.cstate && panel.cstate.tasks ? panel.cstate.tasks : [];
            const started = tasks.filter(t => t.word !== "not_started");
            const byStart = agents.slice().sort((a, b) => ((a.agent || {}).started_at || "").localeCompare((b.agent || {}).started_at || ""));
            const paneOf = task => {
                // Newest pane first: after an engine restart the old, dead panes keep their task names.
                const named = agents.filter(a => a.task === task).sort((x, y) => ((y.agent || {}).started_at || "").localeCompare((x.agent || {}).started_at || ""))[0];
                if (named)
                    return named;
                const i = started.findIndex(t => t.task === task);
                return i >= 0 && i < byStart.length ? byStart[i] : null;
            };
            let task = panel.selected;
            if (!task || !paneOf(task)) {
                const working = tasks.find(t => t.word === "working" || t.word === "waiting_on_job");
                task = working ? working.task : (started.length ? started[0].task : "");
            }
            const agent = task ? paneOf(task) : null;
            if (!agent)
                return;
            panel.shownTask = task;
            panel.pane = agent.pane;
            panel.paneGeneration = agent.generation !== undefined ? agent.generation : ((agent.agent || {}).generation ?? null);
            panel.paneStartedAt = (agent.agent || {}).started_at || agent.started_at || "";
        });
    }
    // The directive is read from its own file (directivePath), never from a route that answers with
    // something else.
    function loadInstruction() {
    }
    function loadTurns() {
        if (!pane) {
            turns = null;
            return;
        }
        Backend.request("conversation.get", {
            campaign: name,
            task: shownTask
        }, (ok, r) => panel.turns = ok && r.turns && r.turns.length ? r.turns : null);
    }
    onSelectedChanged: {
        resolvePane();
        loadInstruction();
    }
    onPaneChanged: loadTurns()
    // States arrive after the panel opens; the default worker is chosen once they do.
    onCstateChanged: if (!selected && !pane)
        resolvePane()
    onViewWordChanged: {
        if (viewWord === "Conversation")
            loadTurns();
        if (viewWord === "Instruction")
            loadInstruction();
    }
    Component.onCompleted: {
        resolvePane();
        loadScope();
    }
    Connections {
        target: Backend
        function onSessionsChanged() {
            panel.resolvePane();
        }
        function onPicksChanged() {
            if (Backend.picks[panel.name] !== undefined)
                panel.selected = Backend.picks[panel.name];
        }
        function onCampaignsChanged() {
            panel.resolvePane();
        }
        function onRefreshed() {
            panel.resolvePane();
        }
        // A new engine has new panes: the task's newest one is found again.
        function onReconnected() {
            panel.resolvePane();
        }
    }
    Timer {
        interval: 3000
        repeat: true
        running: panel.viewWord === "Conversation" && panel.turns !== null && panel.visible
        onTriggered: panel.loadTurns()
    }

    Item {
        id: termArea
        anchors { left: panel.stacked ? parent.left : side.right; right: parent.right; top: panel.stacked ? side.bottom : head.bottom; bottom: parent.bottom; margins: 8; leftMargin: panel.stacked ? 8 : 28 }
        // Prepared: the campaign's scope in place of the views, at most twelve lines until opened.
        Flickable {
            id: scopeView
            anchors.fill: parent
            visible: panel.prepared
            clip: true
            contentHeight: scopeColumn.implicitHeight
            boundsBehavior: Flickable.StopAtBounds
            readonly property var items: panel.scope && panel.scope.items ? panel.scope.items : []
            Column {
                id: scopeColumn
                width: scopeView.width
                spacing: 4
                Text {
                    width: scopeColumn.width
                    wrapMode: Text.WordWrap
                    text: panel.scope ? (scopeView.items.length ? "Built from " + scopeView.items.length + (scopeView.items.length === 1 ? " tracker item" : " tracker items") : (panel.scope.message || "This campaign has no tracker rows or outcome groups.")) : "Reading the tracker items"
                    color: Theme.quiet
                    font { family: Theme.font; pixelSize: Theme.small; weight: Font.DemiBold }
                }
                Repeater {
                    model: panel.scopeAll ? scopeView.items : scopeView.items.slice(0, 12)
                    delegate: Item {
                        id: scopeLine
                        required property var modelData
                        width: scopeColumn.width
                        height: Math.ceil(Theme.small * 1.5)
                        Text {
                            anchors { left: parent.left; right: status.left; rightMargin: 8; verticalCenter: parent.verticalCenter }
                            text: (scopeLine.modelData.kind === "group" ? "Group · " : "") + (scopeLine.modelData.words || "No words recorded")
                            elide: Text.ElideRight
                            maximumLineCount: 1
                            color: Theme.foreground
                            font { family: Theme.font; pixelSize: Theme.small }
                        }
                        Text {
                            id: status
                            anchors { right: parent.right; verticalCenter: parent.verticalCenter }
                            text: {
                                const w = (scopeLine.modelData.status || "").replace(/_/g, " ");
                                return w ? w.charAt(0).toUpperCase() + w.slice(1) : "";
                            }
                            color: Theme.quiet
                            font { family: Theme.font; pixelSize: Theme.small }
                        }
                    }
                }
                MouseArea {
                    visible: scopeView.items.length > 12
                    width: more.implicitWidth
                    height: visible ? more.implicitHeight + 6 : 0
                    cursorShape: Qt.PointingHandCursor
                    onClicked: panel.scopeAll = !panel.scopeAll
                    Text {
                        id: more
                        anchors.bottom: parent.bottom
                        text: panel.scopeAll ? "Show fewer items" : (scopeView.items.length - 12) + " more " + (scopeView.items.length - 12 === 1 ? "item" : "items")
                        color: Theme.citium
                        font { family: Theme.font; pixelSize: Theme.small }
                    }
                }
            }
        }
        Row {
            id: viewBar
            visible: !panel.prepared
            anchors { right: parent.right; top: parent.top }
            spacing: 4
            Repeater {
                model: ["Conversation", "Terminal", "Instruction"]
                delegate: Button {
                    required property string modelData
                    word: modelData
                    selected: panel.viewWord === modelData
                    onClicked: Backend.setView(panel.name, modelData)
                }
            }
        }
        Item {
            id: views
            visible: !panel.prepared
            anchors { left: parent.left; right: parent.right; top: viewBar.bottom; topMargin: 6; bottom: parent.bottom }
            clip: true

            Terminal {
                id: terminal
                anchors.fill: parent
                visible: panel.viewWord === "Terminal"
                pane: panel.pane
                task: panel.shownTask
                paneGeneration: panel.paneGeneration
                paneStartedAt: panel.paneStartedAt
            }

            // Conversation: the daemon's reading of the session; while none is recorded, the pane
            // itself under one line saying so.
            Item {
                anchors.fill: parent
                visible: panel.viewWord === "Conversation"
                Text {
                    id: notYet
                    visible: panel.turns === null
                    width: parent.width
                    text: "No conversation recorded for this task yet"
                    color: Theme.quiet
                    wrapMode: Text.WordWrap
                    font { family: Theme.font; pixelSize: Theme.small }
                }
                Terminal {
                    anchors { left: parent.left; right: parent.right; top: notYet.bottom; topMargin: 4; bottom: parent.bottom }
                    visible: parent.visible && panel.turns === null
                    pane: panel.pane
                    task: panel.shownTask
                    paneGeneration: panel.paneGeneration
                    paneStartedAt: panel.paneStartedAt
                }
                ListView {
                    anchors.fill: parent
                    visible: panel.turns !== null
                    clip: true
                    spacing: 10
                    model: panel.turns || []
                    delegate: Column {
                        id: turn
                        required property var modelData
                        width: ListView.view.width
                        Text {
                            text: turn.modelData.role || ""
                            color: Theme.quiet
                            font { family: Theme.font; pixelSize: Backend.paneSize(panel.pane) - 2 }
                        }
                        Text {
                            width: parent.width
                            text: turn.modelData.text || ""
                            textFormat: Text.MarkdownText
                            wrapMode: Text.Wrap
                            color: Theme.foreground
                            font { family: Theme.font; pixelSize: Backend.paneSize(panel.pane) }
                        }
                    }
                }
            }

            // Instruction: the task's directive rendered as Markdown.
            Flickable {
                id: instructionView
                anchors.fill: parent
                visible: panel.viewWord === "Instruction"
                clip: true
                contentHeight: markdown.implicitHeight + 16
                boundsBehavior: Flickable.StopAtBounds
                Text {
                    id: markdown
                    x: 8
                    y: 8
                    width: instructionView.width - 16
                    text: panel.instruction || panel.instructionRefusal
                    textFormat: panel.instruction ? Text.MarkdownText : Text.PlainText
                    wrapMode: Text.Wrap
                    color: panel.instruction ? Theme.foreground : Theme.quiet
                    font { family: Theme.font; pixelSize: Backend.paneSize(panel.pane) }
                    onLinkActivated: link => {}
                }
                MouseArea {
                    anchors.fill: parent
                    acceptedButtons: Qt.NoButton
                    onWheel: wheel => instructionView.contentY = Math.max(0, Math.min(instructionView.contentHeight - instructionView.height, instructionView.contentY - wheel.angleDelta.y))
                }
            }
        }
    }
}
