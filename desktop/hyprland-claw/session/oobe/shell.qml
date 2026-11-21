pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Controls.Basic as Controls
import QtQuick.Layouts
import QtQuick.Window
import Quickshell
import Quickshell.Io
import Quickshell.Services.Greetd

ShellRoot {
    id: setup

    property string current: "network"
    property string network: ""
    property bool skipNetwork: false
    property bool finishing: false
    property bool signInFailed: false
    readonly property bool local: Quickshell.env("CLAW_LOCAL_DESKTOP_SETUP") === "1"
    property string username: ""
    property string newPassword: ""
    property string confirmation: ""
    property string avatar: ""
    property string language: ""
    property string keyboard: "us"
    property string zone: ""
    property bool light: false
    readonly property bool matchingPassword: newPassword !== "" && newPassword === confirmation
    readonly property bool validAccount: /^[a-z][a-z0-9_-]{0,30}$/.test(username) && matchingPassword
    property var state: ({
        networkState: "checking", wifiState: "idle", wifiName: "", hasWifi: false, networks: [],
        driverState: "checking", packages: [], agentState: "idle", providers: [],
        savedModel: "", userCode: "", signedIn: false, copilotModels: [], finished: false,
        languages: [], zones: [], finishError: ""
    })
    readonly property var pages: ({
        network: [qsTr("Get connected"), state.hasWifi ? qsTr("Connect to Wi-Fi or plug in an Ethernet cable.")
            : qsTr("Plug in an Ethernet cable or continue offline."), networkPage],
        language: [qsTr("Select a language"), qsTr("Choose the language your system will use."), languagePage],
        keyboard: [qsTr("Select keyboard layout"), qsTr("This layout takes effect in your desktop session. Setup and sign-in use the current system keyboard."), keyboardPage],
        agent: [qsTr("Configure your Agent"), qsTr("Connect a model provider now, or set it up later."), modelPage],
        account: [qsTr("Create your account"), qsTr("You'll use this account and password to sign in to Claw OS."), accountPage],
        location: [qsTr("Select a time zone"), qsTr("Choose the time zone where you use this computer."), locationPage],
        appearance: [qsTr("Personalize appearance"), qsTr("Preview a light or dark look and set the preference for your apps."), appearancePage],
        drivers: [qsTr("Set up graphics and hardware"), qsTr("Claw OS installs only the additional packages that match this computer."), driverPage],
        finish: [qsTr("You're ready"), qsTr("Finish setup to open your Claw OS desktop."), finishPage]
    })
    readonly property var steps: {
        const order = local
            ? ["language", "keyboard", "agent", "account", "location", "appearance"]
            : ["agent"];
        if (!skipNetwork)
            order.unshift("network");
        if (state.packages.length > 0)
            order.push("drivers");
        order.push("finish");
        return order;
    }
    readonly property int step: steps.indexOf(current)
    readonly property var pageInfo: pages[current]

    function send(action: string, values: var): void {
        backend.write(JSON.stringify(Object.assign({ action: action }, values ?? {})) + "\n");
    }

    function go(next: int): void {
        current = steps[Math.max(0, Math.min(steps.length - 1, next))];
        scroll.contentItem.contentY = 0;
        if (current === "network")
            send("scan");
        if (current === "agent")
            send("models");
    }

    function receive(message: var): void {
        state = Object.assign({}, state, message);
        if (message.finishError) {
            finishing = false;
            if (["invalidUsername", "invalidPassword", "invalidAvatar", "usernameTaken"].includes(message.finishError))
                go(steps.indexOf("account"));
        }
        if (message.finished) {
            confirmation = "";
            if (!local)
                Qt.quit();
            else if (Greetd.available)
                Greetd.createSession(username);
            else
                cannotSignIn();
        } else if (!skipNetwork && ["online", "wired"].includes(message.networkState)) {
            skipNetwork = true;
            if (current === "network")
                go(0);
        } else if (current === "agent" && message.agentState === "saved") {
            go(step + 1);
        }
    }

    function cannotSignIn(): void {
        newPassword = "";
        signInFailed = true;
    }

    function finish(): void {
        finishing = true;
        state = Object.assign({}, state, {finishError: ""});
        if (local) {
            send("finish", {
                username: username, password: newPassword, avatar: avatar, locale: language,
                keyboard: keyboard, zone: zone, light: light
            });
        } else {
            send("finish");
        }
    }

    Component.onCompleted: Quickshell.watchFiles = false

    Connections {
        target: setup.local ? Greetd : null
        enabled: setup.state.finished && !setup.signInFailed

        function onAuthMessage(message, error, responseRequired, echoResponse) {
            if (!responseRequired)
                return;
            if (echoResponse || setup.newPassword === "") {
                setup.cannotSignIn();
                Greetd.cancelSession();
                return;
            }
            const password = setup.newPassword;
            setup.newPassword = "";
            Greetd.respond(password);
        }
        function onAuthFailure(message) { setup.cannotSignIn(); }
        function onError(message) { setup.cannotSignIn(); }
        function onReadyToLaunch() {
            setup.newPassword = "";
            Greetd.launch(["/usr/bin/systemd-cat", "--identifier=claw-desktop",
                           "/usr/local/bin/claw-display-session", "--desktop"], ["XDG_SESSION_TYPE=wayland"]);
        }
    }

    Process {
        id: backend
        running: true
        stdinEnabled: true
        command: ["/usr/bin/env", "-u", "GREETD_SOCK", "/usr/libexec/claw-oobe", "--backend"]
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
        readonly property color panel: setup.light ? "#dcf2f6fe" : "#c9182132"
        readonly property color rail: setup.light ? "#edf3fe" : "#232d42"
        readonly property color card: setup.light ? "#a4ffffff" : "#a32e3b52"
        readonly property color hover: setup.light ? "#f8fbff" : "#3c4d69"
        readonly property color outline: setup.light ? "#9baec9" : "#58718f"
        readonly property color text: setup.light ? "#172842" : "#f1f5ff"
        readonly property color muted: setup.light ? "#3d526e" : "#ccd8ec"
        readonly property color accent: setup.light ? "#315eac" : "#b3d1ff"
        readonly property color accentInk: setup.light ? "#ffffff" : "#14253f"
        readonly property color selected: setup.light ? "#cfdef8" : "#486387"
        readonly property color good: setup.light ? "#235d47" : "#aee6c9"
        readonly property color error: setup.light ? "#942e2a" : "#ffc5bb"
        readonly property string font: "Inter"
    }

    Controls.ApplicationWindow {
        id: window
        title: qsTr("Set up Claw OS")
        flags: Qt.FramelessWindowHint
        visibility: Window.FullScreen
        width: 1100
        height: 700
        color: "#161d2c"
        onClosing: event => event.accepted = setup.state.finished
        GlassBackdrop {
            id: background
            anchors.fill: parent
        }

        GlassCard {
            id: mainPanel
            anchors.centerIn: parent
            width: Math.min(698, window.width - 30)
            height: Math.min(590, window.height - 30)
            wallpaper: background.wallpaper
            tint: theme.panel

            ColumnLayout {
                anchors.fill: parent
                anchors.margins: window.width < 640 ? 20 : 32
                spacing: 18

                RowLayout {
                    spacing: 11
                    BrandMark {}
                    ColumnLayout {
                        spacing: 1
                        Label { text: "Claw OS"; font.pixelSize: 19; font.weight: Font.DemiBold }
                        Label { text: qsTr("First-run setup"); color: theme.muted; font.pixelSize: 12 }
                    }
                    Item { Layout.fillWidth: true }
                    Label {
                        text: qsTr("%1 of %2").arg(setup.step + 1).arg(setup.steps.length)
                        color: theme.muted
                        font.pixelSize: 12
                    }
                }
                Rectangle {
                    Layout.fillWidth: true
                    implicitHeight: 3
                    radius: 2
                    color: theme.outline
                    Rectangle {
                        width: parent.width * (setup.step + 1) / setup.steps.length
                        height: parent.height
                        radius: 2
                        color: theme.accent
                        Behavior on width { NumberAnimation { duration: 180; easing.type: Easing.OutCubic } }
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

                        Label {
                            Layout.fillWidth: true
                            text: setup.pageInfo[0]
                            wrapMode: Text.WordWrap
                            font.pixelSize: window.width >= 680 ? 28 : 23
                            font.weight: Font.DemiBold
                        }
                        Label {
                            Layout.fillWidth: true
                            Layout.bottomMargin: 5
                            text: setup.pageInfo[1]
                            color: theme.muted
                            wrapMode: Text.Wrap
                        }
                        Loader {
                            id: page
                            Layout.fillWidth: true
                            sourceComponent: setup.pageInfo[2]
                        }
                    }
                }

                Rectangle {
                    Layout.fillWidth: true
                    implicitHeight: 1
                    color: theme.outline
                }
                RowLayout {
                    spacing: 8
                    ActionButton {
                        text: qsTr("Back")
                        visible: setup.step > 0
                        enabled: !setup.finishing && setup.state.agentState !== "saving"
                        onClicked: {
                            if (setup.current === "agent")
                                setup.send("cancelSignIn");
                            setup.go(setup.step - 1);
                        }
                    }
                    Item { Layout.fillWidth: true }
                    ActionButton {
                        visible: setup.current === "network" || setup.current === "agent"
                        enabled: setup.state.agentState !== "saving"
                        text: setup.current === "network" ? qsTr("Continue offline") : qsTr("Set up later")
                        onClicked: {
                            if (setup.current === "agent")
                                setup.send("cancelSignIn");
                            setup.go(setup.step + 1);
                        }
                    }
                    ActionButton {
                        primary: true
                        visible: setup.current !== "network"
                        enabled: setup.signInFailed || (!setup.finishing
                            && (setup.current !== "language" || setup.language !== "")
                            && (setup.current !== "account" || setup.validAccount)
                            && (setup.current !== "location" || setup.zone !== "")
                            && (setup.current !== "agent" || !!page.item?.canContinue))
                        text: setup.signInFailed ? qsTr("Go to sign-in")
                            : setup.state.finished && setup.local ? qsTr("Signing in…")
                            : setup.current === "finish" ? (setup.finishing ? qsTr("Saving…") : qsTr("Finish"))
                            : setup.current === "agent" && !setup.state.savedModel ? qsTr("Save and continue") : qsTr("Next")
                        onClicked: {
                            if (setup.signInFailed)
                                Qt.quit();
                            else if (setup.current === "agent")
                                page.item.advance();
                            else if (setup.current === "finish")
                                setup.finish();
                            else
                                setup.go(setup.step + 1);
                        }
                    }
                }
            }
        }
    }

    Component {
        id: languagePage
        Picker {
            function languageName(code: string): string {
                const locale = Qt.locale(code.split(".")[0]);
                return qsTr("%1 (%2)").arg(locale.nativeLanguageName).arg(locale.nativeTerritoryName);
            }
            label: qsTr("Choose a language")
            model: setup.state.languages.map(code => languageName(code))
            selection: setup.language ? languageName(setup.language) : ""
            onActivated: index => setup.language = setup.state.languages[index]
        }
    }

    Component {
        id: keyboardPage
        Picker {
            readonly property var layouts: ["us", "gb", "de", "fr", "es", "it", "br", "jp", "ru"]
            label: qsTr("Keyboard layout")
            model: [qsTr("English (US)"), qsTr("English (UK)"), qsTr("German"), qsTr("French"),
                    qsTr("Spanish"), qsTr("Italian"), qsTr("Portuguese (Brazil)"), qsTr("Japanese"), qsTr("Russian")]
            selection: model[layouts.indexOf(setup.keyboard)]
            onActivated: index => setup.keyboard = layouts[index]
        }
    }

    Component {
        id: accountPage

        GridLayout {
            id: account
            readonly property bool wide: width >= 550
            readonly property var avatars: [
                { id: "fox", name: qsTr("Fox") },
                { id: "penguin", name: qsTr("Penguin") },
                { id: "rabbit", name: qsTr("Rabbit") },
                { id: "frog", name: qsTr("Frog") },
                { id: "robot", name: qsTr("Robot") },
                { id: "star", name: qsTr("Star") }
            ]
            columns: wide ? 2 : 1
            columnSpacing: 22
            rowSpacing: 16

            ColumnLayout {
                Layout.fillWidth: true
                Layout.alignment: Qt.AlignTop
                spacing: 8

                Field {
                    Layout.fillWidth: true
                    placeholderText: qsTr("Username")
                    maximumLength: 31
                    text: setup.username
                    onTextEdited: setup.username = text
                }
                Label {
                    Layout.fillWidth: true
                    Layout.bottomMargin: 4
                    text: qsTr("Start with a lowercase letter. You can also use digits, hyphens or underscores.")
                    color: theme.muted
                    font.pixelSize: 12
                    wrapMode: Text.Wrap
                }
                Field {
                    Layout.fillWidth: true
                    placeholderText: qsTr("Password")
                    maximumLength: 256
                    echoMode: TextInput.Password
                    text: setup.newPassword
                    onTextEdited: setup.newPassword = text
                }
                Field {
                    Layout.fillWidth: true
                    placeholderText: qsTr("Confirm password")
                    maximumLength: 256
                    echoMode: TextInput.Password
                    text: setup.confirmation
                    onTextEdited: setup.confirmation = text
                }
                Label {
                    Layout.fillWidth: true
                    visible: (setup.confirmation !== "" && !setup.matchingPassword) || setup.state.finishError !== ""
                    color: theme.error
                    text: setup.confirmation !== "" && !setup.matchingPassword ? qsTr("Passwords don't match.")
                        : setup.state.finishError === "usernameTaken" ? qsTr("That username is already in use.")
                        : setup.state.finishError === "invalidAvatar" ? qsTr("Choose an available avatar or use the Claw logo.")
                        : qsTr("Check the account and setup fields, then try again.")
                    wrapMode: Text.Wrap
                }
            }

            Rectangle {
                Layout.fillWidth: !account.wide
                Layout.preferredWidth: 276
                Layout.alignment: Qt.AlignTop
                implicitHeight: gallery.implicitHeight + 24
                radius: 16
                color: theme.card
                border.color: theme.outline

                Controls.ButtonGroup { id: avatarGroup }

                ColumnLayout {
                    id: gallery

                    anchors.fill: parent
                    anchors.margins: 12
                    spacing: 8

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 10
                        AccountAvatar {
                            Layout.preferredWidth: 40
                            Layout.preferredHeight: 40
                            avatarId: setup.avatar
                        }
                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 2
                            Label {
                                Layout.fillWidth: true
                                text: qsTr("Choose an avatar")
                                font.pixelSize: 14
                                font.weight: Font.DemiBold
                                wrapMode: Text.Wrap
                            }
                            Label {
                                Layout.fillWidth: true
                                text: account.avatars.find(item => item.id === setup.avatar)?.name ?? qsTr("Claw logo")
                                color: theme.muted
                                font.pixelSize: 12
                                elide: Text.ElideRight
                            }
                        }
                        ActionButton {
                            text: qsTr("Reset")
                            Accessible.name: qsTr("Use the Claw logo")
                            enabled: setup.avatar !== ""
                            implicitHeight: 30
                            leftPadding: 8
                            rightPadding: 8
                            font.pixelSize: 12
                            onClicked: setup.avatar = ""
                        }
                    }

                    GridLayout {
                        Layout.fillWidth: true
                        columns: width < 200 ? 2 : width >= 435 ? 6 : 3
                        columnSpacing: 4
                        rowSpacing: 3

                        Repeater {
                            model: account.avatars

                            delegate: Controls.RadioButton {
                                id: option
                                required property var modelData
                                Layout.fillWidth: true
                                Layout.preferredWidth: 72
                                implicitHeight: 68
                                padding: 3
                                hoverEnabled: true
                                text: modelData.name
                                checked: setup.avatar === modelData.id
                                Controls.ButtonGroup.group: avatarGroup
                                Accessible.name: text
                                onClicked: setup.avatar = modelData.id
                                indicator: null
                                background: Rectangle {
                                    radius: 12
                                    color: option.checked ? theme.selected : option.hovered ? theme.hover : "transparent"
                                    border.color: option.visualFocus ? theme.accent : "transparent"
                                }
                                contentItem: Column {
                                    spacing: 3
                                    Item {
                                        anchors.horizontalCenter: parent.horizontalCenter
                                        width: 44
                                        height: 44
                                        AccountAvatar {
                                            anchors.fill: parent
                                            avatarId: option.modelData.id
                                        }
                                        Rectangle {
                                            anchors.fill: parent
                                            anchors.margins: -2
                                            radius: width / 2
                                            color: "transparent"
                                            border.color: theme.accent
                                            border.width: 2
                                            visible: option.checked
                                        }
                                    }
                                    Label {
                                        width: parent.width
                                        text: option.text
                                        font.pixelSize: 11
                                        horizontalAlignment: Text.AlignHCenter
                                        elide: Text.ElideRight
                                    }
                                }
                            }
                        }
                    }

                    Label {
                        Layout.fillWidth: true
                        text: qsTr("Shown on sign-in and lock screens.")
                        color: theme.muted
                        font.pixelSize: 12
                        wrapMode: Text.Wrap
                    }
                }
            }
        }
    }

    Component {
        id: locationPage
        ColumnLayout {
            id: timeZonePage
            readonly property var matches: setup.state.zones.filter(zone =>
                zone.toLowerCase().replace(/_/g, " ").includes(filterZone.text.trim().toLowerCase()))

            function zoneName(zone: string): string {
                return zone.replace(/_/g, " ");
            }

            Field {
                id: filterZone
                Layout.fillWidth: true
                placeholderText: qsTr("Search by city or time zone")
            }
            Picker {
                Layout.fillWidth: true
                label: qsTr("Choose a time zone")
                model: timeZonePage.matches.map(zone => timeZonePage.zoneName(zone))
                selection: timeZonePage.zoneName(setup.zone)
                onActivated: index => setup.zone = timeZonePage.matches[index]
            }
        }
    }

    Component {
        id: appearancePage
        RowLayout {
            spacing: 12
            ActionButton {
                Layout.fillWidth: true
                primary: !setup.light
                text: qsTr("Dark")
                onClicked: setup.light = false
            }
            ActionButton {
                Layout.fillWidth: true
                primary: setup.light
                text: qsTr("Light")
                onClicked: setup.light = true
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
                    if (setup.state.networkState === "wired")
                        return qsTr("Connected by Ethernet");
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
                    setup.go(setup.step + 1);
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
                tint: setup.signInFailed || setup.state.finishError ? theme.error : theme.muted
                text: {
                    if (setup.signInFailed)
                        return qsTr("Your account is ready, but automatic sign-in didn't complete. Sign in with the password you just set.");
                    if (setup.state.finishError === "busy")
                        return qsTr("Wait for the current setup operation, then try again.");
                    if (setup.state.finishError === "invalidRegionalSettings")
                        return qsTr("Go back and choose a language, keyboard layout, and time zone.");
                    if (setup.state.finishError)
                        return setup.local ? qsTr("Couldn't finish creating the account. Your desktop hasn't been opened. Try again.")
                            : qsTr("Couldn't save setup completion. Check that your home folder is writable.");
                    if (setup.local)
                        return qsTr("Account: %1").arg(setup.username);
                    return setup.state.savedModel ? qsTr("Your Agent model is set to %1.").arg(setup.state.savedModel)
                        : qsTr("You can still configure your Agent from Settings.");
                }
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
        palette.dark: theme.accent
        background: Rectangle {
            radius: 12
            color: picker.hovered ? theme.hover : theme.card
            border.color: picker.visualFocus ? theme.accent : theme.outline
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
