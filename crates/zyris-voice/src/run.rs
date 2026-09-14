//! The part that is actually listening, and the stored answer to "should it be?".
//!
//! # When voice starts, and why it is not at launch
//!
//! Nothing here opens a microphone until a person has said to. That decision is task 7's and it
//! is made against two bad alternatives:
//!
//! - **Starting at launch** would download 141 MB and open a microphone on a machine whose owner
//!   has not asked for either, on every install, including the ones that only ever wanted a
//!   shell and a file system. A microphone opening unasked is not a thing to do quietly.
//! - **Never starting on its own** would be a feature that silently does nothing — the shape
//!   this workspace keeps calling a control that cannot work.
//!
//! So the switch is asked for once and **remembered**: [`Settings`] is written into the
//! instance's data directory, and [`Engine::resume`] honours it at the next launch. The first
//! time is explicit and informed — the screen says how big the download is and that the
//! microphone will be open — and every time after that it is what the person already chose.
//! Turning it on is therefore not "turn on for now"; the Voice screen says which it is, which is
//! the distinction the MCP screen's switch had to make in the other direction.
//!
//! `--headless` never calls [`Engine::resume`]: a run with no window is a run with nobody at the
//! keyboard, and `zyris-app`'s `hotkey` module answers that with no hotkey at all. A stored
//! `listen: true` therefore listens in a window, minimized or not, and not in a headless node.
//!
//! # Why the microphone is on a thread of its own
//!
//! `capture::Capture` holds a `cpal::Stream`, which is not `Send` on every platform, so it
//! cannot be held across an `await` or moved onto a task. The thread here does one thing: open
//! the stream, hand the receiver back, and stay alive until told to stop — dropping the
//! `Capture` is what closes the device. The session runs on the tokio runtime and reads that
//! receiver, exactly as `session`'s own documentation says it is meant to be driven.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{Mutex, broadcast};

use crate::apm::Apm;
use crate::capture::{APM_FRAME, Capture, Captured, Choice, Chunker, VAD_FRAME};
use crate::session::{Session, Stopped};
use crate::vad::{Endpointer, Listening};
use crate::view::{
    DeviceList, ListeningState, ModelView, VoiceView, WakeState, WakeView, show,
};
use crate::{Push, VoiceEvent, VoiceSupport, stt, wake};

/// What the file in the data directory holds.
///
/// Two fields, both of them answers a person gave: whether to listen, and what to listen with.
/// Nothing measured lives here — the model, the devices and the wake word are all read off the
/// machine on every look, because every one of them can change while this program is not running.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Whether a microphone should be open. Defaults to **off**: see the module.
    pub listen: bool,
    /// Which microphone.
    pub device: Choice,
}

/// What the settings file is called, inside the instance's data directory.
///
/// The **data** directory and not the cache, and scoped by instance like everything else `main`
/// derives from the instance name: a `--server` run choosing to listen must not turn the
/// microphone on for the production node.
pub const SETTINGS_FILE: &str = "voice.json";

/// The microphone, the session over it, and the settings that say whether there should be one.
pub struct Engine {
    settings_path: Option<PathBuf>,
    events: broadcast::Sender<VoiceEvent>,
    /// Where `zyris-app` puts the push-to-talk key. Held by the engine rather than handed to the
    /// session, so that a session started later still gets every press after it started.
    keys: broadcast::Sender<Push>,
    live: Mutex<Live>,
}

struct Live {
    settings: Settings,
    state: ListeningState,
    running: Option<Running>,
}

struct Running {
    /// Telling the capture thread to let the device go. A `std` channel because the thread that
    /// waits on it is a plain thread and not a task.
    stop: std::sync::mpsc::Sender<()>,
    session: tokio::task::JoinHandle<Stopped>,
}

impl Engine {
    /// Read the stored settings and build an engine that is not listening yet.
    ///
    /// Never fails, and never opens anything. A settings file that cannot be read is logged and
    /// treated as the default — which is **off**, so the failure mode is a switch a person has
    /// to move again rather than a microphone that opens for a reason nobody can see.
    pub fn new(dir: Option<&Path>, events: broadcast::Sender<VoiceEvent>) -> Engine {
        let settings_path = dir.map(|dir| dir.join(SETTINGS_FILE));
        let settings = settings_path.as_deref().map(read_settings).unwrap_or_default();

        Engine {
            settings_path,
            events,
            keys: broadcast::channel(KEY_CAPACITY).0,
            live: Mutex::new(Live { settings, state: ListeningState::Off, running: None }),
        }
    }

