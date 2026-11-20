pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Controls.Basic as Controls
import QtQuick.Layouts
import QtQuick.Window
import Quickshell
import Quickshell.Io
import Quickshell.Services.Greetd

ShellRoot {
    id: root
    property var accounts: []
    property string notice: ""
    property string prompt: ""
    property bool otherAccount: false
    property bool responding: false
    property bool echo: false
    property bool authenticationRejected: false
    property string submitted: ""
    readonly property bool idle: Greetd.state === GreetdState.Inactive
    readonly property string username: root.otherAccount ? other.text.trim()
        : root.accounts[accounts.currentIndex]?.username ?? ""

    function submit(): void {
        if (!signIn.enabled)
            return;
        notice = "";
        authenticationRejected = false;
        const answer = password.text;
        password.clear();
        if (responding) {
            responding = false;
            Greetd.respond(answer);
        } else {
            submitted = answer;
            Greetd.createSession(username);
        }
    }

    function fail(message: string): void {
        submitted = "";
        responding = false;
        prompt = "";
        password.clear();
        notice = message;
        password.forceActiveFocus();
    }

    Component.onCompleted: Quickshell.watchFiles = false

    SystemClock {
        id: clock
        precision: SystemClock.Minutes
    }

    Process {
        running: true
        command: ["/usr/libexec/claw-oobe", "--accounts"]
        stdout: StdioCollector {
            onStreamFinished: {
                root.accounts = JSON.parse(text);
                accounts.currentIndex = root.accounts.length ? 0 : -1;
                root.otherAccount = root.accounts.length === 0;
                (root.otherAccount ? other : password).forceActiveFocus();
            }
        }
    }

    Connections {
        target: Greetd
        function onAuthMessage(message, error, responseRequired, echoResponse) {
            if (!responseRequired) {
                root.notice = message;
            } else if (!echoResponse && root.submitted !== "") {
                const answer = root.submitted;
                root.submitted = "";
                Greetd.respond(answer);
            } else {
                root.submitted = "";
                root.echo = echoResponse;
                root.prompt = message;
                root.responding = true;
                password.forceActiveFocus();
            }
        }
        function onAuthFailure(message) {
            root.authenticationRejected = true;
            root.fail(qsTr("Sign-in was not accepted. Check your password and try again."));
        }
        function onError(message) {
            // greetd may also reject cancellation after closing a failed PAM attempt.
            if (!root.authenticationRejected)
                root.fail(qsTr("The sign-in service couldn't complete the request. Try again."));
        }
        function onReadyToLaunch() {
            root.submitted = "";
            root.notice = qsTr("Opening your desktop…");
            Greetd.launch(["/usr/bin/systemd-cat", "--identifier=claw-desktop",
                           "/usr/local/bin/claw-display-session", "--desktop"], ["XDG_SESSION_TYPE=wayland"]);
        }
    }

    Controls.ApplicationWindow {
        id: window
        title: "Claw OS"
        visibility: Window.FullScreen
        flags: Qt.FramelessWindowHint
        color: "#101923"
        onClosing: event => event.accepted = false

        Image {
            anchors.fill: parent
            source: "file:///usr/share/backgrounds/claw-os/claw-default.jpg"
            fillMode: Image.PreserveAspectCrop
            sourceSize.width: Math.min(3200, Math.max(1280, Math.ceil(width * Screen.devicePixelRatio)))
            asynchronous: true
        }
        Rectangle {
            anchors.fill: parent
            gradient: Gradient {
                GradientStop { position: 0.0; color: "#ab101923" }
                GradientStop { position: 0.3; color: "#63101923" }
                GradientStop { position: 0.59; color: "#16101923" }
                GradientStop { position: 1.0; color: "#65101923" }
            }
        }

        Item {
            id: scene
            readonly property real fit: Math.min(1, Math.max(1, window.width) / 384,
                                                    Math.max(1, window.height) / 440)
            width: window.width / fit
            height: window.height / fit
            scale: fit
            transformOrigin: Item.TopLeft

            Column {
                anchors.horizontalCenter: parent.horizontalCenter
                anchors.top: parent.top
                anchors.topMargin: Math.round(Math.min(112, scene.height * 0.09))
                spacing: -1

                Caption {
                    anchors.horizontalCenter: parent.horizontalCenter
                    height: 24
                    text: Qt.formatDate(clock.date, "dddd, MMMM d")
                    font.pixelSize: 20
                    font.weight: Font.DemiBold
                    verticalAlignment: Text.AlignVCenter
                }
                Caption {
                    anchors.horizontalCenter: parent.horizontalCenter
                    height: font.pixelSize
                    text: Qt.formatTime(clock.date, "hh:mm")
                    font.pixelSize: Math.round(Math.min(108, scene.height * 0.135))
                    font.weight: Font.DemiBold
                    font.letterSpacing: -3
                    verticalAlignment: Text.AlignVCenter
                }
            }

            ColumnLayout {
                id: access
                anchors.horizontalCenter: parent.horizontalCenter
                anchors.bottom: parent.bottom
                anchors.bottomMargin: Math.max(footer.implicitHeight + 16,
                                                Math.min(106, Math.max(64, Math.round(scene.height * 0.1))))
                width: 280
                spacing: 8

                Rectangle {
                    Layout.alignment: Qt.AlignHCenter
                    Layout.preferredWidth: 62
                    Layout.preferredHeight: 62
                    radius: width / 2
                    color: "#e7edf4"
                    border.color: "#b3ffffff"
                    Image {
                        anchors.centerIn: parent
                        width: 36
                        height: width
                        source: "logo.png"
                        fillMode: Image.PreserveAspectFit
                    }
                }
                Caption {
                    visible: root.otherAccount || root.accounts.length < 2
                    Layout.alignment: Qt.AlignHCenter
                    Layout.fillWidth: true
                    Layout.preferredHeight: 32
                    text: root.otherAccount
                        ? (root.accounts.length ? qsTr("Other account") : qsTr("Sign in"))
                        : accounts.displayText
                    font.pixelSize: 18
                    font.weight: Font.DemiBold
                    horizontalAlignment: Text.AlignHCenter
                    verticalAlignment: Text.AlignVCenter
                    elide: Text.ElideRight
                }
                Controls.ComboBox {
                    id: accounts
                    visible: !root.otherAccount && root.accounts.length > 1
                    enabled: root.idle
                    Layout.fillWidth: true
                    implicitHeight: 32
                    model: root.accounts
                    textRole: "name"
                    Accessible.name: qsTr("Account")
                    palette.window: "#1d2938"
                    palette.text: "#f4f8ff"
                    palette.highlightedText: "white"
                    palette.highlight: "#577493"
                    palette.mid: "#54667d"
                    palette.light: "#36495e"
                    palette.midlight: "#465c74"
                    background: Rectangle {
                        radius: height / 2
                        color: accounts.hovered || accounts.down ? "#28ffffff" : "transparent"
                        border.color: accounts.activeFocus ? "#b3ffffff" : "transparent"
                    }
                    indicator: Caption {
                        x: accounts.width - width - 14
                        anchors.verticalCenter: parent.verticalCenter
                        text: "\u2304"
                    }
                    contentItem: Caption {
                        leftPadding: 26
                        rightPadding: 26
                        text: accounts.displayText
                        font.pixelSize: 18
                        font.weight: Font.DemiBold
                        horizontalAlignment: Text.AlignHCenter
                        verticalAlignment: Text.AlignVCenter
                        elide: Text.ElideRight
                    }
                    onActivated: {
                        root.authenticationRejected = false;
                        root.notice = "";
                        password.clear();
                        password.forceActiveFocus();
                    }
                }
                LoginField {
                    id: other
                    visible: root.otherAccount
                    enabled: root.idle
                    Layout.fillWidth: true
                    placeholderText: qsTr("Username")
                    onAccepted: password.forceActiveFocus()
                }
                Caption {
                    visible: root.responding && root.prompt !== ""
                    Layout.fillWidth: true
                    text: root.prompt
                    horizontalAlignment: Text.AlignHCenter
                    wrapMode: Text.Wrap
                }
                Item {
                    Layout.fillWidth: true
                    implicitHeight: 44

                    LoginField {
                        id: password
                        anchors.fill: parent
                        enabled: root.idle || root.responding
                        rejected: root.authenticationRejected && text === ""
                        rightPadding: signIn.width + 2
                        placeholderText: root.responding && root.echo ? qsTr("Response") : qsTr("Password")
                        echoMode: root.responding && root.echo ? TextInput.Normal : TextInput.Password
                        inputMethodHints: Qt.ImhSensitiveData | Qt.ImhNoPredictiveText
                        onAccepted: root.submit()
                    }
                    Controls.Button {
                        id: signIn
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        width: 44
                        height: width
                        enabled: Greetd.available && root.username !== "" && password.text !== ""
                            && (root.idle || root.responding)
                        Accessible.name: qsTr("Sign in")
                        onClicked: root.submit()
                        background: Item {
                            Rectangle {
                                anchors.centerIn: parent
                                width: 34
                                height: width
                                radius: width / 2
                                color: !signIn.enabled ? "#24ffffff"
                                    : signIn.down ? "#c5d6e8" : signIn.hovered ? "#ffffff" : "#edf5ff"
                                border.color: signIn.visualFocus ? "#80bbff" : "transparent"
                            }
                        }
                        contentItem: Caption {
                            text: "\u2192"
                            font.pixelSize: 21
                            color: signIn.enabled ? "#172431" : "#b8c5d3"
                            horizontalAlignment: Text.AlignHCenter
                            verticalAlignment: Text.AlignVCenter
                        }
                    }
                }
            }

            Column {
                id: footer
                anchors.top: access.bottom
                anchors.topMargin: 6
                anchors.horizontalCenter: access.horizontalCenter
                width: access.width
                spacing: 2

                Caption {
                    visible: root.notice !== ""
                    width: parent.width
                    wrapMode: Text.Wrap
                    horizontalAlignment: Text.AlignHCenter
                    color: root.authenticationRejected ? "#ffc4c0" : "#edf5ff"
                    text: root.notice
                }
                Controls.Button {
                    id: switchAccount
                    visible: root.accounts.length > 0
                    enabled: root.idle
                    anchors.horizontalCenter: parent.horizontalCenter
                    flat: true
                    implicitHeight: 26
                    text: root.otherAccount ? qsTr("Choose an account") : qsTr("Other account")
                    contentItem: Caption {
                        text: switchAccount.text
                        color: switchAccount.hovered ? "white" : "#d7e3ef"
                        font.underline: switchAccount.hovered || switchAccount.visualFocus
                        horizontalAlignment: Text.AlignHCenter
                        verticalAlignment: Text.AlignVCenter
                    }
                    onClicked: {
                        root.authenticationRejected = false;
                        root.notice = "";
                        password.clear();
                        root.otherAccount = !root.otherAccount;
                        (root.otherAccount ? other : password).forceActiveFocus();
                    }
                }
            }
        }
    }

    component Caption: Text {
        color: "#f4f8ff"
        font.family: "Inter"
        font.pixelSize: 14
        textFormat: Text.PlainText
    }

    component LoginField: Controls.TextField {
        id: field
        property bool rejected: false
        implicitHeight: 44
        maximumLength: 256
        leftPadding: 17
        rightPadding: 17
        color: "#f4f8ff"
        placeholderTextColor: "#d7e3ef"
        font.family: "Inter"
        font.pixelSize: 15
        Accessible.name: placeholderText
        background: Rectangle {
            color: "#c4151c26"
            radius: height / 2
            border.color: field.rejected ? "#ffc4c0" : field.activeFocus ? "#b3ffffff" : "#55ffffff"
        }
    }
}
