import QtQuick

Item {
    property alias wallpaper: photo
    Rectangle {
        anchors.fill: parent
        color: "#161d2c"
    }
    Image {
        id: photo
        anchors.fill: parent
        source: "file:///usr/share/backgrounds/claw-os/yuos-default.jpg"
        fillMode: Image.PreserveAspectCrop
        sourceSize.width: Math.min(3200, Math.max(1280, Math.ceil(width * 1.5)))
        asynchronous: true
    }
    Rectangle {
        anchors.fill: parent
        gradient: Gradient {
            GradientStop { position: 0; color: "#14121b35" }
            GradientStop { position: 1; color: "#57101624" }
        }
    }
}
