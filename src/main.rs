use std::collections::HashMap;
use std::sync::Arc;

use qmetaobject::prelude::*;
use qmetaobject::{QObjectBox, QString, QStringList, QUrl, QVariant, queued_callback};

use tokio::sync::{mpsc, watch};
use tracing::{debug, info};

use crate::event::{
    AcquireServer, IpcEvent, ReleaseServer, RequestServer, RequestWebview, SetWebview, TokioEvent,
    UiCmd, UiEvent, WebCmd, WebEvent,
};

mod event;
mod ipc;
mod webserver;

const QML: &str = include_str!("webview.qml");

// --- Shared state from UI -> tokio
#[derive(Clone, Default)]
struct SyncData {
    connectors: Vec<String>,
}

#[allow(non_snake_case)]
#[derive(QObject, Default)]
struct Bridge {
    base: qt_base_class!(trait QObject),

    sync_tx: Option<watch::Sender<Arc<SyncData>>>,

    // Called from QML, it gives us the connector names whenever they update
    setMonitorNames: qt_method!(
        fn setMonitorNames(&self, names: QStringList) {
            let qs: Vec<QString> = names.into();
            let connectors: Vec<String> = qs.into_iter().map(|q| q.to_string()).collect();

            if let Some(sync_tx) = &self.sync_tx {
                let _ = sync_tx.send(Arc::new(SyncData {
                    connectors: connectors.clone(),
                }));
            }

            info!("Monitors:");
            for (i, c) in connectors.iter().enumerate() {
                info!("  {i}: {c}");
            }
        }
    ),
}

// --- Tokio runtime thread ---
fn start_tokio(
    ui_tx: mpsc::UnboundedSender<UiCmd>,
    mut ui_event_rx: mpsc::UnboundedReceiver<UiEvent>,
    synx_rx: watch::Receiver<Arc<SyncData>>,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");

        rt.block_on(async move {
            let (tokio_tx, mut tokio_rx) = mpsc::unbounded_channel::<TokioEvent>();
            let (web_tx, web_rx) = mpsc::unbounded_channel::<WebCmd>();

            tokio::spawn(ipc::ipc_server(tokio_tx.clone()));
            info!(target: "tokio", "Started ipc_server");

            tokio::spawn(webserver::web_manager(tokio_tx.clone(), web_rx));
            info!(target: "tokio", "Started web_manager");

            loop {
                tokio::select! {
                    ui_evt = ui_event_rx.recv() => {
                        match ui_evt {
                            Some(UiEvent::ReleaseServer(release_server)) => {
                                debug!(target: "tokio", release_server=?release_server, "Received from UI");
                                let _ = web_tx.send(WebCmd::ReleaseServer(release_server));
                            }
                            None => break,
                        }
                    }

                    tk = tokio_rx.recv() => {
                        match tk {
                            Some(event) => match event {
                                TokioEvent::IpcEvent(ipc_event) => match ipc_event {
                                    IpcEvent::RequestServer(request_server) => {
                                        debug!(target: "tokio", request_server=?request_server, "Received");
                                        handle_request_server(request_server, &synx_rx, &web_tx);
                                    }
                                    IpcEvent::RequestWebview(request_webview) => {
                                        debug!(target: "tokio", request_webview=?request_webview, "Received");
                                        handle_request_webview(request_webview, &synx_rx, &ui_tx);
                                    }
                                },

                                TokioEvent::WebEvent(web_event) => match web_event {
                                    WebEvent::SetWebview(set_webview) => {
                                        debug!(target: "tokio", set_webview=?set_webview, "WebEvent -> UI");
                                        let _ = ui_tx.send(UiCmd::SetWebview(set_webview));
                                    }
                                },
                            },
                            None => break,
                        }
                    }
                }
            }
        });
    });
}

