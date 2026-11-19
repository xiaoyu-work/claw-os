import QtQuick

Item {
    id: card
    required property Image wallpaper
    property color tint: "#c81a2234"
    property real radius: 26

    Rectangle {
        anchors.fill: parent
        anchors.margins: -4
        anchors.topMargin: 8
        color: "#330a101c"
        radius: card.radius + 3
    }
    Canvas {
        anchors.fill: parent
        readonly property url source: Qt.resolvedUrl("wallpaper-blurred.jpg")
        readonly property rect crop: Qt.rect(
            (card.wallpaper.width - card.wallpaper.paintedWidth) / 2 - card.x,
            (card.wallpaper.height - card.wallpaper.paintedHeight) / 2 - card.y,
            card.wallpaper.paintedWidth, card.wallpaper.paintedHeight)

        Component.onCompleted: loadImage(source)
        onImageLoaded: requestPaint()
        onCropChanged: requestPaint()
        onPaint: {
            const ctx = getContext("2d");
            ctx.reset();
            ctx.clearRect(0, 0, width, height);
            if (!isImageLoaded(source))
                return;
            ctx.save();
            ctx.beginPath();
            ctx.roundedRect(0, 0, width, height, card.radius, card.radius);
            ctx.clip();
            ctx.drawImage(source, crop.x, crop.y, crop.width, crop.height);
            ctx.restore();
        }
    }
    Rectangle {
        anchors.fill: parent
        color: card.tint
        radius: card.radius
        border.color: "#529fc2e8"
    }
}