    /// A new subscription to what the session says.
    pub fn events(&self) -> broadcast::Receiver<VoiceEvent> {
        self.events.subscribe()
    }

    /// The push-to-talk key went down or came up.
    ///
    /// Accepted whether or not anything is listening: `broadcast::send` fails only when nobody
    /// is subscribed, which is the ordinary state of a machine with the switch off. A key
    /// pressed then is not an error and not a reason to start.
    pub fn push(&self, push: Push) {
        let _ = self.keys.send(push);
    }

    /// Start listening if the stored settings say to. Called once, by the windowed branch.
    pub async fn resume(&self) {
        let wanted = self.live.lock().await.settings.listen;
        if wanted {
            self.set_listening(true).await;
        }
    }

    /// Everything the Voice screen reads off this machine, in one answer.
    ///
    /// Reads the disk every time — the model can be deleted and a microphone unplugged while
    /// this window is open, and a remembered answer would be a screen that is confidently wrong
    /// about both.
    pub async fn look(&self) -> VoiceView {
        let mut live = self.live.lock().await;
        self.settle(&mut live).await;

        VoiceView {
            support: crate::capture::support(),
            listening: live.state.clone(),
            devices: match crate::capture::devices() {
                Ok(devices) => DeviceList::Listed { devices },
                // Not an empty list. A sound server that will not answer and a computer with no
                // microphone are two different sentences, and the screen says both.
                Err(problem) => DeviceList::Unreadable { reason: problem.reason },
            },
            chosen: live.settings.device.clone(),
            model: model_view(stt::state(&stt::BASE)),
            model_env: std::env::var(stt::MODEL_ENV).ok().filter(|named| !named.is_empty()),
            wake: wake_view(),
        }
    }

    /// Turn listening on or off, and leave the state saying what that did.
    ///
    /// **What actually happened, not what was asked for** — the same rule
    /// `set_mcp_server_enabled` follows. Turning it on with no model on disk leaves
    /// [`ListeningState::Failed`] carrying the reason, and the stored setting is written anyway:
    /// a person who said "listen" and whose model has not been downloaded yet has still said
    /// "listen", and the next launch should try again rather than forgetting they asked.
    pub async fn set_listening(&self, on: bool) {
        let mut live = self.live.lock().await;
        live.settings.listen = on;
        self.store(&live.settings);

        if !on {
            self.halt(&mut live).await;
            live.state = ListeningState::Off;
            return;
        }

        if live.running.is_some() {
            self.settle(&mut live).await;
            if matches!(live.state, ListeningState::On { .. }) {
                return;
            }
        }
        self.halt(&mut live).await;

        live.state = match self.begin(&live.settings.device).await {
            Ok((running, device)) => {
                live.running = Some(running);
                ListeningState::On { device }
            }
            Err(reason) => ListeningState::Failed { reason },
        };
    }

    /// Choose a different microphone, and reopen it if one is already open.
    ///
    /// The choice is stored whether or not anything is listening: it is a setting, and a person
    /// picking their headset before turning the switch on is doing something reasonable.
    pub async fn choose(&self, device: Choice) {
        let mut live = self.live.lock().await;
        live.settings.device = device;
        self.store(&live.settings);

        if live.running.is_none() && !matches!(live.state, ListeningState::Failed { .. }) {
            return;
        }
        self.halt(&mut live).await;
        live.state = match self.begin(&live.settings.device).await {
            Ok((running, device)) => {
                live.running = Some(running);
                ListeningState::On { device }
            }
            Err(reason) => ListeningState::Failed { reason },
        };
    }

