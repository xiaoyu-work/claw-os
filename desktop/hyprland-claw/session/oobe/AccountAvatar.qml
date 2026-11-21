import QtQuick

Rectangle {
    id: badge

    property string avatarId: ""
    implicitWidth: 62
    implicitHeight: 62
    radius: width / 2
    color: "#e7edf4"
    border.color: "#b3ffffff"

    Image {
        id: portrait
        anchors.fill: parent
        source: badge.avatarId ? "file:///usr/share/claw-os/avatars/" + badge.avatarId + ".png" : ""
        fillMode: Image.PreserveAspectFit
    }

    Image {
        anchors.centerIn: parent
        width: Math.round(badge.width * 0.58)
        height: width
        visible: portrait.status !== Image.Ready
        source: "logo.png"
        fillMode: Image.PreserveAspectFit
    }
}
