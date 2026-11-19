import QtQuick

Rectangle {
    id: mark
    implicitWidth: 42
    implicitHeight: 42
    radius: width * 0.29
    color: "#e7effb"
    border.color: "#c2d6f1"
    Image {
        anchors.centerIn: parent
        width: mark.width * 0.65
        height: width
        fillMode: Image.PreserveAspectFit
        source: "logo.png"
    }
}
