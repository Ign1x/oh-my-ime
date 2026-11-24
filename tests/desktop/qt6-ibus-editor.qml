import QtQuick
import QtQuick.Controls

ApplicationWindow {
    visible: true
    width: 640
    height: 120
    title: "oh-my-ime Qt6 native probe"

    TextField {
        id: entry
        anchors.fill: parent
        placeholderText: "oh-my-ime Qt6 native probe"

        onTextChanged: {
            console.log("text", JSON.stringify(text))
            if (text === "中国")
                Qt.quit()
        }

        Component.onCompleted: {
            forceActiveFocus()
            console.log("ready true")
        }
    }
}