fn handle_request_server(
    request_server: RequestServer,
    synx_rx: &watch::Receiver<Arc<SyncData>>,
    web_tx: &mpsc::UnboundedSender<WebCmd>,
) {
    if let Some(connector) = request_server.connector.clone() {
        let acquire = AcquireServer {
            path: request_server.path,
            connector,
        };
        let _ = web_tx.send(WebCmd::AcquireServer(acquire));
    } else {
        let sync: Arc<SyncData> = synx_rx.borrow().clone();
        for connector in sync.connectors.iter() {
            let acquire = AcquireServer {
                path: request_server.path.clone(),
                connector: connector.clone(),
            };
            let _ = web_tx.send(WebCmd::AcquireServer(acquire));
        }
    }
}

fn handle_request_webview(
    request_webview: RequestWebview,
    synx_rx: &watch::Receiver<Arc<SyncData>>,
    ui_tx: &mpsc::UnboundedSender<UiCmd>,
) {
    if let Some(connector) = request_webview.connector.clone() {
        let set_webview = SetWebview {
            url: request_webview.url,
            path: None,
            connector,
        };
        let _ = ui_tx.send(UiCmd::SetWebview(set_webview));
    } else {
        let sync: Arc<SyncData> = synx_rx.borrow().clone();
        for connector in sync.connectors.iter() {
            let set_webview = SetWebview {
                url: request_webview.url.clone(),
                path: None,
                connector: connector.clone(),
            };
            let _ = ui_tx.send(UiCmd::SetWebview(set_webview));
        }
    }
}

fn main() {
    if cfg!(debug_assertions) {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .init();
    }

    // unsafe { std::env::set_var("QT_WAYLAND_SHELL_INTEGRATION", "layer-shell") };
    // unsafe { std::env::set_var("QT_QPA_PLATFORM", "wayland") };

    let (ui_tx, mut ui_rx) = mpsc::unbounded_channel::<UiCmd>();
    let (ui_event_tx, ui_event_rx) = mpsc::unbounded_channel::<UiEvent>();
    let (sync_tx, sync_rx) = watch::channel(Arc::new(SyncData::default()));

    info!(target: "main", "Starting tokio thread");
    start_tokio(ui_tx.clone(), ui_event_rx, sync_rx.clone());

    // The following QT stuff is quite unrusty, but we'll migrate to QT
    // BRIDGES whenever that releases
    info!(target: "main", "Starting Qt/QML UI");
    let mut engine = QmlEngine::new();
    let bridge = QObjectBox::new(Bridge::default());
    let bridge_pinned = bridge.pinned();
    bridge_pinned.borrow_mut().sync_tx = Some(sync_tx.clone());
    engine.set_object_property("bridge".into(), bridge_pinned);

    engine.load_data(QML.into());

    let engine_ptr: *mut QmlEngine = &mut engine;
    let qt_set_wallpaper = queued_callback(move |(connector, url): (QString, QString)| unsafe {
        let qurl = QUrl::from_user_input(url);
        let args = [QVariant::from(connector), QVariant::from(qurl)];
        (&mut *engine_ptr).invoke_method_noreturn(QByteArray::from("setWallpaper"), &args);
    });

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build ui adapter runtime");

        rt.block_on(async move {
            let mut last_paths: HashMap<String, Option<String>> = HashMap::new();

            while let Some(cmd) = ui_rx.recv().await {
                match cmd {
                    UiCmd::SetWebview(set_webview) => {
                        let entry = last_paths
                            .entry(set_webview.connector.clone())
                            .or_insert(None);

                        let old_path = entry.clone();
                        *entry = set_webview.path.clone();

                        if let Some(old) = old_path {
                            let _ = ui_event_tx
                                .send(UiEvent::ReleaseServer(ReleaseServer { path: old }));
                        }

                        qt_set_wallpaper((
                            QString::from(set_webview.connector),
                            QString::from(set_webview.url),
                        ));
                    }
                }
            }
        });
    });

    engine.exec();
}
