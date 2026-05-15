/* Minimal slideshow shown during the install. Replaced with branded
 * marketing content in M9+. */
import QtQuick 2.0;
import calamares.slideshow 1.0;

Presentation {
    id: presentation
    Timer {
        interval: 5000
        running: true
        repeat: true
        onTriggered: presentation.goToNextSlide()
    }

    Slide {
        Text {
            anchors.centerIn: parent
            text: "Installing Claw OS..."
            wrapMode: Text.WordWrap
            width: parent.width
            horizontalAlignment: Text.Center
        }
    }

    Slide {
        Text {
            anchors.centerIn: parent
            text: "Claw OS is an agent-native operating system.\n\nThe `cos` supervisor is a single Rust binary that orchestrates apps, skills, and a built-in browser engine."
            wrapMode: Text.WordWrap
            width: parent.width
            horizontalAlignment: Text.Center
        }
    }

    Slide {
        Text {
            anchors.centerIn: parent
            text: "After install, run:\n\n  sudo apt update && sudo apt upgrade\n\nto pick up newer Claw OS releases from the official apt repository."
            wrapMode: Text.WordWrap
            width: parent.width
            horizontalAlignment: Text.Center
        }
    }
}
