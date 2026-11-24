pragma Singleton
import QtQuick
import Quickshell
import Quickshell.Io

// The window's one link to the engine: the §2 envelope over the daemon's socket, request ids with
// a pending map and timeouts, the subscription, and a stale flag when nothing is heard for more than 3 s.
Singleton {
    id: backend

    readonly property string socketPath: Quickshell.env("QREAM_SOCKET") || (Quickshell.env("XDG_RUNTIME_DIR") + "/qream/engine.sock")
    readonly property string windowFile: Quickshell.env("QREAM_WINDOW_STATE") || (Quickshell.env("HOME") + "/.local/state/qream/window.json")
    // The current link; replaced by a fresh one for every attempt while the engine is away.
    property var socket: null
    property double socketMade: 0
    readonly property bool connected: socket !== null && socket.connected
    property string build: ""
    property var campaigns: []
    property var states: ({})
    property var refusals: ({})
    property var view: []
    property string notice: ""
    property bool leftFolded: false
    property int sideWidth: 280
    property bool muted: false
    property int volume: 60
    property bool themeFollow: true
    // The notice slot: the last line for ten seconds, and a short list of the latest lines.
    property var notices: []
    readonly property string soundFile: Quickshell.env("QREAM_NOTICE_SOUND") || "/usr/share/sounds/freedesktop/stereo/message.oga"
    readonly property var soundCommand: {
        const v = Quickshell.env("QREAM_SOUND_COMMAND");
        if (v !== null && v !== undefined)
            return String(v).split(" ").filter(s => s.length > 0);
        return ["paplay"];
    }
    property string orchestratorSide: "Right"
    // The orchestrator column: open or closed, its own width, and the campaign whose seat it shows.
    property bool seatOpen: false
    property int seatWidth: 460
    property string seatCampaign: ""
    // Text size: one choice for every pane, or per pane when the lock is open (window state, not the daemon).
    // The default starts from the operator's terminal font (points to pixels) and grows with the window.
    readonly property string terminalConfig: Quickshell.env("QREAM_TERMINAL_CONFIG") || (Quickshell.env("HOME") + "/.config/alacritty/alacritty.toml")
    property string termFamily: Theme.mono
    property int termPoints: 9
    property int windowHeight: 960
    readonly property int defaultSize: Math.max(10, Math.min(24, Math.max(Math.round(termPoints * 96 / 72), Math.round(windowHeight / 80))))
    property int textSize: 0
    property bool sizeLock: true
    property var paneSizes: ({})
    property string focusedPane: ""
    property var viewWords: ({})
    property var viewers: ({})
    signal reconnected
    // One press: reconnect if the engine dropped, then every panel re-finds a live pane and every terminal re-attaches.
    // It never starts, stops or messages an agent.
    signal refreshed
    function refreshAll() {
        if (!connected)
            reconnect();
        for (const n of view)
            loadState(n);
        refreshed();
    }
    signal sessionsChanged
    onStaleChanged: if (!stale)
        sessionsChanged()

    function clampSize(n) {
        return Math.max(10, Math.min(24, n));
    }
    function paneSize(pane) {
        return paneSizes[pane] || textSize || defaultSize;
    }
    // step: +1 larger, -1 smaller, 0 back to the default; the lock decides every pane or the focused one.
    function stepSize(step) {
        if (sizeLock || !focusedPane) {
            textSize = step === 0 ? 0 : clampSize((textSize || defaultSize) + step);
            if (sizeLock)
                paneSizes = {};
        } else {
            const s = Object.assign({}, paneSizes);
            if (step === 0)
                delete s[focusedPane];
            else
                s[focusedPane] = clampSize(paneSize(focusedPane) + step);
            paneSizes = s;
        }
        save();
    }
    function setView(name, word) {
        const v = Object.assign({}, viewWords);
        v[name] = word;
        viewWords = v;
        save();
    }
    function readTerminalConfig(text) {
        const family = text.match(/^\s*normal\s*=\s*\{[^}]*family\s*=\s*"([^"]+)"/m);
        const size = text.match(/^\s*size\s*=\s*([0-9.]+)/m);
        if (family && Qt.fontFamilies().indexOf(family[1]) >= 0)
            termFamily = family[1];
        if (size)
            termPoints = Math.round(parseFloat(size[1]));
    }
    // Selection asked from outside the panel (automation); the panel follows it.
    property var picks: ({})
    function pick(name, task) {
        const p = Object.assign({}, picks);
        p[name] = task;
        picks = p;
    }
    // Scroll every shown viewer of this window to a history offset (automation for checks).
    function scrollShown(n) {
        for (const id in viewers)
            if (viewers[id].visible)
                viewers[id].scrollTo(n);
    }
    function frameReport() {
        const out = [];
        for (const id in viewers) {
            const v = viewers[id];
            out.push({
                viewer: id,
                pane: v.pane,
                cols: v.cols,
                rows: v.rowCount,
                offset: v.offset,
                history: v.scrollbackLen,
                fullFrames: v.fullFrames,
                lastStampToFrameMs: v.lastFrameMs,
                lastApplyToFrameMs: v.lastApplyMs,
                worstApplyToFrameMs: v.worstFrameMs,
                worstStampToFrameMs: v.worstStampMs,
                // The shown words, to compare with `qream term pane text` for the same pane.
                text: Array.from({
                    length: v.rowCount
                }, (_, y) => v.rowText(y)).join("\n").replace(/\s+$/, "")
            });
        }
        return out;
    }
    function addViewer(id, item) {
        const v = viewers;
        v[id] = item;
        viewers = v;
    }
    function dropViewer(id) {
        const v = viewers;
        delete v[id];
        viewers = v;
    }

    property double lastHeard: 0
    property int ageSeconds: 0
    readonly property bool stale: lastHeard > 0 && ageSeconds >= 3

    // The window's shared selection: the Graph page sets it, Review narrows its replay to it.
    property string selectedCampaign: ""
    property string selectedTask: ""
    property var graph: null

    signal storeChanged

    property int counter: 0
    property var pending: ({})
    property bool restored: false

    // Campaigns in arrival order; finished ones leave the live list for Review.
    readonly property var live: arrival(campaigns.filter(c => !finished(c.name)))
    readonly property var done: arrival(campaigns.filter(c => finished(c.name)))
    // Every registered campaign in arrival order, Stopped and Finished included: the left panel's list.
    // A campaign taken down with `campaign down` is no longer registered and leaves it.
    readonly property var listed: arrival(campaigns.filter(c => c.status !== "down"))
    // True once the engine has told the window which campaigns are registered.
    property bool listedOnce: false
    // The panels saved by an earlier session, held back until the registry says which still exist.
    property var savedView: []

    function arrival(list) {
        return list.slice().sort((a, b) => a.created < b.created ? -1 : a.created > b.created ? 1 : (a.name < b.name ? -1 : 1));
    }
    function campaign(name) {
        for (let i = 0; i < campaigns.length; i++)
            if (campaigns[i].name === name)
                return campaigns[i];
        return null;
    }
    function registered(name) {
        const c = campaign(name);
        return c !== null && c.status !== "down";
    }
    function finished(name) {
        const s = states[name];
        if (s)
            return s.word === "finished";
        const c = campaign(name);
        return c !== null && c.status === "finished";
    }
    function inView(name) {
        return view.indexOf(name) >= 0;
    }

    function request(cmd, args, done, timeoutMs) {
        if (!connected) {
            if (done)
                done(false, {
                    code: "offline",
                    message: Words.lost
                });
            return;
        }
        counter += 1;
        const id = "w" + counter;
        const p = pending;
        p[id] = {
            done: done,
            at: Date.now(),
            timeout: timeoutMs || 10000
        };
        pending = p;
        socket.write(JSON.stringify({
            v: 1,
            id: id,
            cmd: cmd,
            args: args || {}
        }) + "\n");
        socket.flush();
    }

    function receive(line) {
        let msg;
        try {
            msg = JSON.parse(line);
        } catch (e) {
            return;
        }
        lastHeard = Date.now();
        ageSeconds = 0;
        if (msg.event !== undefined) {
            event(msg.event, msg.data || {}, msg.at);
            return;
        }
        const p = pending[msg.id];
        if (!p)
            return;
        delete pending[msg.id];
        if (p.done)
            p.done(msg.ok === true, msg.ok ? msg.result : msg.error);
    }

    function event(name, data, at) {
        if (name === "pane") {
            const item = viewers[data.viewer];
            if (item)
                item.damage(data, at);
            return;
        }
        if (name === "hello") {
            build = data.build || "";
            request("subscribe", {
                topics: ["campaigns", "campaign", "task", "notice", "pane", "session", "agent", "store"]
            });
            reconnected();
            listCampaigns();
            for (let i = 0; i < view.length; i++)
                refresh(view[i]);
        } else if (name === "campaigns") {
            // The engine's registry as it is now: the list is replaced, never re-read.
            if (Array.isArray(data))
                takeCampaigns(data);
        } else if (name === "campaign") {
            if (data.campaign && data.word) {
                const s = Object.assign({}, states);
                s[data.campaign] = data;
                states = s;
            } else if (!data.campaigns) {
                // The registry's own change arrives as `campaigns`; any other shape is read again.
                listCampaigns();
            }
        } else if (name === "task") {
            if (data.campaign)
                refresh(data.campaign);
        } else if (name === "session" || name === "agent") {
            sessionsChanged();
        } else if (name === "store") {
            storeChanged();
        } else if (name === "notice") {
            const line = data.message || data.line || data.text || "";
            // The daemon's own catch-up line is for the window, not for him: read the states again.
            if (line === "State events were missed; read states again") {
                listCampaigns();
                return;
            }
            announce(line);
        }
    }

    // First paint (and a missed-events catch-up) only; the `campaigns` event keeps the list current.
    function listCampaigns() {
        request("campaigns.list", {}, (ok, result) => {
            if (ok)
                takeCampaigns(result);
        });
    }
    function takeCampaigns(list) {
        const before = {};
        for (let i = 0; i < campaigns.length; i++)
            before[campaigns[i].name] = campaigns[i].status;
        campaigns = list;
        listedOnce = true;
        for (let i = 0; i < list.length; i++)
            if (!states[list[i].name] || before[list[i].name] !== list[i].status)
                loadState(list[i].name);
        dropUnregistered();
    }
    // A panel stays only for a campaign registered now; the view never shows one by a bare id.
    function dropUnregistered() {
        if (!listedOnce || !restored)
            return;
        const wanted = savedView.length > 0 ? view.concat(savedView.filter(n => view.indexOf(n) < 0)).slice(0, 4) : view;
        const kept = wanted.filter(n => registered(n));
        savedView = [];
        if (kept.length !== view.length || kept.some((n, i) => view[i] !== n)) {
            view = kept;
            save();
        }
        if (seatCampaign && !registered(seatCampaign))
            seatCampaign = "";
    }
    function loadState(name) {
        request("states", {
            name: name
        }, (ok, result) => {
            const r = Object.assign({}, refusals);
            if (ok) {
                const s = Object.assign({}, states);
                s[name] = result;
                states = s;
                delete r[name];
            } else {
                r[name] = result.message || "";
            }
            refusals = r;
        });
    }
    function refresh(name) {
        loadState(name);
    }

    function addToView(name) {
        if (!registered(name))
            return false;
        if (inView(name))
            return true;
        if (view.length >= 4) {
            say(Words.fifthRefused);
            return false;
        }
        view = view.concat([name]);
        refresh(name);
        save();
        return true;
    }
    function removeFromView(name) {
        view = view.filter(n => n !== name);
        save();
    }
    function toggleView(name) {
        if (inView(name))
            removeFromView(name);
        else
            addToView(name);
    }
    function control(name, action, target, done) {
        request("campaign.control", {
            name: name,
            action: action,
            target: target
        }, (ok, result) => {
            if (!ok)
                say(result.message || "");
            if (done)
                done(ok);
        });
    }
    function trigger(name) {
        request("campaign.trigger", {
            name: name
        }, (ok, result) => {
            if (!ok)
                say(result.message || "");
        });
    }
    function select(name, task) {
        selectedCampaign = name;
        selectedTask = task;
    }
    // One tracker row from the store, read through the daemon (CONTRACTS §2 `store.row`).
    function row(id, done) {
        request("store.row", {
            id: id
        }, (ok, result) => done(ok, result));
    }
    // A line in the slot without a sound: the window's own refusals.
    function say(line) {
        if (!line)
            return;
        notice = line;
        noticeTimer.restart();
    }
    // The daemon's notice: one line in the slot, kept in the short list, a sound unless muted.
    function announce(line) {
        if (!line)
            return;
        say(line);
        notices = [{
                at: new Date(),
                line: line
            }].concat(notices).slice(0, 8);
        if (!muted && soundCommand.length > 0) {
            sound.command = soundCommand.concat(["--volume=" + Math.round(volume * 655.36), soundFile]);
            sound.running = true;
        }
    }

    function save() {
        if (!restored)
            return;
        windowState.setText(JSON.stringify({
            view: listedOnce ? view : view.concat(savedView).slice(0, 4),
            leftFolded: leftFolded,
            sideWidth: sideWidth,
            muted: muted,
            volume: volume,
            themeFollow: themeFollow,
            orchestratorSide: orchestratorSide,
            textSize: textSize,
            sizeLock: sizeLock,
            paneSizes: paneSizes,
            viewWords: viewWords,
            seatOpen: seatOpen,
            seatWidth: seatWidth,
            seatCampaign: seatCampaign
        }, null, 1) + "\n");
    }
    function restore(text) {
        try {
            const s = JSON.parse(text);
            // Only names the operator added; unregistered ones drop as soon as the list is known.
            savedView = (s.view || []).filter(n => typeof n === "string" && n.length > 0).slice(0, 4);
            leftFolded = !!s.leftFolded;
            sideWidth = s.sideWidth || 280;
            muted = !!s.muted;
            volume = s.volume === undefined ? 60 : Math.max(0, Math.min(100, s.volume));
            themeFollow = s.themeFollow !== false;
            orchestratorSide = s.orchestratorSide || "Right";
            textSize = s.textSize || 0;
            sizeLock = s.sizeLock !== false;
            paneSizes = s.paneSizes || {};
            viewWords = s.viewWords || {};
            seatOpen = !!s.seatOpen;
            seatWidth = s.seatWidth || 460;
            seatCampaign = s.seatCampaign || "";
        } catch (e) {}
        restored = true;
        dropUnregistered();
    }

    onLeftFoldedChanged: save()
    onSideWidthChanged: save()
    onMutedChanged: save()
    onVolumeChanged: save()
    onThemeFollowChanged: save()
    onOrchestratorSideChanged: save()
    onSizeLockChanged: save()
    onSeatOpenChanged: save()
    onSeatWidthChanged: save()
    onSeatCampaignChanged: save()

    FileView {
        path: backend.terminalConfig
        printErrors: false
        onLoaded: backend.readTerminalConfig(text())
    }

    FileView {
        id: windowState
        path: backend.windowFile
        atomicWrites: true
        printErrors: false
        onLoaded: backend.restore(text())
        onLoadFailed: backend.restored = true
    }

    // A Quickshell Socket whose connect failed keeps its dead local socket and ignores every later
    // `connected = true`, so a link that lost the engine never came back after a restart. Each
    // attempt therefore makes a fresh Socket and drops the old one; the engine's hello on the new
    // link subscribes and reads state again.
    function reconnect() {
        if (connected)
            return;
        const old = socket;
        socketMade = Date.now();
        socket = link.createObject(backend);
        if (old)
            old.destroy();
    }
    function linkChanged(s) {
        if (s !== socket || s.connected)
            return;
        build = "";
        const p = pending;
        for (const id in p)
            if (p[id].done)
                p[id].done(false, {
                    code: "offline",
                    message: Words.lost
                });
        pending = {};
    }
    Component.onCompleted: reconnect()

    Component {
        id: link
        Socket {
            id: s
            path: backend.socketPath
            connected: true
            parser: SplitParser {
                onRead: data => {
                    if (s === backend.socket)
                        backend.receive(data);
                }
            }
            onConnectedChanged: backend.linkChanged(s)
        }
    }

    Process {
        id: sound
    }

    Timer {
        id: noticeTimer
        interval: 10000
        onTriggered: backend.notice = ""
    }

    // A quarter-second tick: bounded attach timeouts, freshness, a one-second ping and reconnect.
    Timer {
        interval: 250
        running: true
        repeat: true
        property int n: 0
        onTriggered: {
            n += 1;
            const now = Date.now();
            if (backend.lastHeard > 0)
                backend.ageSeconds = Math.floor((now - backend.lastHeard) / 1000);
            const p = backend.pending;
            for (const id in p) {
                if (now - p[id].at >= p[id].timeout) {
                    const done = p[id].done;
                    delete p[id];
                    if (done)
                        done(false, {
                            code: "timeout",
                            message: Words.lost
                        });
                }
            }
            if (!backend.connected) {
                // A fresh link every half second while the engine is away; a pending one gets 400 ms.
                if (n % 2 === 0 && now - backend.socketMade >= 400)
                    backend.reconnect();
            } else if (n % 4 === 0) {
                backend.request("ping", {});
            }
        }
    }
}
