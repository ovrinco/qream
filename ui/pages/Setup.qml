pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Controls as Q
import Quickshell
import Quickshell.Io
import "../parts"
import "../parts" as Parts
import ".."

// The planning page: the live grid, the campaign column, and the orchestrator column's slot.
// Setup builds a draft through the setup routes and publishes it as Prepared; it launches nothing.
Item {
    id: setup

    // Where new campaign folders are made, and where the page remembers its open draft.
    readonly property string folderBase: Quickshell.env("QREAM_SETUP_DIR") || (Quickshell.env("HOME") + "/04_memory/runs/qream")
    readonly property string stateFile: Quickshell.env("QREAM_SETUP_STATE") || (Quickshell.env("HOME") + "/.local/state/qream/setup.json")

    property string name: ""
    property string root: ""
    property var draft: null
    property var refusals: []
    property bool checked: false
    property var routes: ({})
    property var routeRefusals: ({})
    property var harnesses: []
    // Each harness's efforts as the engine lists them (`seat.harnesses`), by kind.
    property var efforts: ({})
    property int lastWave: 0
    property string expected: ""
    property bool closeDeclared: false
    property bool confirmed: false
    property bool closeOpen: false
    property var openCard: null
    property string openText: ""
    property var rowInfo: null
    property var folded: ({})
    property var rowWords: ({})
    property double draftAt: 0

    // The orchestrator column fills this slot; its item may offer `ask(text)`.
    property alias orchestratorSlot: slot

    readonly property bool started: root.length > 0
    readonly property var scopeIds: started && draft ? scopeOf(draft) : grid.pickedList()
    readonly property var cards: draft ? draft.cards : []
    readonly property var waves: draft ? (draft.waves.length > 0 ? draft.waves : [1]) : []
    readonly property bool routed: waves.length > 0 && waves.every(w => !!routes[w] && !routeRefusals[w])
    readonly property bool checksPass: checked && refusals.length === 0 && cards.length > 0
    readonly property string confirmReason: !checksPass ? (cards.length === 0 ? "No directives yet" : "The checks refuse this campaign") : !routed ? "Choose a harness for every wave" : !closeDeclared ? "Declare the close first" : ""

    function scopeOf(d) {
        const out = d.scope.rows.slice();
        for (let i = 0; i < d.cards.length; i++)
            for (const id in d.cards[i].rows)
                if (out.indexOf(id) < 0)
                    out.push(id);
        return out;
    }
    function words(id) {
        const r = grid.rowById(id);
        if (r)
            return grid.slug(r);
        return rowWords[id] || "";
    }
    function groupOf(id) {
        const r = grid.rowById(id);
        return r && r.outcome_id ? r.outcome_id : "";
    }
    function groupWords(g) {
        const r = grid.rows.find(x => x.outcome_id === g);
        return r && r.outcome ? r.outcome : g;
    }
    // Scope references grouped under their outcome group.
    function groups() {
        const by = {};
        const order = [];
        for (let i = 0; i < scopeIds.length; i++) {
            const g = groupOf(scopeIds[i]);
            if (!by[g]) {
                by[g] = [];
                order.push(g);
            }
            by[g].push(scopeIds[i]);
        }
        return order.map(g => ({
                    group: g,
                    ids: by[g]
                }));
    }
    function cardsOf(id) {
        return cards.filter(c => c.rows[id] !== undefined);
    }
    function folderName() {
        const d = new Date();
        const pad = n => (n < 10 ? "0" : "") + n;
        return d.getFullYear() + "-" + pad(d.getMonth() + 1) + "-" + pad(d.getDate()) + "_" + name;
    }

    // The refusal form: "<rule> refuses <row id> (<the row's first six words>): <reason in words>".
    readonly property var ruleWords: ({
            R1: "Row text",
            L27: "Row text",
            L28: "Served row",
            R2: "Served row",
            R3: "Unpicked row",
            L29: "Unpicked row",
            R4: "Thin row",
            L30: "Exclusion line",
            L31: "Work items",
            L32: "Work items",
            L25: "Shared scope",
            L23: "Harness",
            R5: "Scope",
            L20: "Folder",
            L24: "Manifest"
        })
    // The rule in words, read from the engine's reason (its codes overlap), else from the code.
    function ruleName(code, reason) {
        const r = String(reason).toLowerCase();
        const by = [["not covered", "Exclusion line"], ["work items", "Work items"], ["has no directive", "Served row"], ["writable scope", "Shared scope"], ["differs from tracker", "Row text"], ["word for word", "Row text"], ["was not picked", "Unpicked row"], ["not picked", "Unpicked row"], ["forty characters", "Thin row"], ["definition of done", "Thin row"], ["serves no row", "Served row"], ["served row", "Served row"], ["no full block", "Served row"], ["scope line", "Exclusion line"], ["out of scope", "Exclusion line"], ["not in scope", "Exclusion line"], ["exclusion", "Exclusion line"], ["work item", "Work items"], ["overlap", "Shared scope"], ["harness", "Harness"], ["effort", "Harness"], ["route", "Harness"]];
        for (let i = 0; i < by.length; i++)
            if (r.indexOf(by[i][0]) >= 0)
                return by[i][1];
        return ruleWords[code] || "Checks";
    }
    // The engine words a refusal in the contract's form with its rule's code first; the page names the rule in words.
    function refusal(f) {
        const m = String(f.words).match(/^([A-Z][0-9]+) refuses (.*)$/);
        if (m) {
            const reason = m[2];
            return ruleName(m[1], reason) + " refuses " + m[2];
        }
        const id = f.row || "";
        return Words.refusal(ruleName(f.rule, f.words), id, words(id), f.words);
    }
    // One line per distinct refusal.
    function refusalLines() {
        const out = [];
        for (let i = 0; i < refusals.length; i++) {
            const line = refusal(refusals[i]);
            if (out.indexOf(line) < 0)
                out.push(line);
        }
        return out;
    }
    function errorWords(e) {
        if (!e)
            return "";
        return refusal({
            words: String(e.message || ""),
            rule: "",
            row: ""
        });
    }

    // The next act, by stage; the started scope only carries forward.
    readonly property string nextAct: confirmed ? "" : !started ? (scopeIds.length > 0 && name.length > 0 ? "Ask the orchestrator to write the directives" : "") : cards.length === 0 ? "Ask the orchestrator to write the directives" : !checksPass || !routed ? "Review the directives" : !closeDeclared ? "Declare the close" : "Confirm"

    function act() {
        if (nextAct === "Ask the orchestrator to write the directives")
            ask();
        else if (nextAct === "Review the directives")
            openCard = cards.length > 0 ? cards[0] : null;
        else if (nextAct === "Declare the close")
            closeOpen = true;
        else if (nextAct === "Confirm")
            confirm();
    }
    function ask() {
        const go = () => {
            const text = "Write the directives for the campaign " + name + " from its rows, read in full from the store: " + scopeIds.join(", ") + ". Add each with qream setup add-directive --root " + root + ".";
            if (slot.children.length > 0 && typeof slot.children[0].ask === "function")
                slot.children[0].ask(text);
            else
                Backend.say("Open the orchestrator's terminal");
        };
        if (started) {
            go();
            return;
        }
        const ids = grid.pickedList();
        const target = folderBase + "/" + folderName();
        Backend.request("setup.new", {
            name: name,
            root: target,
            rows: ids,
            groups: []
        }, (ok, result) => {
            if (!ok) {
                Backend.say(errorWords(result));
                return;
            }
            setup.root = target;
            setup.save();
            setup.poll();
            go();
        });
    }
    // The draft and its checks are read every second (a directive's text can change while the board does not).
    property string draftKey: ""
    property int reads: 0
    function poll() {
        if (!started)
            return;
        Backend.request("setup.draft.get", {
            root: root
        }, (ok, result) => {
            if (!ok)
                return;
            const key = JSON.stringify(result);
            const changed = key !== setup.draftKey;
            setup.draftKey = key;
            setup.draft = result;
            setup.draftAt = Date.now();
            for (let i = 0; i < result.cards.length; i++) {
                const c = result.cards[i];
                if (c.kind && !setup.routes[c.wave])
                    setup.adoptRoute(c);
            }
            setup.reads += 1;
            if (changed || setup.reads > 0)
                setup.check();
        });
    }
    function check() {
        Backend.request("setup.check", {
            root: root
        }, (ok, result) => {
            if (!ok)
                return;
            setup.refusals = result.refusals;
            setup.checked = true;
            for (let i = 0; i < result.refusals.length; i++)
                setup.learn(result.refusals[i].row);
        });
    }
    // A refused row outside the loaded grid still needs its first six words.
    function learn(id) {
        if (!id || grid.rowById(id) || rowWords[id] !== undefined)
            return;
        Backend.request("store.row", {
            id: id
        }, (ok, r) => {
            const w = Object.assign({}, setup.rowWords);
            w[id] = ok ? grid.slug(r) : "";
            setup.rowWords = w;
        });
    }
    function adoptRoute(card) {
        const args = card.args || [];
        const at = k => {
            const i = args.indexOf(k);
            return i >= 0 && i + 1 < args.length ? args[i + 1] : "";
        };
        let effort = at("--effort");
        const c = args.find(a => a.indexOf("model_reasoning_effort=") === 0);
        if (c)
            effort = c.split("=")[1].replace(/"/g, "");
        const r = Object.assign({}, routes);
        r[card.wave] = {
            kind: card.kind,
            model: at("--model") || at("-m"),
            effort: effort || "low"
        };
        routes = r;
    }
    function route(wave, kind, model, effort) {
        const r = Object.assign({}, routes);
        r[wave] = {
            kind: kind,
            model: model,
            effort: effort
        };
        routes = r;
        Backend.request("setup.manifest", {
            root: root,
            wave: wave,
            kind: kind,
            model: model,
            effort: effort
        }, (ok, result) => {
            const f = Object.assign({}, setup.routeRefusals);
            if (ok)
                delete f[wave];
            else
                f[wave] = errorWords(result);
            setup.routeRefusals = f;
        });
    }
    function harness(kind) {
        return harnesses.find(h => h.kind === kind) || null;
    }
    function confirm() {
        if (confirmReason.length > 0)
            return;
        Backend.request("setup.confirm", {
            root: root,
            close: {
                last_wave: lastWave,
                expected_close: expected,
                by: "operator"
            }
        }, (ok, result) => {
            if (!ok) {
                Backend.say(errorWords(result));
                return;
            }
            if (result.ok === false) {
                setup.refusals = result.refusals;
                return;
            }
            // Publishing registers the campaign for the list; its trigger stays the operator's.
            Backend.request("campaign.up", {
                name: setup.name,
                root: setup.root,
                title: setup.name
            }, (up, r) => {
                if (!up) {
                    Backend.say(errorWords(r));
                    return;
                }
                setup.confirmed = true;
                setup.save();
                Backend.listCampaigns();
            });
        });
    }
    function openDirective(card) {
        openCard = card;
        openText = "";
        const l = {};
        for (const id in card.rows)
            l[id] = true;
        grid.lit = l;
        Backend.request("setup.show", {
            root: root
        }, (ok, c) => {
            if (!ok)
                return;
            for (let i = 0; i < c.waves.length; i++)
                for (let j = 0; j < c.waves[i].directives.length; j++)
                    if (c.waves[i].directives[j].id === card.id)
                        setup.openText = c.waves[i].directives[j].text;
        });
    }
    // A fresh pick after a started scope begins a new one; the draft stays on disk.
    function freshScope() {
        root = "";
        draft = null;
        refusals = [];
        checked = false;
        routes = {};
        routeRefusals = {};
        closeDeclared = false;
        confirmed = false;
        openCard = null;
        name = "";
        save();
    }

    function save() {
        stateView.setText(JSON.stringify({
            name: name,
            root: root,
            lastWave: lastWave,
            expected: expected,
            closeDeclared: closeDeclared,
            confirmed: confirmed
        }) + "\n");
    }
    function restore(text) {
        try {
            const s = JSON.parse(text);
            name = s.name || "";
            root = s.root || "";
            lastWave = s.lastWave || 0;
            expected = s.expected || "";
            closeDeclared = !!s.closeDeclared;
            confirmed = !!s.confirmed;
            poll();
        } catch (e) {}
    }

    FileView {
        id: stateView
        path: setup.stateFile
        atomicWrites: true
        printErrors: false
        onLoaded: setup.restore(text())
    }

    // The installed harnesses: the engine's profiles when it serves them, else a look for each command once.
    Process {
        id: look
        command: ["sh", "-c", "for c in claude codex opencode; do command -v $c >/dev/null && echo $c; done"]
        stdout: StdioCollector {
            onStreamFinished: {
                const have = text.split("\n");
                setup.harnesses = [
                    { kind: "claude", word: "Claude Code", installed: have.indexOf("claude") >= 0, models: ["claude-opus-5-5", "sonnet"] },
                    { kind: "codex", word: "Codex", installed: have.indexOf("codex") >= 0, models: ["gpt-6.1-sol"] },
                    { kind: "opencode", word: "OpenCode", installed: have.indexOf("opencode") >= 0, models: ["anthropic/claude-opus-5-5"] }
                ];
            }
        }
    }
    Component.onCompleted: {
        Backend.request("profile.get", {}, (ok, result) => {
            if (ok && result.harnesses)
                setup.harnesses = result.harnesses;
            else
                look.running = true;
        });
        Backend.request("seat.harnesses", {}, (ok, list) => {
            if (!ok || !Array.isArray(list))
                return;
            const e = {};
            for (const h of list)
                e[h.kind] = h.efforts || [];
            setup.efforts = e;
        });
    }

    // The draft: read on every campaign event and every second while the page is open.
    Timer {
        interval: 1000
        repeat: true
        running: setup.started && setup.visible
        onTriggered: setup.poll()
    }
    Connections {
        target: Backend
        function onCampaignsChanged() {
            setup.poll();
        }
        function onStatesChanged() {
            setup.poll();
        }
    }

    Parts.Grid {
        id: grid
        anchors { left: parent.left; top: parent.top; bottom: parent.bottom; right: column.left; margins: 8 }
        onPickChanged: {
            if (setup.started && grid.pickedList().length > 0 && !grid.pickedList().every(id => setup.scopeIds.indexOf(id) >= 0))
                setup.freshScope();
        }
        onRowOpened: row => setup.rowInfo = row
    }

    // The campaign column: name, scope, next act, wave board, checks.
    Rectangle {
        id: column
        anchors { top: parent.top; bottom: parent.bottom; right: slot.left; margins: 8 }
        width: 380
        color: Theme.surface
        radius: Theme.radius
        border { width: 1; color: Theme.border }

        Flickable {
            anchors { fill: parent; margins: 10 }
            contentHeight: body.implicitHeight
            clip: true
            boundsBehavior: Flickable.StopAtBounds

            Column {
                id: body
                width: parent.width
                spacing: 10

                Text {
                    visible: setup.scopeIds.length === 0
                    width: parent.width
                    wrapMode: Text.WordWrap
                    text: "Pick rows or an outcome group in the grid to start a campaign"
                    color: Theme.quiet
                    font { family: Theme.font; pixelSize: Theme.normal }
                }

                // The campaign's name.
                Rectangle {
                    visible: setup.scopeIds.length > 0
                    width: parent.width
                    height: 28
                    radius: Theme.radius
                    color: Theme.sunken
                    border { width: 1; color: Theme.border }
                    TextInput {
                        id: nameInput
                        anchors { fill: parent; leftMargin: 8; rightMargin: 8 }
                        verticalAlignment: TextInput.AlignVCenter
                        text: setup.name
                        readOnly: setup.started
                        validator: RegularExpressionValidator { regularExpression: /[A-Za-z0-9_-]{0,64}/ }
                        color: Theme.foreground
                        font { family: Theme.font; pixelSize: Theme.large }
                        onTextEdited: setup.name = text
                    }
                    Text {
                        anchors { fill: parent; leftMargin: 8 }
                        verticalAlignment: Text.AlignVCenter
                        visible: nameInput.text.length === 0
                        text: "Campaign name"
                        color: Theme.quiet
                        font { family: Theme.font; pixelSize: Theme.large }
                    }
                }

                // The scope: one line per picked row, under its group's words.
                Text {
                    visible: setup.scopeIds.length > 0
                    text: "Scope · " + setup.scopeIds.length + (setup.scopeIds.length === 1 ? " row" : " rows")
                    color: Theme.quiet
                    font { family: Theme.font; pixelSize: Theme.small; weight: Font.DemiBold }
                }
                Repeater {
                    model: setup.groups()
                    Column {
                        id: grp
                        required property var modelData
                        width: body.width
                        MouseArea {
                            width: parent.width
                            height: gw.implicitHeight + 4
                            cursorShape: Qt.PointingHandCursor
                            onClicked: {
                                const f = Object.assign({}, setup.folded);
                                f[grp.modelData.group] = !f[grp.modelData.group];
                                setup.folded = f;
                            }
                            Text {
                                id: gw
                                width: parent.width
                                wrapMode: Text.WordWrap
                                maximumLineCount: 2
                                elide: Text.ElideRight
                                text: (setup.folded[grp.modelData.group] ? "▸ " : "▾ ") + setup.groupWords(grp.modelData.group) + " · " + grp.modelData.ids.length
                                color: Theme.foreground
                                font { family: Theme.font; pixelSize: Theme.small; weight: Font.DemiBold }
                            }
                        }
                        Repeater {
                            model: setup.folded[grp.modelData.group] ? [] : grp.modelData.ids
                            MouseArea {
                                id: ref
                                required property string modelData
                                width: grp.width
                                height: 18
                                hoverEnabled: true
                                onClicked: grid.showRow(modelData)
                                Text {
                                    anchors.fill: parent
                                    leftPadding: 12
                                    elide: Text.ElideRight
                                    text: "<font face=\"" + Theme.mono + "\">" + ref.modelData + "</font>  " + setup.words(ref.modelData)
                                    textFormat: Text.StyledText
                                    color: Theme.foreground
                                    font { family: Theme.font; pixelSize: Theme.small }
                                }
                                Hint {
                                    shown: ref.containsMouse
                                    words: { const r = grid.rowById(ref.modelData); return r ? grid.purposeOf(r) : ""; }
                                }
                            }
                        }
                    }
                }

                // The next act, in words.
                Button {
                    visible: setup.nextAct.length > 0
                    word: setup.nextAct
                    tone: Theme.citium
                    enabledWord: setup.nextAct !== "Confirm" || setup.confirmReason.length === 0
                    hint: setup.nextAct === "Confirm" ? setup.confirmReason : ""
                    onClicked: if (enabledWord) setup.act()
                }

                // Published: Prepared in the list, triggered from there.
                Text {
                    visible: setup.confirmed
                    width: parent.width
                    wrapMode: Text.WordWrap
                    text: "Prepared · Closes after wave " + setup.lastWave + " · expected " + setup.expected.slice(11, 16)
                    color: Theme.muted
                    font { family: Theme.font; pixelSize: Theme.normal }
                }

                // The wave board.
                Repeater {
                    model: setup.waves
                    Column {
                        id: wave
                        required property int modelData
                        readonly property var route: setup.routes[modelData] || null
                        width: body.width
                        spacing: 4
                        Text {
                            text: "Wave " + wave.modelData
                            color: Theme.foreground
                            font { family: Theme.font; pixelSize: Theme.normal; weight: Font.DemiBold }
                        }
                        // Harness, model and effort for the wave, from the installed harnesses.
                        Flow {
                            width: parent.width
                            spacing: 4
                            Repeater {
                                model: setup.harnesses
                                Button {
                                    required property var modelData
                                    word: modelData.word
                                    enabledWord: modelData.installed && !setup.confirmed
                                    hint: modelData.installed ? "" : "Not installed"
                                    selected: !!wave.route && wave.route.kind === modelData.kind
                                    tone: selected ? Theme.citium : Theme.foreground
                                    onClicked: if (enabledWord) setup.route(wave.modelData, modelData.kind, modelData.models[0], wave.route ? wave.route.effort : "low")
                                }
                            }
                        }
                        Flow {
                            visible: !!wave.route
                            width: parent.width
                            spacing: 4
                            Rectangle {
                                width: 170
                                height: 22
                                radius: Theme.radius
                                color: Theme.sunken
                                border { width: 1; color: Theme.border }
                                TextInput {
                                    id: model
                                    anchors { fill: parent; leftMargin: 6; rightMargin: 6 }
                                    verticalAlignment: TextInput.AlignVCenter
                                    text: wave.route ? wave.route.model : ""
                                    readOnly: setup.confirmed
                                    color: Theme.foreground
                                    clip: true
                                    font { family: Theme.mono; pixelSize: Theme.small }
                                    onEditingFinished: if (wave.route && text !== wave.route.model) setup.route(wave.modelData, wave.route.kind, text, wave.route.effort)
                                }
                            }
                            Repeater {
                                model: wave.route ? (setup.efforts[wave.route.kind] || []) : []
                                Button {
                                    required property string modelData
                                    word: modelData.charAt(0).toUpperCase() + modelData.slice(1)
                                    selected: !!wave.route && wave.route.effort === modelData
                                    tone: selected ? Theme.citium : Theme.foreground
                                    enabledWord: !setup.confirmed
                                    onClicked: if (wave.route && enabledWord) setup.route(wave.modelData, wave.route.kind, wave.route.model, modelData)
                                }
                            }
                        }
                        Text {
                            visible: !!setup.routeRefusals[wave.modelData]
                            width: parent.width
                            wrapMode: Text.WordWrap
                            text: setup.routeRefusals[wave.modelData] || ""
                            color: Theme.red
                            font { family: Theme.font; pixelSize: Theme.small }
                        }
                        // Directive cards as they land.
                        Repeater {
                            model: setup.cards.filter(c => c.wave === wave.modelData)
                            MouseArea {
                                id: card
                                required property var modelData
                                width: wave.width
                                height: cardText.implicitHeight + 12
                                cursorShape: Qt.PointingHandCursor
                                onClicked: setup.openDirective(modelData)
                                Rectangle {
                                    anchors.fill: parent
                                    radius: Theme.radius
                                    color: setup.openCard && setup.openCard.id === card.modelData.id ? Theme.hover : Theme.sunken
                                    border { width: 1; color: setup.refusals.some(f => f.file === card.modelData.directive) ? Theme.red : Theme.border }
                                }
                                Text {
                                    id: cardText
                                    anchors { left: parent.left; right: parent.right; top: parent.top; margins: 6 }
                                    wrapMode: Text.WordWrap
                                    text: card.modelData.title + "\n" + Object.keys(card.modelData.rows).join(" ") + "\n" + (setup.refusals.some(f => f.file === card.modelData.directive) ? "Refused" : setup.checked ? "Checks pass" : "")
                                    color: Theme.foreground
                                    font { family: Theme.font; pixelSize: Theme.small }
                                }
                            }
                        }
                    }
                }
                Text {
                    visible: setup.started && setup.cards.length === 0
                    text: "No waves yet"
                    color: Theme.quiet
                    font { family: Theme.font; pixelSize: Theme.small }
                }

                // The close: last wave and expected time.
                Column {
                    visible: setup.closeOpen || setup.closeDeclared
                    width: parent.width
                    spacing: 4
                    Row {
                        spacing: 6
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            text: "Closes after wave"
                            color: Theme.foreground
                            font { family: Theme.font; pixelSize: Theme.small }
                        }
                        Repeater {
                            model: setup.waves
                            Button {
                                required property int modelData
                                word: String(modelData)
                                selected: setup.lastWave === modelData
                                enabledWord: !setup.confirmed
                                onClicked: if (enabledWord) { setup.lastWave = modelData; setup.closeDeclared = false; }
                            }
                        }
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            text: "expected"
                            color: Theme.foreground
                            font { family: Theme.font; pixelSize: Theme.small }
                        }
                        Rectangle {
                            width: 56
                            height: 22
                            radius: Theme.radius
                            color: Theme.sunken
                            border { width: 1; color: Theme.border }
                            TextInput {
                                id: closeTime
                                anchors { fill: parent; leftMargin: 6 }
                                verticalAlignment: TextInput.AlignVCenter
                                inputMask: "99:99"
                                text: setup.expected.length >= 16 ? setup.expected.slice(11, 16) : "18:00"
                                readOnly: setup.confirmed
                                color: Theme.foreground
                                font { family: Theme.mono; pixelSize: Theme.small }
                            }
                        }
                        Button {
                            word: "Declare the close"
                            enabledWord: setup.lastWave > 0 && !setup.confirmed
                            selected: setup.closeDeclared
                            onClicked: {
                                if (!enabledWord)
                                    return;
                                const d = new Date();
                                const pad = n => (n < 10 ? "0" : "") + n;
                                const off = -d.getTimezoneOffset();
                                const tz = (off >= 0 ? "+" : "-") + pad(Math.floor(Math.abs(off) / 60)) + ":" + pad(Math.abs(off) % 60);
                                setup.expected = d.getFullYear() + "-" + pad(d.getMonth() + 1) + "-" + pad(d.getDate()) + "T" + closeTime.text + tz;
                                setup.closeDeclared = true;
                                setup.save();
                            }
                        }
                    }
                }

                // The checks, in the contract's words.
                Text {
                    visible: setup.started && setup.checked && setup.cards.length > 0
                    width: parent.width
                    wrapMode: Text.WordWrap
                    text: setup.refusals.length === 0 ? "Checks pass" : setup.refusalLines().join("\n")
                    color: setup.refusals.length === 0 ? Theme.green : Theme.red
                    font { family: Theme.font; pixelSize: Theme.small }
                }
                Button {
                    visible: setup.started && !setup.confirmed
                    word: "Confirm"
                    enabledWord: setup.confirmReason.length === 0
                    hint: setup.confirmReason
                    onClicked: if (enabledWord) setup.confirm()
                }

                // A row clicked in the grid: its directive, wave and campaign.
                Text {
                    visible: !!setup.rowInfo
                    width: parent.width
                    wrapMode: Text.WordWrap
                    text: {
                        const r = setup.rowInfo;
                        if (!r)
                            return "";
                        const cs = setup.cardsOf(r.id);
                        if (cs.length > 0)
                            return "On: " + grid.slug(r) + " (" + r.id + ") · " + cs.map(c => c.title + " · Wave " + c.wave).join("; ") + " · " + setup.name;
                        return r.campaign ? "On: " + grid.slug(r) + " (" + r.id + ") · " + r.campaign : "";
                    }
                    color: Theme.quiet
                    font { family: Theme.font; pixelSize: Theme.small }
                }
            }
        }
    }

    // The opened directive, word for word.
    Rectangle {
        visible: !!setup.openCard
        anchors { left: grid.left; right: grid.right; bottom: grid.bottom; top: grid.verticalCenter }
        color: Theme.surface
        radius: Theme.radius
        border { width: 1; color: Theme.border }
        Q.ScrollView {
            id: directiveView
            anchors { fill: parent; margins: 10 }
            clip: true
            Text {
                width: directiveView.availableWidth
                wrapMode: Text.WordWrap
                text: setup.openText
                color: Theme.foreground
                font { family: Theme.font; pixelSize: Theme.small }
            }
        }
        MouseArea {
            anchors { right: parent.right; top: parent.top; margins: 4 }
            width: 22
            height: 22
            cursorShape: Qt.PointingHandCursor
            onClicked: {
                setup.openCard = null;
                grid.lit = {};
            }
            Text {
                anchors.centerIn: parent
                text: "✕"
                color: Theme.quiet
            }
        }
    }

    // The orchestrator column's slot.
    Item {
        id: slot
        anchors { top: parent.top; bottom: parent.bottom; right: parent.right }
        width: children.length > 0 ? 360 : 0
    }

    // Automation for checks and captures: `qs ipc call setup <function> <arg>`.
    IpcHandler {
        target: "setup"
        function state(): string { return setup.report() }
        function pick(ids: string): void {
            const p = {};
            ids.split(",").filter(s => s.length > 0).forEach(id => p[id] = true);
            grid.picked = p;
            grid.pickChanged();
        }
        function name(n: string): void { setup.name = n }
        function act(): string { const a = setup.nextAct; setup.act(); return a }
        function route(spec: string): void { const s = spec.split(" "); setup.route(Number(s[0]), s[1], s[2], s[3]) }
        function declare(spec: string): void { const s = spec.split(" "); setup.lastWave = Number(s[0]); setup.closeOpen = true; setup.expected = s[1]; setup.closeDeclared = true; setup.save() }
        function confirm(): void { setup.confirm() }
        function open(id: string): void { const c = setup.cards.find(x => x.id === id); if (c) setup.openDirective(c) }
        function row(id: string): void { const r = grid.rowById(id); if (r) { grid.focusId = id; setup.rowInfo = r; grid.showRow(id) } }
        function filter(st: string): void { grid.status = st }
        function poll(): void { setup.poll() }
        function has(id: string): string { const r = grid.shown.find(x => x.id === id); return r ? r.status : "" }
    }

    function report() {
        return JSON.stringify({
            firstPaintMs: grid.firstPaintMs,
            lastRefreshMs: grid.lastRefreshMs,
            rows: grid.rows.length,
            total: grid.total,
            revision: grid.revision,
            picked: grid.pickedList(),
            scope: scopeIds,
            next: nextAct,
            cards: cards.length,
            board: cards.map(c => c.id + "@" + c.wave).join(" "),
            refusals: refusalLines(),
            confirm: confirmReason,
            confirmed: confirmed
        });
    }
}
