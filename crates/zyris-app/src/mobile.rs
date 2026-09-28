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
    // Android sends a process's stderr to logcat (tag `RustStdoutStderr`), so this is the phone
    // app's log. `RUST_LOG` is not something a phone user sets; `info` is what is kept.
    // `set_global_default` rather than `init`: `init` also claims the `log` facade, which Tauri
    // has already taken on Android, and fails as a whole when it cannot.
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "zyris phone app starting");
    let runtime = tokio::runtime::Runtime::new().expect("a tokio runtime");
    let handle = runtime.handle().clone();
    let bus = EventBus::new(EVENT_CAPACITY);

    let setup_bus = bus.clone();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_zyris_mobile::init())
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
            set_voice_listening,
            set_voice_device,
            set_voice_speaker,
            set_voice_compute,
            set_voice_volume,
            set_speaking_rate,
            set_speech_model,
            fetch_speech_model,
            fetch_voice_model,
            forget_speech_model,
            record_wake_take,
            clear_wake_word,
            push_to_talk,
            check_for_update,
            install_update,
            phone_status,
            phone_open_touch_settings,
            phone_open_files_settings,
            phone_allow_screen,
            phone_allow_notifications,
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
                // The websocket reads roots from files: the system store is a directory of them.
                // SAFETY: as for HOME above.
                unsafe { std::env::set_var("SSL_CERT_DIR", "/system/etc/security/cacerts") };
                // ONNX Runtime comes from the APK (Microsoft's AAR), loaded by name at first use.
                // SAFETY: as for HOME above.
                unsafe { std::env::set_var("ORT_DYLIB_PATH", "libonnxruntime.so") };
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
            let connector = zyris_runtime::connection::Connector::new(identity, setup_bus.clone());
            // Android lends its screen, touch and files to the agent; iOS allows none of them.
            #[cfg(target_os = "android")]
            let connector = connector.with_capabilities(zyris_runtime::LiveCapabilities::new(
                crate::phone::capabilities(app.handle(), data.clone()),
            ));
            let connector = connector
                .add_connect_hook(move |connection| {
                    let voice = hook.clone();
                    async move { voice.on_connect(connection).await }
                });

            forward_core(app.handle().clone(), setup_bus.clone(), &handle);
            forward_traces(app.handle().clone(), voice.traces(), &handle);
            app.manage(voice);
            handle.spawn(connector.run());
            // The foreground service that keeps the connection up off screen. Off the main
            // thread: the call waits for Kotlin, which answers on it.
            #[cfg(target_os = "android")]
            {
                use tauri_plugin_zyris_mobile::PhoneExt;
                let app = app.handle().clone();
                handle.spawn_blocking(move || {
                    if let Err(error) = app.phone().call::<serde_json::Value>("startBackground", ()) {
                        tracing::warn!(%error, "could not start the background connection");
                    }
                });
            }
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