    /// Download the speech model, and start listening afterwards if that is what was asked for.
    ///
    /// Blocking for as long as 141 MB takes, which is why the screen shows what it is doing
    /// rather than a button that appears to do nothing.
    pub async fn fetch_model(&self) -> Result<(), String> {
        let dir = stt::cache_dir().ok_or_else(|| stt::Fault::NoCacheDirectory.to_string())?;
        {
            let mut live = self.live.lock().await;
            live.state = ListeningState::Starting {
                detail: "the speech model is being downloaded".to_string(),
            };
        }

        let fetched = stt::fetch(&stt::BASE, &dir, |_| {}).await.map_err(|f| f.to_string());

        let wanted = self.live.lock().await.settings.listen;
        match (&fetched, wanted) {
            // It was already asked for; now there is something to start.
            (Ok(_), true) => self.set_listening(true).await,
            (Ok(_), false) => self.live.lock().await.state = ListeningState::Off,
            (Err(reason), _) => {
                self.live.lock().await.state = ListeningState::Failed { reason: reason.clone() };
            }
        }
        fetched.map(|_| ())
    }

    /// Record one wake word take from the chosen microphone and keep it.
    ///
    /// **Its own short capture, not the listening one.** The live stream belongs to the session
    /// and its audio goes to whisper; a take has to be kept as audio. The recording ends when the
    /// endpointer says the utterance is over or at [`wake::MAX_TAKE`], whichever comes first, so
    /// a person says the phrase and stops rather than watching a timer.
    ///
    /// The audio is conditioned exactly as the live pipeline conditions it, and which build
    /// conditioned it is written into the manifest — see [`wake`].
    pub async fn record_wake_take(&self) -> Result<(), String> {
        let store = wake::Store::on_this_machine().map_err(|f| f.to_string())?;
        // Refused here rather than after five seconds of somebody's time: `Store::add` would say
        // the same thing, having already recorded them.
        if let wake::Enrolment::Complete { .. } = store.state() {
            return Err(wake::Fault::Enough.to_string());
        }

        let choice = self.live.lock().await.settings.device.clone();
        let (samples, conditioning) = tokio::task::spawn_blocking(move || record_one(&choice))
            .await
            .map_err(|_| "the recording stopped before it produced anything".to_string())??;

        let take = wake::Take::recorded(samples, conditioning).map_err(|f| f.to_string())?;
        store.add(&take).map_err(|f| f.to_string())?;
        Ok(())
    }

    /// Delete the downloaded speech model, and the wreckage of any download that was killed.
    ///
    /// **It turns listening off first**, and not only because the running session holds the
    /// model open: leaving the switch on over a model that is no longer there would be a screen
    /// showing "listening" for something that cannot transcribe a word.
    ///
    /// **It refuses to delete a file `ZYRIS_WHISPER_MODEL` names.** That file is an operator's
    /// own choice of model, kept wherever they keep it; this button is for the cache Zyris
    /// filled, and deleting somebody else's file because it happens to be in use here is not
    /// this program's to do.
    pub async fn forget_model(&self) -> Result<(), String> {
        if std::env::var_os(stt::MODEL_ENV).is_some_and(|named| !named.is_empty()) {
            return Err(format!(
                "the speech model in use was named by the {} environment variable, so Zyris will \
                 not delete it. Remove that setting to go back to the model Zyris downloads.",
                stt::MODEL_ENV
            ));
        }
        self.set_listening(false).await;

        let dir = stt::cache_dir().ok_or_else(|| stt::Fault::NoCacheDirectory.to_string())?;
        let model = dir.join(stt::BASE.file);
        match std::fs::remove_file(&model) {
            Ok(()) => {}
            // Nothing to delete is not a failure: it is what the screen already says is there.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(stt::Fault::Storage { path: model, detail: error.to_string() }
                    .to_string());
            }
        }

        // And the wreckage: `fetch` names a partial download `<file>.part-<pid>-<nanos>` and
        // never sweeps one, because the cache is shared and the file it swept might be another
        // instance's download in flight. A person pressing this button is the one case where
        // sweeping is asked for rather than guessed at.
        let prefix = format!("{}{}", stt::BASE.file, stt::PART_SUFFIX);
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with(&prefix) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        Ok(())
    }

    /// Forget every take of the wake word.
    pub async fn clear_wake_word(&self) -> Result<(), String> {
        let store = wake::Store::on_this_machine().map_err(|f| f.to_string())?;
        store.clear().map_err(|f| f.to_string())
    }

    /// Whether speech can work on this machine at all.
    pub fn support(&self) -> VoiceSupport {
        crate::capture::support()
    }

    // ------------------------------------------------------------------------------------

    /// Open a microphone and put a session over it.
    async fn begin(&self, choice: &Choice) -> Result<(Running, String), String> {
        let path = match stt::state(&stt::BASE) {
            stt::ModelState::Ready { path, .. } => path,
            // Everything else is the screen's business: it renders the same `ModelState` and has
            // a button for the one case a button fixes. The sentence here is about listening.
            other => return Err(no_model(&other)),
        };

        let apm = Apm::new().map_err(|fault| fault.to_string())?;
        let stt = tokio::task::spawn_blocking(move || stt::Stt::load(&path))
            .await
            .map_err(|_| "loading the speech model stopped before it finished".to_string())?
            .map_err(|fault| fault.to_string())?;

        let (device, audio, stop) = open_on_a_thread(choice.clone()).await?;

        let session = Session::new(
            audio,
            self.keys.subscribe(),
            Arc::new(apm),
            Arc::new(stt),
            self.events.clone(),
        );
        Ok((Running { stop, session: tokio::spawn(session.run()) }, device))
    }

    /// Stop whatever is listening. Safe to call when nothing is.
    async fn halt(&self, live: &mut Live) {
        if let Some(running) = live.running.take() {
            // Dropping the sender would do it too; sending says which of the two happened to
            // anybody reading the thread.
            let _ = running.stop.send(());
            running.session.abort();
            let _ = running.session.await;
        }
    }

    /// Notice a session that ended on its own.
    ///
    /// The microphone going away ends `Session::run`, and without this the screen would go on
    /// saying "on" over a device that is not there — the same defect the MCP screen's re-read
    /// exists to prevent, one layer down.
    async fn settle(&self, live: &mut Live) {
        let finished = live.running.as_ref().is_some_and(|r| r.session.is_finished());
        if !finished {
            return;
        }
        let running = live.running.take().expect("just seen to be there");
        let stopped = running.session.await.ok();
        live.state = ListeningState::Failed {
            reason: match stopped {
                Some(Stopped::MicrophoneGone) => {
                    "the microphone stopped delivering audio, so nothing is listening. Choose a \
                     device and turn listening on again."
                        .to_string()
                }
                _ => "the voice session ended, so nothing is listening. Turn listening on again."
                    .to_string(),
            },
        };
    }

    fn store(&self, settings: &Settings) {
        let Some(path) = &self.settings_path else { return };
        if let Err(error) = write_settings(path, settings) {
            tracing::warn!(%error, path = %path.display(), "could not store the voice settings");
        }
    }
}

