pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Controls.Basic as Controls
import QtQuick.Layouts
import QtQuick.Window
import Quickshell
import Quickshell.Io

ShellRoot {
    id: setup

    property int step: 0
    property string network: ""
    property bool finishing: false
    property var state: ({
        networkState: "checking", wifiState: "idle", wifiName: "", hasWifi: false, networks: [],
        driverState: "checking", packages: [], agentState: "idle", providers: [],
        savedModel: "", userCode: "", signedIn: false, copilotModels: [], finished: false
    })
    readonly property var steps: [
        { icon: "welcome", label: qsTr("Welcome"), title: qsTr("Welcome to Claw OS"),
          description: qsTr("Connect to the internet, finish setting up your hardware, and choose a model for your Agent.") },
        { icon: "network", label: qsTr("Network"), title: qsTr("Connect to the internet"),
          description: qsTr("Choose a Wi-Fi network or connect an Ethernet cable.") },
        { icon: "drivers", label: qsTr("Drivers"), title: qsTr("Set up your hardware"),
          description: qsTr("Claw OS installs only the additional packages that match this computer.") },
        { icon: "model", label: qsTr("Agent"), title: qsTr("Choose your Agent model"),
          description: qsTr("Connect a model provider now, or configure your Agent later.") },
        { icon: "finish", label: qsTr("Finish"), title: qsTr("You're ready"),
          description: qsTr("Finish setup to open your Claw OS desktop.") }
    ]

    function send(action: string, values: var): void {
        backend.write(JSON.stringify(Object.assign({ action: action }, values ?? {})) + "\n");
    }

    function go(next: int): void {
        step = next;
        scroll.contentItem.contentY = 0;
        if (next === 1)
            send("scan");
        if (next === 3)
            send("models");
    }

    function receive(message: var): void {
        state = Object.assign({}, state, message);
        if (message.finishError)
            finishing = false;
        if (state.finished) {
            Qt.quit();
        } else if (step === 2 && state.driverState === "ready") {
            go(3);
        } else if (step === 3 && message.agentState === "saved") {
            go(4);
        }
    }

    Component.onCompleted: Quickshell.watchFiles = false

    Process {
        id: backend
        running: true
        stdinEnabled: true
        command: ["/usr/libexec/claw-oobe", "--backend"]
        stdout: SplitParser {
            onRead: line => setup.receive(JSON.parse(line))
        }
        onExited: () => {
            if (!setup.state.finished)
                Qt.exit(1);
        }
    }

    QtObject {
        id: theme
        readonly property color background: "#101418"
        readonly property color panel: "#171e25"
        readonly property color rail: "#202a34"
        readonly property color card: "#232e3a"
        readonly property color hover: "#2b3a49"
        readonly property color outline: "#384858"
        readonly property color text: "#e5edf6"
        readonly property color muted: "#a9bacc"
        readonly property color accent: "#9ccbfb"
        readonly property color accentInk: "#0e2b45"
        readonly property color selected: "#2a465e"
        readonly property color good: "#a9ddc9"
        readonly property color error: "#ffb4ab"
        readonly property string font: "Inter"
    }

    Controls.ApplicationWindow {
        id: window
        title: qsTr("Set up Claw OS")
        flags: Qt.FramelessWindowHint
        visibility: Window.FullScreen
        width: 1100
        height: 700
        color: theme.background
        onClosing: {
            if (!setup.state.finished)
                Qt.exit(1);
        }

        Rectangle {
            anchors.fill: parent
            gradient: Gradient {
                GradientStop { position: 0; color: "#192632" }
                GradientStop { position: 1; color: theme.background }
            }
        }

        Rectangle {
            id: mainPanel
            anchors.centerIn: parent
            width: Math.min(1100, window.width - (window.width < 720 ? 24 : 64))
            height: Math.min(688, window.height - (window.height < 630 ? 24 : 64))
            radius: 30
            color: theme.panel
            border.color: theme.outline

            RowLayout {
                anchors.fill: parent
                anchors.margins: 12
                spacing: 0

                Rectangle {
                    id: rail
                    visible: window.width >= 800
                    Layout.preferredWidth: 236
                    Layout.fillHeight: true
                    color: theme.rail
                    radius: 23

                    ColumnLayout {
                        anchors.fill: parent
                        anchors.margins: 18
                        spacing: 8

                        RowLayout {
                            Layout.bottomMargin: 30
                            spacing: 12
                            Rectangle {
                                implicitWidth: 40
                                implicitHeight: 40
                                radius: 13
                                color: theme.accent
                                Image {
                                    anchors.centerIn: parent
                                    width: 28
                                    height: 28
                                    source: "logo.png"
                                }
                            }
                            Label {
                                text: "Claw OS"
                                font.pixelSize: 21
                                font.weight: Font.DemiBold
                            }
                        }

                        Label {
                            Layout.leftMargin: 9
                            Layout.bottomMargin: 6
                            text: qsTr("FIRST-RUN SETUP")
                            color: theme.muted
                            font.pixelSize: 11
                            font.letterSpacing: 1.8
                            font.weight: Font.DemiBold
                        }

                        Repeater {
                            model: setup.steps
                            delegate: Rectangle {
                                id: tile
                                required property int index
                                required property var modelData
                                Layout.fillWidth: true
                                implicitHeight: 46
                                radius: 15
                                color: index === setup.step ? theme.selected : "transparent"

                                RowLayout {
                                    anchors.fill: parent
                                    anchors.leftMargin: 12
                                    anchors.rightMargin: 12
                                    spacing: 12
                                    SetupIcon {
                                        name: tile.modelData.icon
                                        color: tile.index === setup.step ? theme.accent : theme.muted
                                        Layout.preferredWidth: 19
                                        Layout.preferredHeight: 19
                                    }
                                    Label {
                                        Layout.fillWidth: true
                                        text: tile.modelData.label
                                        color: tile.index === setup.step ? theme.text : theme.muted
                                        font.weight: tile.index === setup.step ? Font.DemiBold : Font.Normal
                                    }
                                }
                            }
                        }
                        Item { Layout.fillHeight: true }
                    }
                }

                ColumnLayout {
                    id: content
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    Layout.margins: window.width < 640 ? 12 : 26
                    spacing: 18

                    RowLayout {
                        spacing: 6
                        Repeater {
                            model: setup.steps.length
                            delegate: Rectangle {
                                required property int index
                                implicitWidth: index === setup.step ? 32 : 12
                                implicitHeight: 6
                                radius: 3
                                color: index === setup.step ? theme.accent : theme.outline
                            }
                        }
                        Item { Layout.fillWidth: true }
                        Label {
                            text: qsTr("Step %1 of %2").arg(setup.step + 1).arg(setup.steps.length)
                            color: theme.muted
                            font.pixelSize: 12
                        }
                    }

                    Controls.ScrollView {
                        id: scroll
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        contentWidth: availableWidth
                        clip: true
                        Controls.ScrollBar.horizontal.policy: Controls.ScrollBar.AlwaysOff

                        ColumnLayout {
                            width: scroll.availableWidth
                            spacing: 13

                            Rectangle {
                                Layout.topMargin: 3
                                implicitWidth: 48
                                implicitHeight: 48
                                radius: 16
                                color: theme.selected
                                SetupIcon {
                                    anchors.centerIn: parent
                                    name: setup.steps[setup.step].icon
                                    color: theme.accent
                                }
                            }
                            Label {
                                Layout.fillWidth: true
                                text: setup.steps[setup.step].title
                                wrapMode: Text.Wrap
                                font.pixelSize: window.width >= 800 ? 34 : 27
                                font.weight: Font.DemiBold
                            }
                            Label {
                                Layout.fillWidth: true
                                Layout.bottomMargin: 8
                                text: setup.steps[setup.step].description
                                color: theme.muted
                                wrapMode: Text.Wrap
                            }
                            Loader {
                                id: page
                                Layout.fillWidth: true
                                sourceComponent: [welcomePage, networkPage, driverPage, modelPage, finishPage][setup.step]
                            }
                        }
                    }

                    Rectangle {
                        Layout.fillWidth: true
                        implicitHeight: 1
                        color: theme.outline
                    }

                    RowLayout {
                        spacing: 10
                        Item { Layout.fillWidth: true }
                        ActionButton {
                            visible: setup.step >= 1 && setup.step <= 3
                            enabled: setup.step === 1
                                || (setup.step === 2 && setup.state.driverState !== "installing")
                                || (setup.step === 3 && setup.state.agentState !== "saving")
                            text: [ "", qsTr("Continue offline"), qsTr("Continue without drivers"), qsTr("Set up later") ][setup.step] ?? ""
                            onClicked: {
                                if (setup.step === 3) {
                                    setup.send("cancelSignIn");
                                    setup.go(4);
                                } else {
                                    setup.go(3);
                                }
                            }
                        }
                        ActionButton {
                            primary: true
                            visible: setup.step !== 2
                            enabled: setup.step === 0
                                || (setup.step === 1 && setup.state.networkState === "online")
                                || (setup.step === 3 && page.item && page.item.canContinue)
                                || (setup.step === 4 && !setup.finishing)
                            text: setup.step === 4 ? qsTr("Finish") : setup.step === 3 && !setup.state.savedModel
                                ? qsTr("Save and continue") : qsTr("Next")
                            onClicked: {
                                if (setup.step === 0)
                                    setup.go(1);
                                else if (setup.step === 1)
                                    setup.go(setup.state.driverState === "ready" ? 3 : 2);
                                else if (setup.step === 3)
                                    page.item.advance();
                                else if (setup.step === 4) {
                                    setup.finishing = true;
                                    setup.send("finish");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Component {
        id: welcomePage
        ColumnLayout {
            spacing: 7
            Repeater {
                model: [
                    { icon: "network", title: qsTr("Network"), detail: qsTr("Connect by Wi-Fi or Ethernet.") },
                    { icon: "drivers", title: qsTr("Hardware"), detail: qsTr("Install any missing packages for this computer.") },
                    { icon: "model", title: qsTr("Agent"), detail: qsTr("Choose a provider and a model.") }
                ]
                delegate: Rectangle {
                    id: summary
                    required property var modelData
                    Layout.fillWidth: true
                    implicitHeight: summaryContent.implicitHeight + 20
                    radius: 15
                    color: theme.card
                    RowLayout {
                        id: summaryContent
                        anchors.fill: parent
                        anchors.margins: 10
                        spacing: 12
                        SetupIcon { name: summary.modelData.icon; color: theme.accent }
                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 2
                            Label { text: summary.modelData.title; font.weight: Font.DemiBold }
                            Label {
                                Layout.fillWidth: true
                                text: summary.modelData.detail
                                color: theme.muted
                                font.pixelSize: 13
                                wrapMode: Text.Wrap
                            }
                        }
                    }
                }
            }
        }
    }

    Component {
        id: networkPage
        ColumnLayout {
            id: networks
            spacing: 10
            readonly property var selected: setup.state.networks.find(row => row.name === setup.network)
            function connectWifi(): void {
                setup.send("wifi", { network: setup.network, password: password.text });
                password.clear();
            }
            Status {
                Layout.fillWidth: true
                tint: setup.state.networkState === "online" ? theme.good : theme.muted
                text: {
                    if (setup.state.wifiState === "connecting")
                        return qsTr("Connecting to %1…").arg(setup.state.wifiName);
                    if (setup.state.networkState === "online")
                        return qsTr("Connected to the internet");
                    if (setup.state.wifiState === "failed")
                        return qsTr("Couldn't connect. Check the Wi-Fi password and try again.");
                    if (setup.state.wifiState === "passwordRequired")
                        return qsTr("Enter a WPA password or select a saved or open network.");
                    if (setup.state.networkState === "portal")
                        return qsTr("This network requires a sign-in. Try another network or continue offline.");
                    if (setup.state.networkState === "unavailable")
                        return qsTr("NetworkManager is unavailable. You can continue offline.");
                    return setup.state.networkState === "checking" ? qsTr("Checking connection…") : qsTr("Not connected");
                }
            }

            RowLayout {
                Layout.fillWidth: true
                visible: setup.state.hasWifi
                Label { Layout.fillWidth: true; text: qsTr("Wi-Fi networks"); font.weight: Font.DemiBold }
                ActionButton { text: qsTr("Rescan"); onClicked: setup.send("scan") }
            }
            ListView {
                Layout.fillWidth: true
                Layout.preferredHeight: Math.min(setup.state.networks.length, 3) * 44
                clip: true
                model: setup.state.networks
                visible: count > 0
                Controls.ScrollBar.vertical: Controls.ScrollBar {}
                delegate: Controls.ItemDelegate {
                    id: accessPoint
                    required property var modelData
                    width: ListView.view.width
                    height: 43
                    highlighted: modelData.name === setup.network
                    enabled: setup.state.wifiState !== "connecting"
                    Accessible.name: modelData.name
                    background: Rectangle {
                        radius: 12
                        color: accessPoint.highlighted ? theme.selected : (accessPoint.hovered ? theme.hover : theme.card)
                        border.color: accessPoint.visualFocus ? theme.accent : "transparent"
                    }
                    contentItem: RowLayout {
                        SetupIcon { name: "network"; color: theme.accent; Layout.preferredWidth: 17; Layout.preferredHeight: 17 }
                        Label { Layout.fillWidth: true; text: accessPoint.modelData.name; elide: Text.ElideRight }
                        Label { text: accessPoint.modelData.secure ? qsTr("Secured") : qsTr("Open"); color: theme.muted; font.pixelSize: 12 }
                    }
                    onClicked: {
                        if (setup.network !== modelData.name)
                            password.clear();
                        setup.network = modelData.name;
                    }
                }
            }
            RowLayout {
                Layout.fillWidth: true
                visible: !!networks.selected
                enabled: setup.state.wifiState !== "connecting"
                Field {
                    id: password
                    Layout.fillWidth: true
                    visible: !!networks.selected?.secure
                    placeholderText: qsTr("Wi-Fi password")
                    echoMode: TextInput.Password
                    onAccepted: networks.connectWifi()
                }
                ActionButton { primary: true; text: qsTr("Connect"); onClicked: networks.connectWifi() }
            }
        }
    }

    Component {
        id: driverPage
        ColumnLayout {
            spacing: 12
            Status {
                Layout.fillWidth: true
                text: {
                    if (setup.state.driverState === "installing")
                        return qsTr("Downloading and installing the matched packages…");
                    if (setup.state.driverState === "failed")
                        return qsTr("Installation didn't finish. Claw OS will retry on a new connection.");
                    return qsTr("Waiting for the driver installer…");
                }
            }
            Label {
                Layout.fillWidth: true
                text: setup.state.packages.join(", ")
                color: theme.muted
                wrapMode: Text.Wrap
            }
        }
    }

    Component {
        id: modelPage
        ColumnLayout {
            id: model
            spacing: 10
            property var provider: null
            readonly property bool oauth: provider?.auth_kind === "oauth_device"
            readonly property bool busy: ["loading", "saving", "signingIn", "authorizing", "loadingModels"].includes(setup.state.agentState)
            readonly property bool needsKey: !!provider?.needs_credential && !oauth
            readonly property bool needsEndpoint: provider?.extra_fields?.some(field => field.key === "base_url" && field.required) ?? false
            readonly property bool canContinue: !busy && (!!setup.state.savedModel || !!(
                provider && modelName.text.trim() && (!needsEndpoint || endpoint.text.trim())
                && (!needsKey || secret.text.trim()) && (!oauth || setup.state.signedIn)))
            readonly property var suggestions: oauth ? setup.state.copilotModels : (provider?.models ?? []).map(item => item.name)

            function advance(): void {
                if (setup.state.savedModel) {
                    setup.go(4);
                    return;
                }
                setup.send("saveModel", {
                    provider: provider.name, model: modelName.text, apiKey: secret.text,
                    endpoint: endpoint.visible ? endpoint.text : "", apiVersion: apiVersion.visible ? apiVersion.text : ""
                });
                secret.clear();
            }

            Status {
                Layout.fillWidth: true
                visible: setup.state.savedModel !== "" || model.busy || ["saveFailed", "signInFailed", "unavailable"].includes(setup.state.agentState)
                tint: setup.state.savedModel ? theme.good
                    : ["saveFailed", "signInFailed", "unavailable"].includes(setup.state.agentState) ? theme.error : theme.muted
                text: {
                    if (setup.state.savedModel)
                        return qsTr("Model configured: %1").arg(setup.state.savedModel);
                    const messages = {
                        loading: qsTr("Loading providers…"), saving: qsTr("Saving model configuration…"),
                        signingIn: qsTr("Requesting a sign-in code…"), authorizing: qsTr("Waiting for GitHub sign-in…"),
                        loadingModels: qsTr("Loading Copilot models…"),
                        unavailable: qsTr("Couldn't load the provider list. You can configure your Agent later."),
                        saveFailed: qsTr("Couldn't save the model. Check the fields and try again."),
                        signInFailed: qsTr("GitHub sign-in didn't finish. Try again.")
                    };
                    return messages[setup.state.agentState] ?? "";
                }
            }
            Picker {
                Layout.fillWidth: true
                visible: !setup.state.savedModel && setup.state.providers.length > 0
                enabled: !model.busy
                label: qsTr("Choose a provider")
                selection: model.provider ? (model.oauth ? "GitHub Copilot" : model.provider.label) : ""
                model: setup.state.providers.map(provider => provider.name === "copilot" ? "GitHub Copilot" : provider.label)
                onActivated: index => {
                    model.provider = setup.state.providers[index];
                    secret.clear();
                    endpoint.clear();
                    apiVersion.clear();
                    modelName.text = model.oauth ? "" : (model.provider.default_model ?? "");
                }
            }
            RowLayout {
                Layout.fillWidth: true
                visible: model.oauth && !setup.state.signedIn
                ActionButton {
                    text: qsTr("Sign in with GitHub")
                    enabled: !model.busy
                    onClicked: setup.send("signIn")
                }
                ActionButton {
                    visible: setup.state.userCode !== ""
                    text: qsTr("Cancel sign-in")
                    onClicked: setup.send("cancelSignIn")
                }
            }
            Label {
                Layout.fillWidth: true
                visible: model.oauth && setup.state.userCode !== ""
                text: qsTr("Enter code %1 at github.com/login/device.").arg(setup.state.userCode)
                color: theme.accent
                wrapMode: Text.Wrap
            }
            ActionButton {
                visible: model.oauth && setup.state.userCode !== ""
                text: qsTr("Open GitHub")
                onClicked: Qt.openUrlExternally("https://github.com/login/device")
            }
            RowLayout {
                Layout.fillWidth: true
                visible: !!model.provider && !setup.state.savedModel && (!model.oauth || setup.state.signedIn)
                enabled: !model.busy
                Field {
                    id: modelName
                    Layout.fillWidth: true
                    placeholderText: qsTr("Model name")
                }
                Picker {
                    visible: model.suggestions.length > 0
                    Layout.preferredWidth: 152
                    label: qsTr("Browse models")
                    model: model.suggestions
                    onActivated: index => modelName.text = model.suggestions[index]
                }
            }
            Field {
                id: secret
                Layout.fillWidth: true
                visible: model.needsKey && !setup.state.savedModel
                enabled: !model.busy
                placeholderText: qsTr("API key")
                echoMode: TextInput.Password
            }
            Field {
                id: endpoint
                Layout.fillWidth: true
                visible: !setup.state.savedModel && ["azure", "ollama", "openai_compat", "openai", "xai", "deepseek", "openrouter"].includes(model.provider?.name)
                enabled: !model.busy
                placeholderText: model.needsEndpoint ? qsTr("Provider endpoint (required)") : qsTr("Custom endpoint (optional)")
            }
            Field {
                id: apiVersion
                Layout.fillWidth: true
                visible: !setup.state.savedModel && model.provider?.name === "azure"
                enabled: !model.busy
                placeholderText: qsTr("Azure API version (optional)")
            }
        }
    }

    Component {
        id: finishPage
        ColumnLayout {
            Status {
                Layout.fillWidth: true
                tint: setup.state.finishError ? theme.error : theme.muted
                text: setup.state.finishError
                    ? qsTr("Couldn't save setup completion. Check that your home folder is writable.")
                    : setup.state.savedModel ? qsTr("Your Agent model is set to %1.").arg(setup.state.savedModel)
                    : qsTr("You can still configure your Agent from Settings.")
            }
        }
    }

    component Label: Text {
        color: theme.text
        font.family: theme.font
        font.pixelSize: 15
        textFormat: Text.PlainText
    }

    component Status: Rectangle {
        id: box
        property alias text: caption.text
        property color tint: theme.muted
        color: theme.card
        radius: 14
        implicitHeight: caption.implicitHeight + 24
        Label {
            id: caption
            anchors.fill: parent
            anchors.margins: 12
            color: box.tint
            wrapMode: Text.Wrap
        }
    }

    component ActionButton: Controls.Button {
        id: button
        property bool primary: false
        implicitHeight: 42
        leftPadding: 16
        rightPadding: 16
        hoverEnabled: true
        font.family: theme.font
        font.pixelSize: 14
        font.weight: Font.DemiBold
        background: Rectangle {
            radius: 21
            color: button.primary && button.enabled
                ? (button.down ? Qt.darker(theme.accent, 1.14) : button.hovered ? Qt.lighter(theme.accent, 1.09) : theme.accent)
                : button.hovered && button.enabled ? theme.hover : theme.card
            border.color: button.visualFocus ? theme.accent : "transparent"
            border.width: 2
        }
        contentItem: Label {
            text: button.text
            font: button.font
            color: !button.enabled ? theme.muted : button.primary ? theme.accentInk : theme.text
            horizontalAlignment: Text.AlignHCenter
            verticalAlignment: Text.AlignVCenter
        }
    }

    component Field: Controls.TextField {
        id: input
        implicitHeight: 42
        leftPadding: 12
        rightPadding: 12
        color: theme.text
        placeholderTextColor: theme.muted
        selectionColor: theme.selected
        selectedTextColor: theme.text
        font.family: theme.font
        font.pixelSize: 14
        inputMethodHints: echoMode === TextInput.Password ? Qt.ImhSensitiveData | Qt.ImhNoPredictiveText : Qt.ImhNone
        Accessible.name: placeholderText
        background: Rectangle {
            radius: 12
            color: theme.card
            border.color: input.activeFocus ? theme.accent : theme.outline
        }
    }

    component Picker: Controls.ComboBox {
        id: picker
        property string label
        property string selection
        implicitHeight: 42
        currentIndex: -1
        hoverEnabled: true
        leftPadding: 12
        rightPadding: 34
        displayText: selection || label
        Accessible.name: label
        background: Rectangle {
            radius: 12
            color: picker.hovered ? theme.hover : theme.card
            border.color: picker.visualFocus ? theme.accent : theme.outline
        }
        indicator: SetupIcon {
            x: picker.width - 28
            y: (picker.height - height) / 2
            width: 19
            height: 19
            name: "down"
            color: theme.accent
        }
        contentItem: Label {
            text: picker.displayText
            font.pixelSize: 14
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
        }
        delegate: Controls.ItemDelegate {
            id: choice
            required property string modelData
            required property int index
            width: picker.width - 10
            highlighted: picker.highlightedIndex === index
            Accessible.name: modelData
            contentItem: Label { text: choice.modelData; elide: Text.ElideRight }
            background: Rectangle {
                radius: 9
                color: choice.highlighted || choice.hovered ? theme.selected : "transparent"
            }
        }
        popup: Controls.Popup {
            y: picker.height + 4
            width: picker.width
            padding: 5
            implicitHeight: Math.min(224, contentItem.implicitHeight + 10)
            background: Rectangle { radius: 12; color: theme.rail; border.color: theme.outline }
            contentItem: ListView {
                clip: true
                implicitHeight: contentHeight
                model: picker.popup.visible ? picker.delegateModel : null
                currentIndex: picker.highlightedIndex
                Controls.ScrollBar.vertical: Controls.ScrollBar {}
            }
        }
    }
}
