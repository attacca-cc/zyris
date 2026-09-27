//! The phone app: Android and iOS.
//!
//! **A conversation client, not a node that does things.** Everything the desktop app announces
//! — a shell, the files, the screen, input, MCP servers, file transfer — acts on the computer it
//! runs on, and a phone's sandbox allows none of it; so this connects with no capabilities at
//! all. What it keeps is the window: enrolment, the connection, and the Conversation screen,
//! typed. Speech waits for whisper and Supertonic to build for these targets (`zyris-voice`'s
//! `conversation` feature is the part that already does).
//!
//! The commands keep the desktop's names and answer shapes, so the window is the same code.

use std::sync::Arc;

use tauri::{Emitter, Manager, State};
use zyris_runtime::{CoreEvent, EventBus};

/// Has to match `EVENT_NAME` in `bridge.rs` and `state.ts`.
const EVENT_NAME: &str = "core-event";
/// Has to match `VOICE_TRACE_NAME` in `bridge.rs` and `state.ts`.
const VOICE_TRACE_NAME: &str = "voice-trace";
/// The desktop's, for the same reason: `broadcast` needs a number.
const EVENT_CAPACITY: usize = 256;
/// The instance name the keychain and the data directory are known by.
const INSTANCE: &str = "zyris";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let runtime = tokio::runtime::Runtime::new().expect("a tokio runtime");
    let handle = runtime.handle().clone();
    let bus = EventBus::new(EVENT_CAPACITY);

    let setup_bus = bus.clone();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(bus)
        .invoke_handler(tauri::generate_handler![
            latest_event,
            pending_peer,
            peer_fingerprint,
            open_verification_url,
            voice_state,
            set_read_aloud,
            stop_speaking,
            send_conversation_text,
            conversation_sessions,
            choose_conversation_session,
            new_conversation_session,
            conversation_history,
        ])
        .setup(move |app| {
            // **Where everything this app keeps lives.** The runtime finds its secrets and data
            // through `directories`, which on Android reads `HOME` — and an app process has none —
            // so it is pointed at the app's own private directory before anything asks.
            let data = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data)?;
            // SAFETY: set once, before any thread this program starts reads the environment.
            unsafe { std::env::set_var("HOME", &data) };
            #[cfg(target_os = "android")]
            {
                if let Err(error) = init_tls_verifier() {
                    tracing::error!(%error, "could not set up certificate verification; enrolment will fail");
                }
                // The websocket reads roots from files: the system store is a directory of them.
                // SAFETY: as for HOME above.
                unsafe { std::env::set_var("SSL_CERT_DIR", "/system/etc/security/cacerts") };
            }
            #[cfg(target_os = "ios")]
            match write_root_certificates(&data) {
                // SAFETY: as for HOME above.
                Ok(file) => unsafe { std::env::set_var("SSL_CERT_FILE", file) },
                Err(error) => tracing::error!(%error, "could not write the root certificates; the connection will fail"),
            }

            let voice = Arc::new(zyris_voice::start(Some(&data)));
            let identity =
                zyris_runtime::identity::Identity::new(zyris_runtime::secret::SecretStore::new(INSTANCE));
            let hook = voice.clone();
            let connector = zyris_runtime::connection::Connector::new(identity, setup_bus.clone())
                .add_connect_hook(move |connection| {
                    let voice = hook.clone();
                    async move { voice.on_connect(connection).await }
                });

            forward_core(app.handle().clone(), setup_bus.clone(), &handle);
            forward_traces(app.handle().clone(), voice.traces(), &handle);
            app.manage(voice);
            handle.spawn(connector.run());
            // `tauri.conf.json` declares the window hidden, for the desktop's tray-first start;
            // a phone app has no tray to open it from.
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the Zyris app ended with an error");
    drop(runtime);
}

/// **Why the phone stopped at "Asking Attacca for a code".** Enrolment and the connection verify
/// TLS through `rustls-platform-verifier`, which on Android asks the platform's trust store over
/// JNI and panics on first use unless it was given the JVM and the app's Context. The panic took
/// the connector task with it, so the window waited for a code forever. The Kotlin half it calls
/// is added to the Gradle project by `mobile.yml`.
#[cfg(target_os = "android")]
fn init_tls_verifier() -> Result<(), jni::errors::Error> {
    let android = ndk_context::android_context();
    // SAFETY: both pointers come from the activity that started this process and live as long as
    // it; the Context is a global reference, which this never deletes.
    let vm = unsafe { jni::JavaVM::from_raw(android.vm().cast()) };
    vm.attach_current_thread(|env| {
        let context = unsafe { jni::objects::JObject::from_raw(env, android.context().cast()) };
        rustls_platform_verifier::android::init_with_env(env, context)
    })
}

