pragma ComponentBehavior: Bound
import QtQuick
import ".."

// The replay's scope from the window's shared selection: the whole campaign, or the one task picked
// on the Graph page (RW-04-R03). Each line is a task's recorded acts, as the daemon's replay summary serves them.
Column {
    id: scope
    property var summary: []
    readonly property var shown: Backend.selectedTask === "" ? summary : summary.filter(s => s.task === Backend.selectedTask)
    readonly property var cstate: Backend.states[Backend.selectedCampaign] || null
    spacing: 4

    function title(task) {
        const ts = cstate ? cstate.tasks : [];
        for (let i = 0; i < ts.length; i++)
            if (ts[i].task === task)
                return ts[i].title || task;
        return task;
    }
    function load() {
        if (Backend.selectedCampaign === "") {
            summary = [];
            return;
        }
        Backend.request("replay.summary", {
            campaign: Backend.selectedCampaign
        }, (ok, r) => scope.summary = ok ? r : []);
    }
    Component.onCompleted: load()
    Connections {
        target: Backend
        function onSelectedCampaignChanged() {
            scope.load();
        }
    }

    Text {
        visible: Backend.selectedCampaign !== ""
        text: {
            const c = Backend.campaign(Backend.selectedCampaign);
            const name = c ? c.title || c.name : Backend.selectedCampaign;
            return Backend.selectedTask === "" ? name : name + " · " + scope.title(Backend.selectedTask);
        }
        color: Theme.foreground
        font { family: Theme.font; pixelSize: Theme.large; bold: true }
    }
    Repeater {
        model: scope.shown
        delegate: Text {
            required property var modelData
            text: scope.title(modelData.task) + " · " + Object.keys(modelData.counts).map(k => k + " " + modelData.counts[k]).join(", ")
            color: Theme.quiet
            font { family: Theme.font; pixelSize: Theme.normal }
        }
    }
}