/// How many key events a session may fall behind before it loses the oldest.
///
/// The same order of magnitude `zyris-app`'s `hotkey::EVENT_CAPACITY` uses and for the same
/// reason: a hold is two events and a person cannot produce many per second. `Session` treats a
/// lag as the end of a turn, so this being generous is what keeps that path out of ordinary use.
const KEY_CAPACITY: usize = 32;

/// Why listening cannot start, said in terms of the model.
fn no_model(state: &stt::ModelState) -> String {
    match state {
        stt::ModelState::Ready { .. } => unreachable!("the caller matched `Ready` already"),
        stt::ModelState::Absent { .. } => {
            "the speech model has not been downloaded yet, so there is nothing to transcribe with"
                .to_string()
        }
        stt::ModelState::Damaged { .. } => {
            "the speech model on this computer is not the right size, so it cannot be loaded; \
             download it again"
                .to_string()
        }
        stt::ModelState::Unreadable { path, detail } => format!(
            "the speech model could not be read at {}: {detail}",
            path.display()
        ),
        stt::ModelState::Nowhere { reason } => reason.clone(),
    }
}

/// Hold a `cpal::Stream` on a thread of its own and hand the audio back.
///
/// Returns the device's name, the receiver the session reads, and the handle that closes it.
async fn open_on_a_thread(
    choice: Choice,
) -> Result<
    (String, tokio::sync::mpsc::UnboundedReceiver<Captured>, std::sync::mpsc::Sender<()>),
    String,
> {
    let (ready, opened) = tokio::sync::oneshot::channel();
    let (stop, told_to_stop) = std::sync::mpsc::channel::<()>();

    std::thread::Builder::new()
        .name("zyris-microphone".to_string())
        .spawn(move || match Capture::open(&choice, APM_FRAME) {
            Ok((capture, audio)) => {
                let device = capture.device().to_string();
                if ready.send(Ok((device, audio))).is_err() {
                    return;
                }
                // Blocks until `stop` is sent or dropped. The `Capture` is dropped on the way
                // out of this closure, which is what stops and closes the stream.
                let _ = told_to_stop.recv();
            }
            Err(problem) => {
                let _ = ready.send(Err(problem.reason));
            }
        })
        .map_err(|error| format!("a thread for the microphone could not be started: {error}"))?;

    let (device, audio) = opened
        .await
        .map_err(|_| "the microphone thread stopped before it opened anything".to_string())??;
    Ok((device, audio, stop))
}

