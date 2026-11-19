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
        color: "#161d2c"
        onClosing: event => event.accepted = false

        GlassBackdrop {
            id: background
            anchors.fill: parent
        }
        GlassCard {
            id: card
            anchors.centerIn: parent
            width: Math.min(402, window.width - 30)
            height: column.implicitHeight + 62
            tint: "#ad172131"
            wallpaper: background.wallpaper

            ColumnLayout {
                id: column
                anchors.horizontalCenter: parent.horizontalCenter
                anchors.top: parent.top
                anchors.topMargin: 31
                width: parent.width - 58
                spacing: 13

                BrandMark {
                    Layout.alignment: Qt.AlignHCenter
                    Layout.preferredWidth: 60
                    Layout.preferredHeight: 60
                }
                Caption {
                    Layout.alignment: Qt.AlignHCenter
                    text: "Claw OS"
                    font.pixelSize: 25
                    font.weight: Font.DemiBold
                }
                Caption {
                    Layout.alignment: Qt.AlignHCenter
                    Layout.bottomMargin: 7
                    text: qsTr("Sign in to your desktop")
                    color: "#d0ddee"
                }
                Controls.ComboBox {
                    id: accounts
                    visible: !root.otherAccount
                    enabled: root.idle
                    Layout.fillWidth: true
                    implicitHeight: 43
                    model: root.accounts
                    textRole: "name"
                    Accessible.name: qsTr("Account")
                    background: Rectangle {
                        color: "#ab141e2c"
                        radius: 11
                        border.color: accounts.activeFocus ? "#a3caff" : "#777f9fc4"
                    }
                    contentItem: Caption {
                        leftPadding: 13
                        rightPadding: 29
                        text: accounts.displayText
                        verticalAlignment: Text.AlignVCenter
                        elide: Text.ElideRight
                    }
                    indicator: SetupIcon {
                        x: accounts.width - 28
                        anchors.verticalCenter: parent.verticalCenter
                        width: 18
                        height: 18
                        name: "down"
                        color: "#c6dcfb"
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
                LoginField {
                    id: password
                    Layout.fillWidth: true
                    enabled: root.idle || root.responding
                    placeholderText: root.prompt || qsTr("Password")
                    echoMode: root.responding && root.echo ? TextInput.Normal : TextInput.Password
                    inputMethodHints: Qt.ImhSensitiveData | Qt.ImhNoPredictiveText
                    onAccepted: root.submit()
                }
                Caption {
                    Layout.fillWidth: true
                    visible: root.notice !== ""
                    wrapMode: Text.Wrap
                    color: root.idle ? "#ffd0c9" : "#d0ddee"
                    text: root.notice
                }
                Controls.Button {
                    id: signIn
                    Layout.fillWidth: true
                    implicitHeight: 43
                    enabled: Greetd.available && root.username !== "" && password.text !== ""
                        && (root.idle || root.responding)
                    text: qsTr("Sign in")
                    onClicked: root.submit()
                    background: Rectangle {
                        radius: 11
                        color: !signIn.enabled ? "#7086a5"
                            : signIn.down ? "#95b8ef" : signIn.hovered ? "#c5dcff" : "#b3d1ff"
                        border.color: signIn.visualFocus ? "white" : "transparent"
                    }
                    contentItem: Caption {
                        text: signIn.text
                        font.weight: Font.DemiBold
                        color: "#14253f"
                        horizontalAlignment: Text.AlignHCenter
                        verticalAlignment: Text.AlignVCenter
                    }
                }
                Controls.Button {
                    id: switchAccount
                    visible: root.accounts.length > 0
                    enabled: root.idle
                    Layout.alignment: Qt.AlignHCenter
                    flat: true
                    text: root.otherAccount ? qsTr("Choose an account") : qsTr("Other account")
                    contentItem: Caption { text: switchAccount.text; color: "#d1e4ff" }
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
        color: "#f0f5ff"
        font.family: "Inter"
        font.pixelSize: 14
        textFormat: Text.PlainText
    }

    component LoginField: Controls.TextField {
        id: field
        implicitHeight: 43
        maximumLength: 256
        leftPadding: 13
        rightPadding: 13
        color: "#f0f5ff"
        placeholderTextColor: "#c3d3e6"
        font.family: "Inter"
        font.pixelSize: 15
        Accessible.name: placeholderText
        background: Rectangle {
            color: "#ab141e2c"
            radius: 11
            border.color: field.activeFocus ? "#a3caff" : "#777f9fc4"
        }
    }
}
