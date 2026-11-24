import QtQuick
import QtQuick.Controls as Q
import ".."

// The tracker grid: the live store read through the daemon (`store.rows`, paged by limit and offset).
// Whole-row tone by status, active rows first, a content bar, header filters, column ordering, picks.
Item {
    id: grid

    // What the page reads and lights.
    property var picked: ({})
    property var lit: ({})
    property string focusId: ""
    property bool live: visible
    signal pickChanged
    signal rowOpened(var row)

    // The loaded rows and the order shown.
    property var rows: []
    property var shown: []
    property var tabs: []
    property string tab: ""
    property string status: ""
    property string text: ""
    property string orderBy: ""
    property bool descending: false
    property int total: 0
    property int revision: -1
    property bool loading: false
    property int generation: 0
    property int anchor: -1
    property double started: 0
    property int firstPaintMs: -1
    property int lastRefreshMs: -1

    readonly property int page: 4000
    readonly property var statuses: ["in build", "said", "planned", "Planning", "proposed", "implemented", "verified", "built wrong", "superseded", "rejected"]
    // The status column is as wide as its longest word (or its heading with the order mark) plus the cell margins.
    FontMetrics { id: statusMetrics; font { family: Theme.font; pixelSize: Theme.small } }
    FontMetrics { id: headMetrics; font { family: Theme.font; pixelSize: Theme.small; weight: Font.DemiBold } }
    readonly property int statusWidth: {
        let w = headMetrics.advanceWidth("Status ▾");
        for (const word of statuses)
            w = Math.max(w, statusMetrics.advanceWidth(word));
        return Math.ceil(w) + 8 + 4 + 2;
    }
    readonly property var columns: [
        { key: "id", word: "Id", width: 150 },
        { key: "tab", word: "Tab", width: 92 },
        { key: "status", word: "Status", width: statusWidth },
        { key: "purpose", word: "Purpose", width: 0 },
        { key: "outcome_id", word: "Group", width: 150 },
        { key: "campaign", word: "Campaign", width: 110 },
        { key: "age", word: "Age", width: 46 }
    ]

    function active(r) {
        return r.status === "in build";
    }
    function tone(st) {
        switch (st) {
        case "in build":
            return Theme.blue;
        case "planned":
        case "Planning":
            return Theme.yellow;
        case "implemented":
            return Theme.cyan;
        case "verified":
            return Theme.green;
        case "built wrong":
            return Theme.red;
        default:
            return "transparent";
        }
    }
    // The words' colour: the tone, or the plain foreground for neutral statuses.
    function ink(st) {
        const t = tone(st);
        return t === "transparent" ? Theme.foreground : t;
    }
    function cell(r, key) {
        if (key === "tab")
            return r.tab || r.sheet || "";
        const v = r[key];
        return v === null || v === undefined ? "" : String(v);
    }
    function purposeOf(r) {
        return r.purpose || r.fault || r.name || r.title || "";
    }
    function slug(r) {
        return purposeOf(r).split(/\s+/).filter(w => w.length > 0).slice(0, 6).join(" ");
    }
    function rowById(id) {
        for (let i = 0; i < rows.length; i++)
            if (rows[i].id === id)
                return rows[i];
        return null;
    }
    function pickedList() {
        const out = [];
        for (const id in picked)
            out.push(id);
        return out;
    }

    // Active first (a stable partition), then the chosen column.
    function arrange() {
        let list = rows.slice();
        if (orderBy.length > 0) {
            const k = orderBy;
            const sign = descending ? -1 : 1;
            list.sort((a, b) => {
                const x = k === "age" ? Number(a[k] || 0) : cell(a, k).toLowerCase();
                const y = k === "age" ? Number(b[k] || 0) : cell(b, k).toLowerCase();
                return x < y ? -sign : x > y ? sign : 0;
            });
        }
        const first = list.filter(r => active(r));
        const rest = list.filter(r => !active(r));
        shown = first.concat(rest);
        while (rowModel.count < shown.length)
            rowModel.append({});
        while (rowModel.count > shown.length)
            rowModel.remove(rowModel.count - 1);
    }

    // Loads every page for the current filters into a fresh list, then swaps it in whole:
    // the view keeps its scroll and the picks are ids, so neither is lost.
    function load() {
        generation += 1;
        const gen = generation;
        const acc = [];
        const seenTabs = {};
        const t0 = Date.now();
        loading = true;
        const args = {};
        if (tab.length > 0)
            args.tab = tab;
        if (status.length > 0)
            args.status = status;
        if (text.length > 0)
            args.text = text;
        function step(offset) {
            Backend.request("store.rows", Object.assign({
                limit: offset === 0 && grid.rows.length === 0 ? 200 : grid.page,
                offset: offset
            }, args), (ok, result) => {
                if (gen !== grid.generation)
                    return;
                if (!ok) {
                    grid.loading = false;
                    return;
                }
                for (let i = 0; i < result.rows.length; i++) {
                    const r = result.rows[i];
                    acc.push(r);
                    seenTabs[r.tab || r.sheet || ""] = true;
                }
                grid.total = result.total;
                // First paint: show the first page at once on an empty grid.
                if (grid.rows.length === 0 && offset === 0) {
                    grid.rows = acc.slice();
                    grid.arrange();
                    if (grid.firstPaintMs < 0)
                        grid.firstPaintMs = Date.now() - grid.started;
                }
                if (result.rows.length > 0 && offset + result.rows.length < result.total) {
                    step(offset + result.rows.length);
                    return;
                }
                grid.rows = acc;
                grid.arrange();
                if (tab.length === 0 && status.length === 0 && text.length === 0)
                    grid.tabs = Object.keys(seenTabs).filter(k => k.length > 0).sort();
                grid.loading = false;
                grid.lastRefreshMs = Date.now() - t0;
            });
        }
        step(0);
    }
    function reload() {
        rows = [];
        arrange();
        started = Date.now();
        load();
    }

    function pick(index, mods) {
        const r = shown[index];
        if (!r)
            return;
        const p = (mods & Qt.ControlModifier) || (mods & Qt.ShiftModifier) ? Object.assign({}, picked) : {};
        if ((mods & Qt.ShiftModifier) && anchor >= 0) {
            const a = Math.min(anchor, index), b = Math.max(anchor, index);
            for (let i = a; i <= b; i++)
                p[shown[i].id] = true;
        } else if ((mods & Qt.ControlModifier) && picked[r.id]) {
            delete p[r.id];
        } else {
            p[r.id] = true;
            anchor = index;
        }
        picked = p;
        focusId = r.id;
        pickChanged();
        rowOpened(r);
    }
    // A group heading picks its rows: the store's members of the outcome group.
    function pickGroup(group, add) {
        Backend.request("store.members", {
            group: group
        }, (ok, ids) => {
            if (!ok)
                return;
            const p = add ? Object.assign({}, grid.picked) : {};
            for (let i = 0; i < ids.length; i++)
                p[ids[i]] = true;
            grid.picked = p;
            grid.pickChanged();
        });
    }
    function clearPick() {
        picked = {};
        anchor = -1;
        pickChanged();
    }
    function showRow(id) {
        for (let i = 0; i < shown.length; i++)
            if (shown[i].id === id) {
                list.positionViewAtIndex(i, ListView.Contain);
                return;
            }
    }

    Component.onCompleted: {
        started = Date.now();
        load();
    }
    onTabChanged: reload()
    onStatusChanged: reload()
    onOrderByChanged: arrange()
    onDescendingChanged: arrange()

    // A worker's write: the store's revision is read four times a second and a change reloads the rows.
    Timer {
        interval: 250
        repeat: true
        running: grid.live && Backend.connected
        onTriggered: Backend.request("store.revision", {}, (ok, r) => {
            if (!ok)
                return;
            if (grid.revision >= 0 && r.revision !== grid.revision && !grid.loading)
                grid.load();
            grid.revision = r.revision;
        })
    }
    Timer {
        id: textDelay
        interval: 250
        onTriggered: grid.reload()
    }

    ListModel {
        id: rowModel
    }

    Column {
        id: top
        anchors { left: parent.left; right: parent.right; top: parent.top }
        spacing: 6

        // The tabs of the store, as linked category tabs.
        Flow {
            width: parent.width
            spacing: 4
            Button {
                word: "All"
                selected: grid.tab === ""
                onClicked: grid.tab = ""
            }
            Repeater {
                model: grid.tabs
                Button {
                    required property string modelData
                    word: modelData
                    selected: grid.tab === modelData
                    onClicked: grid.tab = modelData
                }
            }
        }

        // Header filters: the text filter and status chips, wrapping to a second line in a narrow window.
        Flow {
            width: parent.width
            spacing: 4
            Rectangle {
                width: 220
                height: 22
                radius: Theme.radius
                color: Theme.sunken
                border { width: 1; color: Theme.border }
                TextInput {
                    id: filterText
                    anchors { fill: parent; leftMargin: 6; rightMargin: 6 }
                    verticalAlignment: TextInput.AlignVCenter
                    color: Theme.foreground
                    clip: true
                    font { family: Theme.font; pixelSize: Theme.small }
                    onTextEdited: {
                        grid.text = text;
                        textDelay.restart();
                    }
                }
                Text {
                    anchors { fill: parent; leftMargin: 6 }
                    verticalAlignment: Text.AlignVCenter
                    visible: filterText.text.length === 0
                    text: "Filter"
                    color: Theme.quiet
                    font { family: Theme.font; pixelSize: Theme.small }
                }
            }
            Repeater {
                model: grid.statuses
                Button {
                    required property string modelData
                    word: modelData
                    tone: grid.ink(modelData)
                    selected: grid.status === modelData
                    onClicked: grid.status = grid.status === modelData ? "" : modelData
                }
            }
        }

        // The content bar: the focused row's full words.
        Rectangle {
            width: parent.width
            height: Math.max(26, content.implicitHeight + 8)
            radius: Theme.radius
            color: Theme.sunken
            border { width: 1; color: Theme.border }
            Text {
                id: content
                anchors { fill: parent; margins: 4; leftMargin: 6 }
                wrapMode: Text.WordWrap
                maximumLineCount: 3
                elide: Text.ElideRight
                readonly property var row: grid.focusId.length > 0 ? grid.rowById(grid.focusId) : null
                text: row ? row.id + "  " + grid.purposeOf(row) : (grid.total + " rows")
                color: Theme.foreground
                font { family: Theme.font; pixelSize: Theme.small }
            }
        }

        // Column headings: a click orders by the column, a second reverses it.
        Row {
            id: heads
            width: parent.width
            height: 22
            Repeater {
                model: grid.columns
                MouseArea {
                    required property var modelData
                    width: modelData.width > 0 ? modelData.width : Math.max(120, heads.width - grid.fixedWidth())
                    height: 22
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        if (grid.orderBy === modelData.key)
                            grid.descending = !grid.descending;
                        else {
                            grid.descending = false;
                            grid.orderBy = modelData.key;
                        }
                    }
                    Text {
                        anchors { fill: parent; leftMargin: 8 }
                        verticalAlignment: Text.AlignVCenter
                        text: parent.modelData.word + (grid.orderBy === parent.modelData.key ? (grid.descending ? " ▾" : " ▴") : "")
                        color: Theme.quiet
                        font { family: Theme.font; pixelSize: Theme.small; weight: Font.DemiBold }
                    }
                }
            }
        }
    }

    function fixedWidth() {
        let w = 0;
        for (let i = 0; i < columns.length; i++)
            w += columns[i].width;
        return w;
    }

    ListView {
        id: list
        anchors { left: parent.left; right: parent.right; top: top.bottom; bottom: parent.bottom; topMargin: 2 }
        clip: true
        model: rowModel
        reuseItems: true
        cacheBuffer: 400
        boundsBehavior: Flickable.StopAtBounds
        Q.ScrollBar.vertical: Q.ScrollBar {}

        delegate: MouseArea {
            id: line
            required property int index
            readonly property var r: grid.shown[index] || ({})
            readonly property bool isPicked: grid.picked[r.id] === true
            readonly property bool isLit: grid.lit[r.id] === true
            width: ListView.view.width
            height: 24
            acceptedButtons: Qt.LeftButton
            hoverEnabled: true
            onClicked: mouse => grid.pick(index, mouse.modifiers)

            Rectangle {
                anchors.fill: parent
                color: grid.tone(line.r.status)
                opacity: 0.22
            }
            Rectangle {
                anchors.fill: parent
                color: line.isPicked ? Theme.hover : line.containsMouse ? Theme.hover : "transparent"
                border { width: line.isLit || line.isPicked ? 1 : 0; color: line.isLit ? Theme.citium : Theme.border }
            }
            // The active stripe.
            Rectangle {
                width: 3
                height: parent.height
                color: Theme.blue
                visible: grid.active(line.r)
            }
            Row {
                anchors.fill: parent
                Repeater {
                    model: grid.columns
                    Item {
                        required property var modelData
                        width: modelData.width > 0 ? modelData.width : Math.max(120, line.width - grid.fixedWidth())
                        height: line.height
                        Text {
                            anchors { fill: parent; leftMargin: 8; rightMargin: 4 }
                            verticalAlignment: Text.AlignVCenter
                            elide: Text.ElideRight
                            text: parent.modelData.key === "purpose" ? grid.purposeOf(line.r) : grid.cell(line.r, parent.modelData.key)
                            color: parent.modelData.key === "status" ? grid.ink(line.r.status) : (line.r.status === "superseded" || line.r.status === "rejected") ? Theme.quiet : Theme.foreground
                            font { family: parent.modelData.key === "id" || parent.modelData.key === "outcome_id" ? Theme.mono : Theme.font; pixelSize: Theme.small }
                        }
                        // The group cell picks the whole group (Ctrl adds it to the picks).
                        MouseArea {
                            anchors.fill: parent
                            enabled: parent.modelData.key === "outcome_id" && grid.cell(line.r, "outcome_id").length > 0
                            cursorShape: enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
                            onClicked: mouse => grid.pickGroup(line.r.outcome_id, (mouse.modifiers & Qt.ControlModifier) !== 0)
                        }
                    }
                }
            }
        }
    }

    Shortcut {
        sequence: "Escape"
        enabled: grid.visible
        onActivated: grid.clearPick()
    }
}