/// Record one wake word take, on the thread this is called on.
///
/// Blocking from end to end: it owns a `cpal::Stream`, which cannot cross an `await`.
fn record_one(choice: &Choice) -> Result<(Vec<f32>, crate::apm::Conditioning), String> {
    let apm = Apm::new().map_err(|fault| fault.to_string())?;
    let (_capture, mut audio) =
        Capture::open(choice, APM_FRAME).map_err(|problem| problem.reason)?;

    let mut to_apm = Chunker::new(APM_FRAME);
    let mut to_vad = Chunker::new(VAD_FRAME);
    let mut endpointer = Endpointer::new();
    let mut kept: Vec<f32> = Vec::new();
    let longest = stt::samples_in(wake::MAX_TAKE);

    while let Some(captured) = audio.blocking_recv() {
        let samples = match captured {
            Captured::Audio(samples) => samples,
            // A device that rerouted itself keeps going; anything else has ended the recording.
            Captured::Problem(problem) => {
                if problem.recovery == crate::capture::Recovery::Continue {
                    continue;
                }
                return Err(problem.reason);
            }
        };

        let mut staged: Vec<f32> = to_apm.push(&samples).flatten().copied().collect();
        for frame in staged.chunks_mut(to_apm.frames()) {
            apm.process_capture(frame).map_err(|fault| fault.to_string())?;
        }

        let ready: Vec<f32> = to_vad.push(&staged).flatten().copied().collect();
        staged.clear();
        for frame in ready.chunks(to_vad.frames()) {
            if frame.len() != to_vad.frames() {
                break;
            }
            kept.extend_from_slice(frame);
            // The endpointer is used to know *when to stop*, and its verdict is not applied to
            // what is kept: `wake` stores the recording untrimmed on purpose, and runs its own
            // copy of the rule over it to write down what today's rule thought.
            if matches!(endpointer.push(frame), Ok(Listening::Ended(_))) {
                return Ok((kept, apm.describe()));
            }
            if kept.len() >= longest {
                return Ok((kept, apm.describe()));
            }
        }
    }
    Err("the microphone stopped before anything was recorded".to_string())
}

/// What is on disk where the model should be, as the screen renders it.
///
/// The state is passed in rather than read here, so that a test can decide it: every arm is a
/// different sentence and a different set of buttons on the Voice screen, and on this machine
/// only one of the five is reachable at a time.
fn model_view(state: stt::ModelState) -> ModelView {
    match state {
        stt::ModelState::Ready { path, bytes } => ModelView::Ready { path: show(&path), bytes },
        // The download's size travels with the absence, so the screen can say what it is about
        // to ask for before anybody clicks.
        stt::ModelState::Absent { path } => {
            ModelView::Absent { path: show(&path), bytes: stt::BASE.bytes }
        }
        stt::ModelState::Damaged { path, bytes, expected } => {
            ModelView::Damaged { path: show(&path), bytes, expected }
        }
        stt::ModelState::Unreadable { path, detail } => {
            ModelView::Unreadable { path: show(&path), reason: detail }
        }
        stt::ModelState::Nowhere { reason } => ModelView::Nowhere { reason },
    }
}

/// How far along the wake word is, as the screen renders it.
fn wake_view() -> WakeView {
    match wake::Store::on_this_machine() {
        Ok(store) => wake_view_of(store.state(), Some(show(store.dir()))),
        // No data directory. **Not "nothing recorded"**: nothing can be.
        Err(fault) => wake_view_of(wake::Enrolment::Unreadable { reason: fault.to_string() }, None),
    }
}

