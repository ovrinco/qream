pragma ComponentBehavior: Bound
import QtQuick
import "../parts"
import ".."

// Review: finished campaigns at the left (earlier ones in the archive), the disk replay of the chosen
// one as a map with its timeline, and beside it the task picker and the rows as they stood.
Item {
    id: page
    focus: true

    property string campaign: ""
    property string task: ""
    property bool archive: false
    property bool loading: false
    property var events: []
    property var times: []
    property var kinds: []
    property double start: 0
    property double end: 1
    property double from: 0
    property double to: 1
    property double playhead: 0
    property bool playing: false
    property real speed: 1
    // Wall-clock time of the last advance while playing; 0 when not playing.
    property double wall: 0
    property var history: null
    property int fps: 0

    readonly property var tasks: {
        const s = Backend.states[campaign];
        return s && s.tasks ? s.tasks : [];
    }
    readonly property var rows: {
        const out = [], seen = {};
        for (const t of tasks)
            if (!task || t.task === task)
                for (const r of (t.rows || []))
                    if (!seen[r.id]) {
                        seen[r.id] = true;
                        out.push(r);
                    }
        return out;
    }
    readonly property int shown: {
        let lo = 0, hi = times.length;
        while (lo < hi) {
            const mid = (lo + hi) >> 1;
            if (times[mid] <= playhead)
                lo = mid + 1;
            else
                hi = mid;
        }
        return lo;
    }
    // The archive holds finished campaigns last changed before this week's Monday.
    readonly property double weekStart: {
        const d = new Date();
        d.setHours(0, 0, 0, 0);
        d.setDate(d.getDate() - (d.getDay() + 6) % 7);
        return d.getTime();
    }
    readonly property var recent: Backend.done.filter(c => Date.parse(c.updated || c.created) >= weekStart)
    readonly property var earlier: Backend.done.filter(c => Date.parse(c.updated || c.created) < weekStart)

    function open(name) {
        campaign = name;
        task = "";
        load();
    }
    function pick(name) {
        task = task === name ? "" : name;
        load();
    }
    function load() {
        playing = false;
        loading = true;
        const args = {
            campaign: campaign
        };
        if (task)
            args.task = task;
        Backend.request("replay.events", args, (ok, result) => {
            loading = false;
            if (!ok) {
                Backend.say(result.message || "");
                return;
            }
            build(result);
        });
        loadHistory();
    }
    function build(list) {
        const t = [], k = [], byPath = {};
        for (const e of list) {
            const at = Date.parse(e.at);
            t.push(at);
            k.push(e.kind);
            if (e.path && (e.kind === "read" || e.kind === "write" || e.kind === "search")) {
                const n = byPath[e.path] || (byPath[e.path] = {
                        path: e.path,
                        times: [],
                        kinds: []
                    });
                n.times.push(at);
                n.kinds.push(e.kind);
            }
        }
        events = list;
        times = t;
        kinds = k;
        start = t.length ? t[0] : 0;
        end = t.length ? t[t.length - 1] : 1;
        from = start;
        to = end;
        playhead = start;
        layout(Object.values(byPath));
    }
    // Districts are folders, packed in a square of squares; one cell per path.
    function layout(nodes) {
        const groups = {};
        for (const n of nodes) {
            const parts = n.path.split("/");
            n.district = parts.length > 1 ? parts[parts.length - 2] || "/" : "/";
            (groups[n.district] = groups[n.district] || []).push(n);
        }
        const names = Object.keys(groups).sort((a, b) => groups[b].length - groups[a].length);
        const sides = names.map(n => Math.ceil(Math.sqrt(groups[n].length)) + 1);
        const perRow = Math.max(1, Math.ceil(Math.sqrt(names.length)));
        const districts = [];
        let x = 0, z = 0, rowDepth = 0, width = 0;
        names.forEach((name, i) => {
            if (i > 0 && i % perRow === 0) {
                z += rowDepth + 1;
                x = 0;
                rowDepth = 0;
            }
            const side = sides[i];
            groups[name].sort((a, b) => a.path < b.path ? -1 : 1).forEach((n, j) => {
                n.x = x + 1 + j % (side - 1);
                n.z = z + 1 + Math.floor(j / (side - 1));
            });
            districts.push({
                name: name,
                x: x + side / 2,
                z: z + side / 2,
                width: side,
                depth: side
            });
            x += side + 1;
            rowDepth = Math.max(rowDepth, side);
            width = Math.max(width, x);
        });
        const cx = width / 2, cz = (z + rowDepth) / 2;
        for (const d of districts) {
            d.x -= cx;
            d.z -= cz;
            d.x += 0.5;
            d.z += 0.5;
        }
        for (const n of nodes) {
            n.x -= cx;
            n.z -= cz;
        }
        map.districts = districts;
        map.nodes = nodes;
    }
    // Row history comes from the store's events through the daemon; until it is served the rows
    // stand as they are now.
    function loadHistory() {
        const ids = rows.map(r => r.id);
        history = null;
        Backend.request("store.events", {
            since: 0,
            ids: ids
        }, (ok, result) => {
            if (ok)
                history = result;
            else
                loadCurrent(ids);
        });
    }
    property var current: ({})
    function loadCurrent(ids) {
        for (const id of ids)
            Backend.request("store.row", {
                id: id
            }, (ok, result) => {
                if (!ok)
                    return;
                const c = Object.assign({}, current);
                c[id] = result.status;
                current = c;
            });
    }
    function statusAt(id, t) {
        if (!history)
            return current[id] || "";
        let s = "";
        for (const e of history) {
            if (e.id !== id || Date.parse(e.at) > t)
                continue;
            if (e.action === "add") {
                try {
                    s = JSON.parse(e.new).status || s;
                } catch (x) {}
            } else if (e.field === "status") {
                try {
                    s = JSON.parse(e.new);
                } catch (x) {
                    s = e.new;
                }
            }
        }
        return s;
    }
    function statusTone(s) {
        return s === "verified" || s === "implemented" ? Theme.green : s === "in build" ? Theme.blue : s === "planned" ? Theme.foreground : Theme.quiet;
    }
    function seek(t) {
        playhead = Math.max(from, Math.min(to, t));
    }
    function step(n) {
        const i = Math.max(0, Math.min(times.length - 1, shown - 1 + n));
        if (times.length)
            seek(times[i]);
    }
    function toggle() {
        if (!playing && playhead >= to)
            playhead = from;
        wall = 0;
        playing = !playing;
    }
    function seekFraction(f) {
        seek(start + (end - start) * f);
    }
    function boundFractions(f, t) {
        from = start + (end - start) * f;
        to = start + (end - start) * t;
        seek(playhead);
    }
    function report() {
        return JSON.stringify({
            campaign: campaign,
            task: task,
            playing: playing,
            speed: speed,
            shown: shown,
            total: times.length,
            playhead: new Date(playhead).toISOString(),
            position_s: Math.round((playhead - from) / 100) / 10,
            from: new Date(from).toISOString(),
            to: new Date(to).toISOString(),
            fps: fps,
            history: history !== null,
            rows: rows.length,
            statuses: rows.slice(0, 4).map(r => r.id + "=" + statusAt(r.id, playhead))
        });
    }
    function elapsed(ms) {
        const s = Math.max(0, Math.floor(ms / 1000));
        return Math.floor(s / 60) + ":" + ("0" + s % 60).slice(-2);
    }
    function clock(t) {
        const d = new Date(t);
        return ("0" + d.getHours()).slice(-2) + ":" + ("0" + d.getMinutes()).slice(-2) + ":" + ("0" + d.getSeconds()).slice(-2);
    }

    // The window's shared selection (picked on the Graph page) narrows the replay to that task.
    Connections {
        target: Backend
        function onSelectedTaskChanged() {
            if (!Backend.selectedCampaign)
                return;
            page.campaign = Backend.selectedCampaign;
            page.task = Backend.selectedTask;
            page.load();
        }
    }

    Keys.onSpacePressed: toggle()
    Keys.onLeftPressed: step(-1)
    Keys.onRightPressed: step(1)
    Keys.onPressed: e => {
        if (e.key === Qt.Key_Home)
            seek(from);
        else if (e.key === Qt.Key_End)
            seek(to);
    }
    onRowsChanged: if (campaign)
        loadHistory()
    // Review opens on the map of the latest finished campaign.
    Component.onCompleted: if (recent.length || earlier.length)
        open((recent.length ? recent : earlier).slice(-1)[0].name)

    // The replay clock advances by real elapsed time times the speed: a 64 s span at 1x takes 64 s.
    FrameAnimation {
        running: page.playing
        onTriggered: {
            const now = Date.now();
            const gap = page.wall > 0 ? now - page.wall : 0;
            page.wall = now;
            const next = page.playhead + gap * page.speed;
            if (next >= page.to) {
                page.playhead = page.to;
                page.playing = false;
            } else {
                page.playhead = next;
            }
        }
    }
    // Paints per second of the map, kept for the frame-rate check.
    Timer {
        interval: 1000
        repeat: true
        running: page.playing
        property int last: 0
        onTriggered: {
            page.fps = map.painted - last;
            last = map.painted;
        }
    }

    // Finished campaigns, then the archive behind "Earlier campaigns".
    Column {
        id: list
        anchors { left: parent.left; top: parent.top; bottom: parent.bottom; margins: 12 }
        width: Math.min(220, page.width * 0.16)
        spacing: 2
        Repeater {
            model: page.archive ? page.earlier : page.recent
            delegate: CampaignChoice {}
        }
        MouseArea {
            width: list.width
            height: 30
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: page.archive = !page.archive
            Text {
                anchors { left: parent.left; leftMargin: 10; verticalCenter: parent.verticalCenter }
                text: page.archive ? "This week" : "Earlier campaigns"
                color: parent.containsMouse ? Theme.foreground : Theme.quiet
                font { family: Theme.font; pixelSize: Theme.normal }
            }
        }
    }
    component CampaignChoice: MouseArea {
        id: choice
        required property var modelData
        width: list.width
        height: 30
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: page.open(modelData.name)
        Rectangle {
            anchors.fill: parent
            radius: Theme.radius
            color: page.campaign === choice.modelData.name || choice.containsMouse ? Theme.hover : "transparent"
        }
        Text {
            anchors { left: parent.left; leftMargin: 10; verticalCenter: parent.verticalCenter }
            text: choice.modelData.title || choice.modelData.name
            color: Theme.foreground
            font { family: Theme.font; pixelSize: Theme.normal }
        }
    }

    Rectangle {
        id: stage
        anchors { left: list.right; right: side.left; top: parent.top; bottom: parent.bottom; margins: 12 }
        radius: Theme.radius
        color: Theme.sunken

        ReplayMap {
            id: map
            anchors { left: parent.left; right: parent.right; top: parent.top; bottom: controls.top }
            visible: page.campaign !== "" && page.events.length > 0
            playhead: page.playhead
            from: page.from
            pulse: Math.max(1000, (page.to - page.from) / 60)
        }
        Text {
            anchors.centerIn: map
            visible: !map.visible
            text: page.campaign === "" ? "Choose a campaign to replay" : page.loading ? "Loading the replay" : "No recorded activity for this campaign yet"
            color: Theme.quiet
            font { family: Theme.font; pixelSize: Theme.large }
        }

        Column {
            id: controls
            anchors { left: parent.left; right: parent.right; bottom: parent.bottom; margins: 12 }
            spacing: 8
            visible: map.visible
            ReplayTimeline {
                width: parent.width
                height: 28
                times: page.times
                kinds: page.kinds
                start: page.start
                end: page.end
                from: page.from
                to: page.to
                playhead: page.playhead
                onSeek: at => page.seek(at)
                onBounds: (f, t) => {
                    page.from = f;
                    page.to = t;
                    page.seek(page.playhead);
                }
            }
            Row {
                spacing: 8
                Button { word: page.playing ? "Pause" : "Play"; onClicked: page.toggle() }
                Text { text: "Speed"; color: Theme.quiet; anchors.verticalCenter: parent.verticalCenter; font { family: Theme.font; pixelSize: Theme.small } }
                Repeater {
                    model: [1, 1.5, 2]
                    delegate: Button {
                        required property real modelData
                        word: modelData + "x"
                        selected: page.speed === modelData
                        onClicked: page.speed = modelData
                    }
                }
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    text: "From " + page.clock(page.from) + "   To " + page.clock(page.to) + "   " + page.clock(page.playhead) + " · " + page.elapsed(page.playhead - page.from) + " of " + page.elapsed(page.to - page.from) + " · " + page.shown + " of " + page.times.length
                    color: Theme.quiet
                    font { family: Theme.font; pixelSize: Theme.small }
                }
            }
        }
    }

    // The task picker (the graph joins here at integration) and the rows as they stood.
    Column {
        id: side
        anchors { right: parent.right; top: parent.top; bottom: parent.bottom; margins: 12 }
        width: Math.min(340, page.width * 0.26)
        spacing: 10
        visible: page.campaign !== ""
        ListView {
            id: picker
            width: parent.width
            height: (parent.height - 10) * 0.5
            clip: true
            model: page.tasks
            delegate: MouseArea {
                id: taskRow
                required property var modelData
                width: picker.width
                height: 26
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: page.pick(modelData.task)
                Rectangle {
                    anchors.fill: parent
                    radius: Theme.radius
                    color: page.task === taskRow.modelData.task ? Theme.hover : taskRow.containsMouse ? Qt.rgba(Theme.hover.r, Theme.hover.g, Theme.hover.b, 0.04) : "transparent"
                    border { width: page.task === taskRow.modelData.task ? 1 : 0; color: Theme.citium }
                }
                Text {
                    anchors { left: parent.left; right: parent.right; leftMargin: 8; rightMargin: 8; verticalCenter: parent.verticalCenter }
                    text: taskRow.modelData.title || taskRow.modelData.task
                    elide: Text.ElideRight
                    color: Theme.foreground
                    font { family: Theme.font; pixelSize: Theme.small }
                }
            }
        }
        Text {
            text: page.history ? "Rows at " + page.clock(page.playhead) : "Rows now"
            color: Theme.quiet
            font { family: Theme.font; pixelSize: Theme.small }
        }
        ListView {
            id: rowList
            width: parent.width
            height: (parent.height - 10) * 0.5 - 30
            clip: true
            model: page.rows
            delegate: Item {
                id: rowItem
                required property var modelData
                readonly property string status: page.statusAt(modelData.id, page.playhead)
                width: rowList.width
                height: 24
                Rectangle { width: 3; height: 14; radius: 1; color: page.statusTone(rowItem.status); anchors.verticalCenter: parent.verticalCenter }
                Text {
                    anchors { left: parent.left; right: state.left; leftMargin: 10; rightMargin: 8; verticalCenter: parent.verticalCenter }
                    text: rowItem.modelData.words
                    elide: Text.ElideRight
                    color: Theme.foreground
                    font { family: Theme.font; pixelSize: Theme.small }
                }
                Text {
                    id: state
                    anchors { right: parent.right; verticalCenter: parent.verticalCenter }
                    text: Words.rowStatus(rowItem.status)
                    color: page.statusTone(rowItem.status)
                    font { family: Theme.font; pixelSize: Theme.small }
                }
            }
        }
    }
}
