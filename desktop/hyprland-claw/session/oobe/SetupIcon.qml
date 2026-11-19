import QtQuick
import QtQuick.Shapes

Item {
    id: root

    required property string name
    required property color color
    implicitWidth: 24
    implicitHeight: 24

    Shape {
        anchors.centerIn: parent
        width: 24
        height: 24
        scale: Math.min(root.width, root.height) / 24

        ShapePath {
            strokeColor: root.color
            fillColor: "transparent"
            strokeWidth: 1.8
            capStyle: ShapePath.RoundCap
            joinStyle: ShapePath.RoundJoin

            PathSvg {
                path: {
                    switch (root.name) {
                        case "welcome": return "M12 2 L14.5 9.5 L22 12 L14.5 14.5 L12 22 L9.5 14.5 L2 12 L9.5 9.5 Z";
                        case "network": return "M2 8 Q12 -0.5 22 8 M5.5 12 Q12 6.5 18.5 12 M9 16 Q12 13 15 16 M12 20 L12 20.25";
                        case "drivers": return "M7 5 H17 Q19 5 19 7 V17 Q19 19 17 19 H7 Q5 19 5 17 V7 Q5 5 7 5 Z M9 9 H15 V15 H9 Z M9 2 V5 M15 2 V5 M9 19 V22 M15 19 V22 M2 9 H5 M2 15 H5 M19 9 H22 M19 15 H22";
                        case "model": return "M6 4 H18 Q21 4 21 7 V14 Q21 17 18 17 H10 L5 21 V17 Q3 17 3 14 V7 Q3 4 6 4 Z M8 10.5 H16";
                        case "account": return "M12 3 A4 4 0 1 1 11.99 3 M4 21 V19 Q4 14 9 14 H15 Q20 14 20 19 V21";
                        case "keyboard": return "M3 6 H21 V18 H3 Z M6 10 H7 M10 10 H11 M14 10 H15 M18 10 H19 M7 14 H17";
                        case "location": return "M12 22 Q4 14 4 9 A8 8 0 1 1 20 9 Q20 14 12 22 Z M12 6 A3 3 0 1 1 11.99 6";
                        case "layout": return "M3 3 H21 V21 H3 Z M3 8 H21 M9 8 V21";
                        case "finish": return "M12 2 A10 10 0 1 1 12 22 A10 10 0 1 1 12 2 Z M8 12 L11 15 L16 9";
                        case "down": return "M6 9 L12 15 L18 9";
                        default: return "";
                    }
                }
            }
        }
    }
}
