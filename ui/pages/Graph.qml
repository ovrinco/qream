pragma ComponentBehavior: Bound
import QtQuick
import "../parts"
import ".."

// The campaign's graph (design sheet 3.4): tasks and the rows they serve, coloured by the live states;
// a task picked here is the window's shared selection, which Review's replay narrows to.
Item {
    id: page

    readonly property var choices: {
        const out = [];
        const seen = {};
        const lists = [Backend.view.map(n => Backend.campaign(n)).filter(c => c !== null), Backend.live, Backend.done];
        for (let i = 0; i < lists.length; i++)
            for (let j = 0; j < lists[i].length; j++)
                if (!seen[lists[i][j].name]) {
                    seen[lists[i][j].name] = true;
                    out.push(lists[i][j]);
                }
        return out;
    }
    property string campaign: Backend.selectedCampaign !== "" ? Backend.selectedCampaign : choices.length > 0 ? choices[0].name : ""
    readonly property var cstate: Backend.states[campaign] || null
    property var card: null
    property var rowCard: null
    property var rowStatus: ({})
    property var asked: ({})

    function show(name) {
        if (name !== campaign) {
            Backend.select(name, "");
            card = null;
            rowCard = null;
        }
    }
    // Each row's status, read once per row and again when the store says it changed.
    function loadStatuses(all) {
        const tasks = cstate ? cstate.tasks : [];
        const want = {};
        for (let i = 0; i < tasks.length; i++)
            for (let j = 0; j < (tasks[i].rows || []).length; j++)
                if (all || asked[tasks[i].rows[j].id] === undefined)
                    want[tasks[i].rows[j].id] = true;
        for (const id in want) {
            asked[id] = true;
            Backend.row(id, (ok, r) => {
                if (!ok)
                    return;
                const s = Object.assign({}, page.rowStatus);
                s[id] = r.status || "";
                page.rowStatus = s;
                if (page.rowCard && page.rowCard.id === id)
                    page.rowCard = r;
            });
        }
    }
    function pick(node) {
        if (node === null) {
            card = null;
            rowCard = null;
            Backend.select(campaign, "");
            return;
        }
        if (node.kind === "task") {
            card = graph.task(node.task);
            rowCard = null;
            Backend.select(campaign, node.task);
        } else {
            openRow(node.row);
        }
    }
    function openRow(id) {
        Backend.row(id, (ok, r) => page.rowCard = ok ? r : null);
    }
    function taskOf(id) {
        return card ? graph.task(card.task) : null;
    }

    onCampaignChanged: {
        Backend.refresh(campaign);
        loadStatuses(false);
    }
    onCstateChanged: {
        loadStatuses(false);
        if (card)
            card = graph.task(card.task);
    }
    Component.onCompleted: {
        Backend.graph = page;
        if (campaign !== "") {
            Backend.refresh(campaign);
            loadStatuses(false);
        }
        if (Backend.selectedTask !== "")
            graph.selected = "task:" + Backend.selectedTask;
    }
    Component.onDestruction: {
        if (Backend.graph === page)
            Backend.graph = null;
    }
    Connections {
        target: Backend
        function onStoreChanged() {
            page.loadStatuses(true);
        }
    }

    // While shown, the campaign's state is asked for every two seconds; events arrive between.
    Timer {
        interval: 2000
        running: page.campaign !== ""
        repeat: true
        onTriggered: Backend.refresh(page.campaign)
    }
    Shortcut {
        sequence: "F"
        onActivated: graph.fit()
    }

    // The campaign tabs scroll sideways when they are wider than the room left of the Fit button.
    Flickable {
        id: bar
        anchors { left: parent.left; right: fitButton.left; top: parent.top; leftMargin: 16; rightMargin: 12; topMargin: 4 }
        height: 34
        contentWidth: tabs.implicitWidth
        contentHeight: height
        flickableDirection: Flickable.HorizontalFlick
        boundsBehavior: Flickable.StopAtBounds
        clip: true
        Row {
            id: tabs
            height: bar.height
            spacing: 6
            Repeater {
                model: page.choices
                delegate: Tab {
                    required property var modelData
                    word: modelData.title || modelData.name
                    shown: modelData.name === page.campaign
                    onClicked: page.show(modelData.name)
                    onShownChanged: if (shown)
                        bar.reveal(x, width)
                    Component.onCompleted: if (shown)
                        bar.reveal(x, width)
                }
            }
        }
        function reveal(x, w) {
            if (x < contentX)
                contentX = x;
            else if (x + w > contentX + width)
                contentX = Math.min(x + w - width, Math.max(0, contentWidth - width));
        }
        // A wheel turned over the tabs scrolls them sideways.
        WheelHandler {
            orientation: Qt.Vertical
            onWheel: e => bar.contentX = Math.max(0, Math.min(bar.contentWidth - bar.width, bar.contentX - e.angleDelta.y))
        }
    }
    Button {
        id: fitButton
        anchors { right: side.visible ? side.left : parent.right; rightMargin: 16; verticalCenter: bar.verticalCenter }
        word: "Fit"
        onClicked: graph.fit()
    }

    ForceGraph {
        id: graph
        anchors { left: parent.left; right: side.visible ? side.left : parent.right; top: bar.bottom; bottom: parent.bottom; topMargin: 8 }
        campaign: page.campaign
        tasks: page.cstate ? page.cstate.tasks : []
        rowStatus: page.rowStatus
        selected: page.rowCard ? "row:" + page.rowCard.id : page.card ? "task:" + page.card.task : ""
        onPicked: node => page.pick(node)
    }

    // Hover: the task's title whole, its state word and one trace line per row; a row's words and id.
    Rectangle {
        id: tip
        readonly property var node: graph.hoveredNode
        readonly property var t: node && node.kind === "task" ? graph.task(node.task) : null
        visible: node !== null
        x: Math.max(graph.x + 8, Math.min(graph.x + (node ? node.x * graph.zoom + graph.ox : 0) + 16, graph.x + graph.width - width - 8))
        y: Math.max(graph.y + 8, Math.min(graph.y + (node ? node.y * graph.zoom + graph.oy : 0) + 16, graph.y + graph.height - height - 8))
        width: Math.min(420, tipText.implicitWidth + 20)
        height: tipText.implicitHeight + 16
        radius: Theme.radius
        color: Theme.surface
        border.color: Theme.border
        Column {
            id: tipText
            x: 10
            y: 8
            spacing: 4
            width: 400
            Text {
                width: parent.width
                wrapMode: Text.Wrap
                text: tip.t ? tip.t.title || tip.t.task : tip.node ? tip.node.label + " (" + tip.node.row + ")" : ""
                color: Theme.foreground
                font { family: Theme.font; pixelSize: Theme.normal; bold: true }
            }
            Text {
                visible: tip.t !== null
                text: Words.task(tip.t)
                color: Words.taskColour(tip.t ? tip.t.word : "")
                font { family: Theme.font; pixelSize: Theme.normal }
            }
            Text {
                visible: tip.node !== null && tip.node.kind === "row"
                text: tip.node && tip.node.kind === "row" ? page.rowStatus[tip.node.row] || "" : ""
                color: Words.rowTone(text)
                font { family: Theme.font; pixelSize: Theme.normal }
            }
            Repeater {
                model: tip.t ? tip.t.rows : []
                delegate: Text {
                    required property var modelData
                    width: 400
                    wrapMode: Text.Wrap
                    text: Words.trace(modelData, tip.t.group, page.cstate ? page.campaign : "")
                    color: Theme.quiet
                    font { family: Theme.font; pixelSize: Theme.small }
                }
            }
        }
    }

    // The card at the right: a task's title, state word and rows; a row's words, status and evidence.
    Rectangle {
        id: side
        visible: page.card !== null || page.rowCard !== null
        anchors { right: parent.right; top: parent.top; bottom: parent.bottom }
        width: 340
        color: Theme.surface
        Rectangle { anchors { left: parent.left; top: parent.top; bottom: parent.bottom } width: 1; color: Theme.border }

        Flickable {
            anchors { fill: parent; margins: 16 }
            contentHeight: body.implicitHeight
            clip: true
            Column {
                id: body
                width: parent.width
                spacing: 8

                // Row
                Text {
                    visible: page.rowCard !== null
                    width: parent.width
                    wrapMode: Text.Wrap
                    text: page.rowCard ? page.rowCard.purpose + " (" + page.rowCard.id + ")" : ""
                    color: Theme.foreground
                    font { family: Theme.font; pixelSize: Theme.normal; bold: true }
                }
                Text {
                    visible: page.rowCard !== null
                    text: page.rowCard ? page.rowCard.status || "" : ""
                    color: Words.rowTone(text)
                    font { family: Theme.font; pixelSize: Theme.normal }
                }
                Repeater {
                    model: page.rowCard ? (page.rowCard.evidence || []).slice().reverse() : []
                    delegate: Text {
                        required property var modelData
                        width: body.width
                        wrapMode: Text.Wrap
                        text: (modelData.note || "") + " · " + (modelData.author || "") + " · " + String(modelData.at || "").replace("T", " ").slice(0, 16)
                        color: Theme.quiet
                        font { family: Theme.font; pixelSize: Theme.small }
                    }
                }
                Text {
                    visible: page.rowCard !== null && (page.rowCard.evidence || []).length === 0
                    text: "No evidence recorded"
                    color: Theme.quiet
                    font { family: Theme.font; pixelSize: Theme.small }
                }
                Rectangle {
                    visible: page.card !== null && page.rowCard !== null
                    width: parent.width
                    height: 1
                    color: Theme.border
                }
                // Task
                Text {
                    visible: page.card !== null
                    width: parent.width
                    wrapMode: Text.Wrap
                    text: page.card ? page.card.title || page.card.task : ""
                    color: Theme.foreground
                    font { family: Theme.font; pixelSize: Theme.large; bold: true }
                }
                Text {
                    visible: page.card !== null
                    text: Words.task(page.card)
                    color: Words.taskColour(page.card ? page.card.word : "")
                    font { family: Theme.font; pixelSize: Theme.normal }
                }
                Text {
                    visible: page.card !== null && !!page.card.group
                    width: parent.width
                    wrapMode: Text.Wrap
                    text: page.card && page.card.group ? page.card.group.words + " (" + page.card.group.id + ")" : ""
                    color: Theme.quiet
                    font { family: Theme.font; pixelSize: Theme.small }
                }
                Repeater {
                    model: page.card ? page.card.rows : []
                    delegate: MouseArea {
                        id: rowLine
                        required property var modelData
                        width: body.width
                        height: rowWords.implicitHeight + 8
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: page.openRow(modelData.id)
                        Rectangle { anchors.fill: parent; radius: Theme.radius; color: rowLine.containsMouse ? Theme.hover : "transparent" }
                        Rectangle {
                            x: 4
                            y: 9
                            width: 8
                            height: 8
                            radius: 4
                            color: "transparent"
                            border.width: 2
                            border.color: Words.rowTone(page.rowStatus[rowLine.modelData.id] || "")
                        }
                        Text {
                            id: rowWords
                            x: 20
                            y: 4
                            width: parent.width - 24
                            wrapMode: Text.Wrap
                            text: rowLine.modelData.words + " (" + rowLine.modelData.id + ")"
                            color: Theme.foreground
                            font { family: Theme.font; pixelSize: Theme.normal }
                        }
                    }
                }

            }
        }
    }

    // Automation for checks and captures (`qs ipc call qream graph <act> <arg>`).
    function automate(act, arg) {
        if (act === "stats")
            return graph.stats();
        if (act === "hover") {
            graph.hovered = arg === "" ? "" : arg.indexOf(":") >= 0 ? arg : "task:" + arg;
            return graph.hovered;
        }
        if (act === "pick") {
            const n = graph.nodes[graph.index[arg.indexOf(":") >= 0 ? arg : "task:" + arg]];
            page.pick(n || null);
            return n ? n.id : "none";
        }
        if (act === "campaign") {
            page.show(arg);
            return page.campaign;
        }
        if (act === "drag") {
            graph.sweepDrag(arg.indexOf(":") >= 0 ? arg : "task:" + arg);
            return "dragging";
        }
        if (act === "colour") {
            const t = graph.task(arg);
            return t ? t.word + " " + Words.taskColour(t.word) : "none";
        }
        if (act === "zoom") {
            graph.zoomAt(graph.width / 2, graph.height / 2, Number(arg));
            return String(graph.zoom);
        }
        if (act === "fit") {
            graph.fit();
            return String(graph.zoom);
        }
        return "unknown";
    }
}