/// The mapping on its own, so that a test can decide which enrolment it is looking at.
///
/// Four answers in and four out, and they are four different sentences on the screen: a store
/// that could not be read must not arrive there as one nobody has recorded into, because
/// somebody told that records five more takes over whatever is already in it.
fn wake_view_of(enrolment: wake::Enrolment, dir: Option<String>) -> WakeView {
    WakeView {
        state: match enrolment {
            wake::Enrolment::Nothing => WakeState::Nothing,
            wake::Enrolment::Partial { recorded, .. } => WakeState::Partial { recorded },
            wake::Enrolment::Complete { recorded } => WakeState::Complete { recorded },
            wake::Enrolment::Unreadable { reason } => WakeState::Unreadable { reason },
        },
        dir,
        wanted: wake::TAKES,
        seconds: wake::MAX_TAKE.as_secs(),
        // The constant, not a second copy of the sentence in TypeScript.
        note: wake::NOTHING_READS_THESE.to_string(),
    }
}

fn read_settings(path: &Path) -> Settings {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(settings) => settings,
            Err(error) => {
                tracing::warn!(
                    %error,
                    path = %path.display(),
                    "the voice settings could not be read; listening stays off",
                );
                Settings::default()
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Settings::default(),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "the voice settings could not be read");
            Settings::default()
        }
    }
}

