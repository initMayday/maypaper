
// src/webview.qml
import QtQuick 2.15
import QtQml 2.15
import QtQuick.Window 2.15
import QtWebEngine 1.0
import org.kde.layershell 1.0 as LayerShell

Item {
    id: app

    // Map: connectorName -> Window instance
    property var windowsByConnector: ({})

    // Called from Rust, sets one connector 
    function setWallpaper(connectorName, url) {
        const w = windowsByConnector[connectorName]
        if (w) {
            w.currentUrl = url
            return
        }
        console.log("setWallpaper: connector not found:", connectorName)
    }

    function reportMonitorsToRust() {
        if (!bridge) {
            console.log("bridge object not set (Rust didn't expose it?)")
            return
        }

        let names = []
        for (let i = 0; i < Application.screens.length; ++i) {
            names.push(Application.screens[i].name)
        }
        bridge.setMonitorNames(names)
    }


    // Only rebuild windows that we need to
    function rebuildWindows() {
        // Keep a handle to the old map so we can diff it.
        const old = windowsByConnector
        const next = ({})

        // Reuse or create windows for current screens
        for (let i = 0; i < Application.screens.length; ++i) {
            const scr = Application.screens[i]
            const name = scr.name

            let w = old[name]
            if (w) {
                // Reuse existing window (keeps its currentUrl, so wallpaper stays)
                w.targetScreen = scr
                w.screen = scr            // ensure Qt moves it if needed
            } else {
                // New monitor -> create new window (starts blank)
                w = wallpaperWindow.createObject(null, {
                    targetScreen: scr,
                    connectorName: name
                })
            }

            next[name] = w
        }

        // Destroy windows for monitors that are gone
        for (let name in old) {
            if (!(name in next)) {
                old[name].destroy()
            }
        }

        windowsByConnector = next
        reportMonitorsToRust()
    }

    Connections {
        target: Application
        function onScreensChanged() {
            console.log("Application.screensChanged -> rebuildWindows()")
            rebuildWindows()
        }
    }

    Component {
        id: wallpaperWindow

        Window {
            id: root
            visible: true
            color: "black"

            property var targetScreen
            property string connectorName: ""

            // By default, load a grey background so we don't sear people's eyes out
            property url currentUrl: "data:text/html,<html><body style='margin:0;background:%23222222;'></body></html>"

            screen: targetScreen
            width: Screen.width
            height: Screen.height

            LayerShell.Window.layer: LayerShell.Window.LayerBackground
            LayerShell.Window.anchors: LayerShell.Window.AnchorTop
                                     | LayerShell.Window.AnchorBottom
                                     | LayerShell.Window.AnchorLeft
                                     | LayerShell.Window.AnchorRight
            LayerShell.Window.exclusionZone: -1
            LayerShell.Window.keyboardInteractivity: LayerShell.Window.KeyboardInteractivity
            LayerShell.Window.scope: "maypaper-wallpaper"

            flags: Qt.FramelessWindowHint | Qt.Tool

            WebEngineView {
                id: web
                anchors.fill: parent
                url: root.currentUrl
            }

            function pushFocusStateToWeb() {
                const activeNow = root.active

                // Mute/unmute audio (QtWebEngineView uses audioMuted)
                if ("audioMuted" in web) {
                    web.audioMuted = !activeNow
                } else if ("muted" in web) {
                    web.muted = !activeNow
                }

                const js =
                    "globalThis.maypaper?.setFocused(" + (activeNow ? "true" : "false") + ");\n" +
                    "//# sourceURL=maypaper://focus"

                web.runJavaScript(js)
            }

            onActiveChanged: pushFocusStateToWeb()
            Component.onCompleted: pushFocusStateToWeb()
        }
    }

    Component.onCompleted: rebuildWindows()
}
