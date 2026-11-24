//@ pragma UseQApplication
import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Hyprland
import "parts"
import "pages"

ShellRoot {
    FloatingWindow {
        id: window
        title: "QREAM"
        color: Theme.background

        // Placement: the window opens tiled, like any other window (his word of 12 Oct 2026); the size below
        // only applies if he floats it himself.
        readonly property var monitor: Hyprland.focusedMonitor
        readonly property size fit: {
            const m = monitor;
            if (!m || m.width <= 0)
                return Qt.size(1600, 960);
            const r = m.lastIpcObject?.reserved ?? [0, 0, 0, 0];
            const w = Math.floor(m.width / m.scale) - r[0] - r[2];
            const h = Math.floor(m.height / m.scale) - r[1] - r[3];
            return Qt.size(Math.min(1600, w - 48), Math.min(960, h - 48));
        }
        implicitWidth: fit.width
        implicitHeight: fit.height
        minimumSize: Qt.size(640, 400)
        visible: monitor !== null
        // Closing the window ends the window process; nothing stays behind hidden (his word of 12 Oct 2026).
        onClosed: Qt.quit()

        property string page: "Execution"
        property string before: "Execution"
        readonly property Review reviewPage: review.item as Review

        function show(name) {
            if (name === "Settings" && page !== "Settings")
                before = page;
            page = (name === "Settings" && page === "Settings") ? before : name;
        }

        Shortcut { sequence: "Ctrl+1"; onActivated: window.show("Execution") }
        Shortcut { sequence: "Ctrl+2"; onActivated: window.show("Graph") }
        Shortcut { sequence: "Ctrl+3"; onActivated: window.show("Setup") }
        Shortcut { sequence: "Ctrl+4"; onActivated: window.show("Review") }
        Shortcut { sequence: "Ctrl+,"; onActivated: window.show("Settings") }
        Shortcut { sequence: "Ctrl+B"; onActivated: Backend.leftFolded = !Backend.leftFolded }
        Shortcut { sequence: "Ctrl+Shift+O"; onActivated: Backend.seatOpen = !Backend.seatOpen }
        Shortcut { sequences: ["Ctrl+=", "Ctrl++"]; onActivated: Backend.stepSize(1) }
        Shortcut { sequence: "Ctrl+-"; onActivated: Backend.stepSize(-1) }
        Shortcut { sequence: "Ctrl+0"; onActivated: Backend.stepSize(0) }
        onHeightChanged: Backend.windowHeight = height

        // Automation for checks and captures: `qs ipc call qream <function> <arg>`.
        IpcHandler {
            target: "qream"
            function column(state: string): string {
                Backend.seatOpen = state === "toggle" ? !Backend.seatOpen : state === "open";
                return JSON.stringify({ open: Backend.seatOpen, width: seat.width, area: area.width, side: Backend.sideWidth });
            }
            function seatFor(name: string): void { Backend.seatCampaign = name }
            function seatOpen(): void { seat.openSeat() }
            function seatChoose(kind: string, model: string, effort: string, resume: string): void { seat.choose(kind, model, effort, resume) }
            function seatInvoke(): void { seat.invoke() }
            function seatControl(action: string): void { seat.control(action) }
            function seatView(word: string): void { seat.viewWord = word }
            function seatType(text: string): void { seat.send(text) }
            function seatScroll(pixels: int): string { seat.scrollBy(pixels); return seat.position() }
            function seatPosition(): string { return seat.position() }
            function side(word: string): void { Backend.orchestratorSide = word }
            function seatWidth(width: int): string {
                Backend.seatWidth = width;
                return JSON.stringify({ side: Backend.sideWidth, column: seat.width });
            }
            function sideWidth(width: int): string {
                Backend.sideWidth = width;
                return JSON.stringify({ side: Backend.sideWidth, column: seat.width });
            }
            function refreshAll(): void { Backend.refreshAll() }
            function add(name: string): string { return Backend.addToView(name) ? "added" : Backend.notice }
            function remove(name: string): void { Backend.removeFromView(name) }
            function show(name: string): void { window.show(name) }
            function page(name: string): void { window.show(name) }
            function open(name: string): void { window.show(name) }
            function capture(file: string): void { frame.grabToImage(r => r.saveToFile(file)) }
            function size(step: int): void { Backend.stepSize(step) }
            function lock(on: bool): void { Backend.sizeLock = on }
            function view(name: string, word: string): void { Backend.setView(name, word) }
            function pick(name: string, task: string): void { Backend.pick(name, task) }
            function scroll(n: int): void { Backend.scrollShown(n) }
            function focus(pane: string): void { Backend.focusedPane = pane }
            function sizes(): string { return JSON.stringify({ defaultSize: Backend.defaultSize, family: Backend.termFamily, textSize: Backend.textSize, lock: Backend.sizeLock, paneSizes: Backend.paneSizes, shown: Backend.frameReport().map(v => ({ pane: v.pane, size: Backend.paneSize(v.pane), cols: v.cols, rows: v.rows })) }) }
            function frames(): string { return JSON.stringify(Backend.frameReport()) }
            function mute(on: bool): void { Backend.muted = on }
            function volume(n: int): void { Backend.volume = n }
            function theme(follow: bool): void { Backend.themeFollow = follow }
            function settings(): string { return JSON.stringify({ muted: Backend.muted, volume: Backend.volume, themeFollow: Backend.themeFollow, side: Backend.orchestratorSide, themeColoursRead: Object.keys(Theme.colours).length, notice: Backend.notice, notices: Backend.notices.map(n => n.line) }) }
            function profile(role: string, kind: string, model: string, effort: string): void { Backend.request("profile.set", { campaign: Backend.seatCampaign || Backend.view[0] || "", role: role, kind: kind, model: model, effort: effort }) }
            // Review: open a campaign, play, pause, speed, seek and bounds as fractions, pick a task.
            function replay(name: string): void { window.show("Review"); window.reviewPage?.open(name) }
            function play(): void { if (window.reviewPage && !window.reviewPage.playing) window.reviewPage.toggle() }
            function pause(): void { if (window.reviewPage) window.reviewPage.playing = false }
            function speed(x: real): void { if (window.reviewPage) window.reviewPage.speed = x }
            function seek(f: real): void { window.reviewPage?.seekFraction(f) }
            function bounds(f: real, t: real): void { window.reviewPage?.boundFractions(f, t) }
            function replayPick(task: string): void { window.reviewPage?.pick(task) }
            function archive(): void { if (window.reviewPage) window.reviewPage.archive = !window.reviewPage.archive }
            function replayState(): string { return window.reviewPage ? window.reviewPage.report() : "" }
            // Graph: drive the force graph and read the shared selection.
            function graph(act: string, arg: string): string { return Backend.graph ? Backend.graph.automate(act, arg) : "no graph" }
            function selection(): string { return Backend.selectedCampaign + " " + Backend.selectedTask }
        }

        Rectangle {
            id: frame
            anchors.fill: parent
            color: Theme.background

            Header {
                id: header
                anchors { left: parent.left; right: parent.right; top: parent.top }
                page: window.page
                onChosen: name => window.show(name)
            }

            LeftPanel {
                id: left
                anchors { left: parent.left; top: header.bottom; bottom: parent.bottom }
                width: Backend.leftFolded ? 0 : 260
                visible: !Backend.leftFolded
            }

            // The orchestrator column, docked on the side Settings names; the pages re-flow beside it.
            OrchestratorColumn {
                id: seat
                readonly property bool onLeftSide: Backend.orchestratorSide === "Left"
                anchors { top: header.bottom; bottom: parent.bottom }
                x: onLeftSide ? left.width : frame.width - width
                width: Backend.seatOpen ? Math.min(Backend.seatWidth, Math.max(300, frame.width - left.width - 320)) : 0
                visible: Backend.seatOpen
                z: 1
            }

            Item {
                id: area
                anchors { top: header.bottom; bottom: parent.bottom }
                x: seat.onLeftSide ? left.width + seat.width : left.width
                width: Math.max(0, frame.width - left.width - seat.width)

                // Execution stays loaded while hidden; the other pages load while shown.
                Execution { anchors.fill: parent; visible: window.page === "Execution" }
                Loader { anchors.fill: parent; active: window.page === "Graph"; sourceComponent: Graph {} }
                Loader { anchors.fill: parent; active: window.page === "Setup"; sourceComponent: Setup {} }
                Loader { id: review; anchors.fill: parent; active: window.page === "Review"; focus: active; sourceComponent: Review {} }
                Loader { anchors.fill: parent; active: window.page === "Settings"; sourceComponent: Settings {} }
            }
        }
    }
}