/// Written through a uniquely named temporary file and one `rename`, the way `wake`'s manifest
/// and `stt`'s download are: a half-written settings file would be read as the default at the
/// next launch, which is a person's answer quietly forgotten.
fn write_settings(path: &Path, settings: &Settings) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(settings)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let part = path.with_extension(format!(
        "part-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::write(&part, &bytes)?;
    std::fs::rename(&part, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&part);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zyris-voice-run-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).expect("create a temporary directory");
        dir
    }

    /// **The default is off, and that is the product decision this task owns.** A test rather
    /// than a comment because `#[serde(default)]` on a `bool` is the kind of thing that flips
    /// when somebody reshapes the struct.
    #[test]
    fn nothing_listens_until_somebody_says_so() {
        assert_eq!(Settings::default(), Settings { listen: false, device: Choice::Default });

        let engine = Engine::new(None, broadcast::channel(4).0);
        let live = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(async { engine.live.lock().await.settings.clone() });

        assert!(!live.listen, "a machine nobody has asked opens no microphone");
    }

    /// The answer survives a restart, which is the other half of the decision: asking once means
    /// the file is read back.
    #[test]
    fn the_answer_is_remembered_across_a_restart() {
        let dir = tempdir();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let engine = Engine::new(Some(&dir), broadcast::channel(4).0);
            // Not `set_listening`, which would try to open a microphone: the subject here is the
            // file, and `choose` writes the same file through the same path.
            engine.choose(Choice::Device { id: "alsa_input.pci-0000_00_1f.3".into() }).await;
            engine.live.lock().await.settings.listen = true;
            let settings = engine.live.lock().await.settings.clone();
            engine.store(&settings);

            let again = Engine::new(Some(&dir), broadcast::channel(4).0);
            let read = again.live.lock().await.settings.clone();
            assert!(read.listen, "the person said listen; a restart must not forget it");
            assert_eq!(read.device, Choice::Device { id: "alsa_input.pci-0000_00_1f.3".into() });
        });

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A settings file with rubbish in it leaves the switch **off** rather than throwing an
    /// error at startup or opening a microphone on a guess.
    #[test]
    fn settings_that_cannot_be_read_leave_the_microphone_shut() {
        let dir = tempdir();
        std::fs::write(dir.join(SETTINGS_FILE), b"{ not json").expect("write");

        let engine = Engine::new(Some(&dir), broadcast::channel(4).0);
        let listen = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(async { engine.live.lock().await.settings.listen });

        assert!(!listen);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The four ways the model stops listening read as **four different sentences**.
    ///
    /// Distinctness is the property, not length: "there is no model", "the model on disk is the
    /// wrong size", "the model could not be read" and "there is nowhere to keep one" send a
    /// person to four different places, and a screen cannot recover a distinction the sentence
    /// behind it threw away.
    #[test]
    fn each_reason_the_model_stops_listening_is_its_own_sentence() {
        let mut said = Vec::new();
        for state in [
            stt::ModelState::Absent { path: "/x/ggml-base.bin".into() },
            stt::ModelState::Damaged {
                path: "/x/ggml-base.bin".into(),
                bytes: 3,
                expected: 147951465,
            },
            stt::ModelState::Unreadable {
                path: "/x/ggml-base.bin".into(),
                detail: "permission denied".into(),
            },
            stt::ModelState::Nowhere { reason: "no cache directory".into() },
        ] {
            let reason = no_model(&state);
            assert!(!reason.is_empty(), "{state:?} produced nothing to read");
            assert!(!said.contains(&reason), "{state:?} says what another state already said");
            said.push(reason);
        }
        assert_eq!(said.len(), 4);
    }

    /// **A missing model carries how big the download is**, so the screen can say what it is
    /// about to ask for before anybody agrees to it. The number is `stt::BASE`'s and not one
    /// typed into the window, which would be a second copy of it with nothing to keep the two
    /// in step.
    #[test]
    fn a_model_that_is_not_there_says_how_big_the_download_would_be() {
        let view = model_view(stt::ModelState::Absent { path: "/c/ggml-base.bin".into() });

        assert_eq!(
            view,
            ModelView::Absent { path: "/c/ggml-base.bin".into(), bytes: stt::BASE.bytes }
        );
    }

    /// Five states about one file, and each is a different sentence and a different set of
    /// buttons. Only one of them is reachable on any given machine, so this is the only place
    /// the other four are decided.
    #[test]
    fn the_five_answers_about_the_model_stay_five() {
        let path: PathBuf = "/c/ggml-base.bin".into();
        let views = [
            model_view(stt::ModelState::Ready { path: path.clone(), bytes: 10 }),
            model_view(stt::ModelState::Absent { path: path.clone() }),
            model_view(stt::ModelState::Damaged { path: path.clone(), bytes: 3, expected: 10 }),
            model_view(stt::ModelState::Unreadable {
                path: path.clone(),
                detail: "not a file".into(),
            }),
            model_view(stt::ModelState::Nowhere { reason: "no cache directory".into() }),
        ];

        for (at, view) in views.iter().enumerate() {
            for other in &views[at + 1..] {
                assert_ne!(view, other, "two states about the model render the same");
            }
        }
    }

    /// The wake word note the screen renders is `wake`'s constant and not a second wording of
    /// it. The constant has a test on each of its claims; a copy would have none.
    #[test]
    fn the_wake_word_note_is_the_constant_with_the_test_on_it() {
        assert_eq!(wake_view().note, wake::NOTHING_READS_THESE);
        assert_eq!(wake_view().wanted, wake::TAKES);
    }

    /// **The fourth separation this workspace has had to make, made once more here.** A store
    /// that could not be read must not reach the screen as one nobody has recorded into:
    /// somebody told they have recorded nothing records five more takes over whatever is there.
    #[test]
    fn a_wake_word_store_that_could_not_be_read_is_not_an_empty_one() {
        let unreadable = wake_view_of(
            wake::Enrolment::Unreadable { reason: "wake-word.json is not JSON".into() },
            None,
        );

        assert_eq!(
            unreadable.state,
            WakeState::Unreadable { reason: "wake-word.json is not JSON".into() }
        );
        assert_ne!(unreadable.state, WakeState::Nothing);
    }

    /// And the four of them stay four, counts included. `partial` losing its count would render
    /// as "0 of 5 takes recorded" beside four takes on disk.
    #[test]
    fn the_four_answers_about_the_wake_word_stay_four() {
        let views = [
            wake_view_of(wake::Enrolment::Nothing, None).state,
            wake_view_of(wake::Enrolment::Partial { recorded: 2, wanted: 5 }, None).state,
            wake_view_of(wake::Enrolment::Complete { recorded: 5 }, None).state,
            wake_view_of(wake::Enrolment::Unreadable { reason: "no".into() }, None).state,
        ];

        assert_eq!(views[1], WakeState::Partial { recorded: 2 });
        assert_eq!(views[2], WakeState::Complete { recorded: 5 });
        for (at, view) in views.iter().enumerate() {
            for other in &views[at + 1..] {
                assert_ne!(view, other, "two enrolments render the same");
            }
        }
    }

    /// A key pressed while nothing is listening is not an error and does not start anything.
    #[test]
    fn a_key_pressed_with_the_switch_off_starts_nothing() {
        let engine = Engine::new(None, broadcast::channel(4).0);

        engine.push(Push::Pressed);
        engine.push(Push::Released);

        let state = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(async { engine.live.lock().await.state.clone() });
        assert_eq!(state, ListeningState::Off);
    }
}
