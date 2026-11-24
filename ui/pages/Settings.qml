pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Controls as Q
import "../parts"
import ".."

// One column of labelled rows, exactly the operator's list (RW-01-R09): sound, theme, the orchestrator
// column's side and the profiles of the campaign in force. Each takes effect at once.
Item {
    id: settings

    // The campaign in force: the one the orchestrator column shows, else the first in view.
    readonly property string campaign: Backend.seatCampaign || (Backend.view.length > 0 ? Backend.view[0] : "")
    readonly property string campaignTitle: (Backend.campaign(campaign) || {}).title || campaign
    property var profiles: null
    // A refusal from the daemon is shown here in plain words; its own message can carry a file path.
    property string trouble: ""
    readonly property string unreadable: "Its profiles cannot be read"
    readonly property string unsaved: "That profile was not saved"

    function load() {
        profiles = null;
        trouble = "";
        if (!campaign)
            return;
        Backend.request("profile.get", {
            campaign: campaign
        }, (ok, result) => {
            if (ok)
                settings.profiles = result;
            else
                settings.trouble = settings.unreadable;
        });
    }
    function choose(role, kind, model, effort) {
        if (!kind || !model || !effort)
            return;
        Backend.request("profile.set", {
            campaign: campaign,
            role: role,
            kind: kind,
            model: model,
            effort: effort
        }, (ok, result) => {
            if (ok) {
                settings.profiles = result;
                settings.trouble = "";
            } else
                settings.trouble = settings.unsaved;
        });
    }

    onCampaignChanged: load()
    Component.onCompleted: load()
    Connections {
        target: Backend
        function onConnectedChanged() {
            if (Backend.connected)
                settings.load();
        }
    }

    Column {
        anchors { left: parent.left; top: parent.top; margins: 24 }
        spacing: 14

        SettingRow {
            label: Words.sound
            Button {
                word: Backend.muted ? Words.muted : Words.soundOn
                selected: Backend.muted
                onClicked: Backend.muted = !Backend.muted
            }
            Q.Slider {
                id: volume
                width: 180
                height: 22
                from: 0
                to: 100
                stepSize: 5
                value: Backend.volume
                enabled: !Backend.muted
                onMoved: Backend.volume = Math.round(value)
            }
            Text {
                anchors.verticalCenter: parent.verticalCenter
                text: Words.volume + " " + Backend.volume
                color: Backend.muted ? Theme.quiet : Theme.foreground
                font { family: Theme.font; pixelSize: Theme.small }
            }
        }
        SettingRow {
            label: Words.theme
            Button { word: Words.followDesktop; selected: Backend.themeFollow; onClicked: Backend.themeFollow = true }
            Button { word: Words.houseColours; selected: !Backend.themeFollow; onClicked: Backend.themeFollow = false }
        }
        SettingRow {
            label: Words.orchestratorSide
            Repeater {
                model: Words.sides
                delegate: Button {
                    required property string modelData
                    word: modelData
                    selected: Backend.orchestratorSide === modelData
                    onClicked: Backend.orchestratorSide = modelData
                }
            }
        }
        SettingRow {
            label: Words.profiles
            Text {
                anchors.verticalCenter: parent.verticalCenter
                text: !settings.campaign ? Words.noCampaignInForce : settings.trouble ? settings.campaignTitle + " · " + settings.trouble : settings.campaignTitle
                color: settings.campaign ? Theme.foreground : Theme.quiet
                font { family: Theme.font; pixelSize: Theme.small }
            }
        }
        Repeater {
            model: settings.campaign && settings.profiles ? [["orchestrator", Words.orchestrator], ["worker", Words.workers]] : []
            delegate: Column {
                id: role
                required property var modelData
                readonly property var current: settings.profiles ? (settings.profiles[modelData[0]] || {}) : {}
                spacing: 6
                SettingRow {
                    label: "   " + role.modelData[1]
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        text: Words.profile(role.current)
                        color: Theme.quiet
                        font { family: Theme.font; pixelSize: Theme.small }
                    }
                }
                SettingRow {
                    label: "      " + Words.harness
                    Repeater {
                        model: settings.profiles ? settings.profiles.kinds : []
                        delegate: Button {
                            required property string modelData
                            word: Words.kindNames[modelData] || modelData
                            selected: role.current.kind === modelData
                            onClicked: settings.choose(role.modelData[0], modelData, role.current.model || model.text, role.current.effort || "medium")
                        }
                    }
                }
                SettingRow {
                    label: "      " + Words.model
                    Rectangle {
                        width: 240
                        height: 22
                        radius: Theme.radius
                        color: "transparent"
                        border { width: 1; color: Theme.border }
                        TextInput {
                            id: model
                            anchors { fill: parent; leftMargin: 6; rightMargin: 6 }
                            verticalAlignment: TextInput.AlignVCenter
                            text: role.current.model || ""
                            color: Theme.foreground
                            clip: true
                            font { family: Theme.font; pixelSize: Theme.small }
                            onEditingFinished: if (text !== (role.current.model || ""))
                                settings.choose(role.modelData[0], role.current.kind || "", text, role.current.effort || "medium")
                        }
                    }
                }
                SettingRow {
                    label: "      " + Words.effort
                    Repeater {
                        model: settings.profiles ? settings.profiles.efforts : []
                        delegate: Button {
                            required property string modelData
                            word: Words.effortNames[modelData] || modelData
                            selected: role.current.effort === modelData
                            onClicked: settings.choose(role.modelData[0], role.current.kind || "", role.current.model || model.text, modelData)
                        }
                    }
                }
            }
        }
    }
}