/// **Why the phone stopped at "Asking Attacca for a code", and then crashed.** Enrolment and the
/// connection verify TLS through `rustls-platform-verifier`, which on Android asks the platform's
/// trust store over JNI and panics unless it was first handed the JVM and the app's Context.
/// 0.1.1 never did; 0.1.2 reached for them through `ndk-context`, which Tauri leaves unset, and
/// aborted at start. The Kotlin plugin calls this as it loads, with its Context in hand.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_cc_attacca_zyris_mobile_ZyrisPlugin_initTls<'caller>(
    mut env: jni::EnvUnowned<'caller>,
    _plugin: jni::objects::JObject<'caller>,
    context: jni::objects::JObject<'caller>,
) {
    env.with_env(|env| -> Result<(), jni::errors::Error> {
        // The audio stack (cpal on AAudio) finds the JVM through `ndk-context`, which Tauri
        // leaves unset; a global reference keeps the Context valid for the life of the process.
        // Once per process: the plugin, and so this call, comes again whenever Android recreates
        // the activity, and `ndk-context` asserts it is set only once.
        static AUDIO_CONTEXT: std::sync::Once = std::sync::Once::new();
        let vm = env.get_java_vm()?.get_raw();
        let global = env.new_global_ref(&context)?.into_raw();
        // SAFETY: a live JVM and a global reference that is never deleted.
        AUDIO_CONTEXT.call_once(|| unsafe { ndk_context::initialize_android_context(vm.cast(), global.cast()) });
        rustls_platform_verifier::android::init_with_env(env, context)
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
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

// ---- Updates and what the phone has allowed -----------------------------------------------------

#[derive(serde::Serialize)]
struct Available {
    version: String,
    notes: Option<String>,
}

/// The newer release, in the desktop's answer shape so the window's update notice is the same
/// code. An iPhone app is installed by the person's own signing tool, so it never offers one.
#[tauri::command]
async fn check_for_update(app: tauri::AppHandle) -> Result<Option<Available>, String> {
    #[cfg(target_os = "android")]
    {
        let current = app.package_info().version.to_string();
        let release = crate::phone::newer_release(&current)
            .await
            .inspect_err(|error| tracing::warn!(%error, "could not look for an update"))?;
        Ok(release.map(|r| Available { version: r.version, notes: r.notes }))
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Ok(None)
    }
}

/// Download the APK and open the system installer, which asks the person to confirm.
#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let current = app.package_info().version.to_string();
        let Some(release) = crate::phone::newer_release(&current).await? else {
            return Err("there is no newer release any more".to_string());
        };
        let cache = app.path().app_cache_dir().map_err(|error| error.to_string())?;
        crate::phone::install(&app, release, cache).await
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Err("updates on this phone come from the tool that installed it".to_string())
    }
}

/// Run one Kotlin command from a window command, off the main thread it answers on.
#[cfg(target_os = "android")]
async fn phone(app: tauri::AppHandle, command: &'static str, args: serde_json::Value) -> Result<serde_json::Value, String> {
    use tauri_plugin_zyris_mobile::PhoneExt;
    tokio::task::spawn_blocking(move || app.phone().call::<serde_json::Value>(command, args))
        .await
        .map_err(|error| error.to_string())?
}

#[cfg(not(target_os = "android"))]
async fn phone(_app: tauri::AppHandle, _command: &'static str, _args: serde_json::Value) -> Result<serde_json::Value, String> {
    Err("this phone does not let apps share their screen, touch or files".to_string())
}

/// Touch, screen, files and installs: what the person has turned on.
#[tauri::command]
async fn phone_status(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    phone(app, "status", serde_json::json!({})).await
}

#[tauri::command]
async fn phone_open_touch_settings(app: tauri::AppHandle) -> Result<(), String> {
    phone(app, "openTouchSettings", serde_json::json!({})).await.map(drop)
}

#[tauri::command]
async fn phone_open_files_settings(app: tauri::AppHandle) -> Result<(), String> {
    phone(app, "openFilesSettings", serde_json::json!({})).await.map(drop)
}

/// Android's screen-capture prompt, asked now rather than on the agent's first screenshot, when
/// the app might not be on screen to show it.
#[tauri::command]
async fn phone_allow_screen(app: tauri::AppHandle) -> Result<(), String> {
    phone(app, "screenshot", serde_json::json!({ "maxWidth": 64 })).await.map(drop)
}

#[tauri::command]
async fn phone_allow_notifications(app: tauri::AppHandle) -> Result<(), String> {
    phone(app, "requestPermissions", serde_json::json!({ "permissions": ["notifications"] })).await.map(drop)
}

// ---- Speech ---------------------------------------------------------------------------------------
//
// The desktop's Voice screen commands, answered the same way. An Android build compiled with
// `voice` hears and speaks on the phone; any other phone build answers with a voice that says it
// cannot, as the desktop's off build does.

type VoiceState<'a> = State<'a, Arc<zyris_voice::Voice>>;

/// Turning listening on asks Android for the microphone first: without it the stream opens and
/// hears nothing, with no error to say why.
#[tauri::command]
async fn set_voice_listening(app: tauri::AppHandle, listening: bool, voice: VoiceState<'_>) -> Result<serde_json::Value, String> {
    if listening {
        let answer = phone(app, "requestPermissions", serde_json::json!({ "permissions": ["microphone"] })).await;
        #[cfg(target_os = "android")]
        if answer?.get("microphone").and_then(|v| v.as_str()) != Some("granted") {
            return Err("Zyris needs the microphone to listen; allow it in the prompt or the app's settings.".to_string());
        }
        #[cfg(not(target_os = "android"))]
        let _ = answer;
    }
    Ok(screen(voice.set_listening(listening).await))
}

#[tauri::command]
async fn set_voice_device(device: zyris_voice::view::Choice, voice: VoiceState<'_>) -> Result<serde_json::Value, String> {
    Ok(screen(voice.choose(device).await))
}

#[tauri::command]
async fn set_voice_speaker(speaker: zyris_voice::view::Choice, voice: VoiceState<'_>) -> Result<serde_json::Value, String> {
    Ok(screen(voice.choose_speaker(speaker).await))
}

#[tauri::command]
async fn set_voice_compute(
    transcribe: Option<String>,
    speak: Option<String>,
    voice: VoiceState<'_>,
) -> Result<serde_json::Value, String> {
    Ok(screen(voice.choose_compute(transcribe, speak).await))
}

#[tauri::command]
async fn set_voice_volume(volume: f32, voice: VoiceState<'_>) -> Result<serde_json::Value, String> {
    Ok(screen(voice.choose_volume(volume).await))
}

#[tauri::command]
async fn set_speaking_rate(rate: f32, voice: VoiceState<'_>) -> Result<serde_json::Value, String> {
    Ok(screen(voice.choose_speaking_rate(rate).await))
}

#[tauri::command]
async fn set_speech_model(id: String, voice: VoiceState<'_>) -> Result<serde_json::Value, String> {
    Ok(screen(voice.choose_model(id).await))
}

#[tauri::command]
async fn fetch_speech_model(id: String, voice: VoiceState<'_>) -> Result<serde_json::Value, String> {
    Ok(screen(voice.fetch_model(id).await?))
}

#[tauri::command]
async fn fetch_voice_model(voice: VoiceState<'_>) -> Result<serde_json::Value, String> {
    Ok(screen(voice.fetch_voice().await?))
}

#[tauri::command]
async fn forget_speech_model(id: String, voice: VoiceState<'_>) -> Result<serde_json::Value, String> {
    Ok(screen(voice.forget_model(id).await?))
}

#[tauri::command]
async fn record_wake_take(voice: VoiceState<'_>) -> Result<serde_json::Value, String> {
    Ok(screen(voice.record_wake_take().await?))
}

#[tauri::command]
async fn clear_wake_word(voice: VoiceState<'_>) -> Result<serde_json::Value, String> {
    Ok(screen(voice.clear_wake_word().await?))
}

/// The talk button on a phone, held and let go: what the push-to-talk key is on a desktop.
#[tauri::command]
fn push_to_talk(down: bool, voice: VoiceState<'_>) {
    voice.push(if down { zyris_voice::Push::Pressed } else { zyris_voice::Push::Released });
}