/// Mozilla's roots as one PEM file in the app's data directory, for the websocket's TLS, which
/// reads certificates from files and finds none on iOS.
#[cfg(target_os = "ios")]
fn write_root_certificates(dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    use base64::Engine;
    let mut pem = String::new();
    for certificate in webpki_root_certs::TLS_SERVER_ROOT_CERTS {
        let encoded = base64::engine::general_purpose::STANDARD.encode(certificate.as_ref());
        pem.push_str("-----BEGIN CERTIFICATE-----\n");
        for line in encoded.as_bytes().chunks(64) {
            pem.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
            pem.push('\n');
        }
        pem.push_str("-----END CERTIFICATE-----\n");
    }
    let file = dir.join("root-certificates.pem");
    std::fs::write(&file, pem)?;
    Ok(file)
}

fn forward_core(app: tauri::AppHandle, bus: EventBus, runtime: &tokio::runtime::Handle) {
    let mut events = bus.subscribe();
    runtime.spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let _ = app.emit(EVENT_NAME, &event);
                }
                // Behind: the latest is all a window needs to redraw.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    if let Some(event) = bus.latest() {
                        let _ = app.emit(EVENT_NAME, &event);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}

fn forward_traces(
    app: tauri::AppHandle,
    mut traces: tokio::sync::broadcast::Receiver<zyris_voice::Trace>,
    runtime: &tokio::runtime::Handle,
) {
    runtime.spawn(async move {
        loop {
            match traces.recv().await {
                Ok(step) => {
                    let _ = app.emit(VOICE_TRACE_NAME, &step);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}

#[tauri::command]
fn latest_event(bus: State<EventBus>) -> Option<CoreEvent> {
    bus.latest()
}

/// No peers on a phone: it runs no file transfer, so nothing can ask to approve one.
#[tauri::command]
fn pending_peer() -> Option<()> {
    None
}

#[tauri::command]
fn peer_fingerprint() -> Option<String> {
    None
}

/// Opens the enrolment URL in the phone's browser. Only https, as on the desktop.
#[tauri::command]
fn open_verification_url(app: tauri::AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    if !url.starts_with("https://") {
        return Err("refusing to open a non-https url".to_string());
    }
    app.opener().open_url(url, None::<&str>).map_err(|error| error.to_string())
}

/// The Voice screen's answer, in the desktop's shape: what the voice says of itself, and a
/// push-to-talk key that cannot exist here.
fn screen(view: zyris_voice::view::VoiceView) -> serde_json::Value {
    serde_json::json!({
        "voice": view,
        "hotkey": { "state": "unavailable", "reason": "a phone has no global keyboard shortcut" },
    })
}

#[tauri::command]
async fn voice_state(voice: State<'_, Arc<zyris_voice::Voice>>) -> Result<serde_json::Value, String> {
    Ok(screen(voice.look().await))
}

#[tauri::command]
async fn set_read_aloud(
    read_aloud: bool,
    voice: State<'_, Arc<zyris_voice::Voice>>,
) -> Result<serde_json::Value, String> {
    Ok(screen(voice.set_read_aloud(read_aloud).await))
}

#[tauri::command]
fn stop_speaking(voice: State<'_, Arc<zyris_voice::Voice>>) {
    voice.stop_speaking();
}

#[tauri::command]
async fn send_conversation_text(text: String, voice: State<'_, Arc<zyris_voice::Voice>>) -> Result<(), String> {
    voice.send_text(text).await
}

#[tauri::command]
async fn conversation_sessions(
    voice: State<'_, Arc<zyris_voice::Voice>>,
) -> Result<zyris_voice::view::SessionsView, String> {
    voice.sessions().await
}

#[tauri::command]
async fn choose_conversation_session(
    session: String,
    voice: State<'_, Arc<zyris_voice::Voice>>,
) -> Result<zyris_voice::view::SessionsView, String> {
    voice.choose_session(session).await
}

#[tauri::command]
async fn new_conversation_session(
    project: Option<String>,
    agent: Option<String>,
    voice: State<'_, Arc<zyris_voice::Voice>>,
) -> Result<zyris_voice::view::SessionsView, String> {
    voice.new_session(project, agent).await
}

#[tauri::command]
async fn conversation_history(
    voice: State<'_, Arc<zyris_voice::Voice>>,
) -> Result<zyris_voice::view::HistoryView, String> {
    voice.history().await
}
