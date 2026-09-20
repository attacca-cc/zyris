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

use std::sync::atomic::AtomicU64;

use crate::apm::Apm;
use crate::capture::{
    APM_FRAME, Capture, Captured, Choice, Chunker, VAD_FRAME, read_delay,
};
use crate::playback::{Playback, Render, Speaker};
use crate::session::{Session, Speaking, Stopped};
use crate::turn::Feed;
use crate::vad::{Endpointer, Listening};
use crate::view::{
    DeviceList, ListeningState, ModelView, SpeakingState, VoiceView, WakeState, WakeView, show,
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
    /// Which Attacca session this machine talks to and listens to.
    ///
    /// **`None` is a machine that can hear and cannot answer**, and that is where task 4 leaves
    /// it: there is no screen for choosing a session yet, so the id is put here by hand or it is
    /// absent. Absent is not a failure — everything about listening works, `Heard` still
    /// reaches whatever is watching the event stream, and the only thing missing is the half
    /// that speaks. Task 6 owns the screen that fills it in.
    pub session: Option<String>,
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
    /// The diagnostic stream. Built here rather than handed in: nothing outside needs to send
    /// on it, and `zyris-app` only ever subscribes.
    traces: broadcast::Sender<crate::Trace>,
    /// Where `zyris-app` puts the push-to-talk key. Held by the engine rather than handed to the
    /// session, so that a session started later still gets every press after it started.
    keys: broadcast::Sender<Push>,
    /// The live turn feed, or `None` on a machine whose settings name no session.
    ///
    /// **Built once, at construction, and never rebuilt** — the same rule `Feed` itself states:
    /// there is no screen for choosing a session, so the id is what it was when this started.
    /// It outlives every connection, which is the point: the subscription belongs to the
    /// connection and the cursor belongs to the feed.
    feed: Option<Arc<Feed>>,
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
    /// The same, for the speaker's thread. `None` on a machine that opened no speaker.
    speaker_stop: Option<std::sync::mpsc::Sender<()>>,
    /// The synthesis worker, the render pump and the delay watch. Aborted together with the
    /// session: each of them holds a handle on the `Apm` this run built, and a pump left running
    /// over the next run's processor would be feeding one canceller from another one's speaker.
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Engine {
    /// Read the stored settings and build an engine that is not listening yet.
    ///
    /// Never fails, and never opens anything. A settings file that cannot be read is logged and
    /// treated as the default — which is **off**, so the failure mode is a switch a person has
    /// to move again rather than a microphone that opens for a reason nobody can see.
    pub fn new(dir: Option<&Path>, events: broadcast::Sender<VoiceEvent>) -> Engine {
        // Deeper than the product stream: a turn produces one or two `VoiceEvent`s and a dozen
        // steps, and a window that fell behind would lose the middle of the pipeline, which is
        // the part somebody is watching for.
        let settings_path = dir.map(|dir| dir.join(SETTINGS_FILE));
        let settings = settings_path.as_deref().map(read_settings).unwrap_or_default();

        let feed = settings.session.as_deref().map(Feed::new);
        Engine {
            settings_path,
            events,
            traces: broadcast::channel(TRACE_CAPACITY).0,
            keys: broadcast::channel(KEY_CAPACITY).0,
            feed,
            live: Mutex::new(Live { settings, state: ListeningState::Off, running: None }),
        }
    }

    /// A connection came up: subscribe to the turn on it.
    ///
    /// **Whether or not anything is listening.** The subscription belongs to the connection, not
    /// to the microphone switch: `turn_events` with `after: None` replays nothing, so a feed that
    /// waited for somebody to turn listening on would miss every delta written before they did.
    pub async fn on_connect(&self, connection: zyris::Connection) {
        match &self.feed {
            Some(feed) => feed.on_connect(connection).await,
            None => tracing::info!(
                "no Attacca session is configured for the voice, so nothing is read aloud; set \
                 `session` in {SETTINGS_FILE}"
            ),
        }
    }

    /// A new subscription to what the session says.
    pub fn events(&self) -> broadcast::Receiver<VoiceEvent> {
        self.events.subscribe()
    }

    /// A new subscription to the diagnostic stream.
    pub fn traces(&self) -> broadcast::Receiver<crate::Trace> {
        self.traces.subscribe()
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
            speaking: match live.settings.session.clone() {
                Some(id) => SpeakingState::Session { id },
                // Not a failure, and not silence without a reason: the file is named so a
                // person can put an id in it, and there is no control here that would.
                None => SpeakingState::NoSession {
                    settings: match &self.settings_path {
                        Some(path) => show(path),
                        None => SETTINGS_FILE.to_string(),
                    },
                },
            },
            wake: wake_view(),
            voice_model: voice_model_view(crate::tts::state()),
            voice_model_env: std::env::var(crate::tts::MODELS_ENV).ok().filter(|n| !n.is_empty()),
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
        let dir = stt::cache_dir()
            .ok_or_else(|| crate::model::Fault::NoCacheDirectory.to_string())?;
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

    /// Download every file of the voice that is not already there.
    ///
    /// **Nothing else in this program fetches these**, which is what made a machine with a
    /// session named and no voice on disk say it was reading answers aloud: the only way to the
    /// 401 MB was an operator unpacking the archive by hand.
    ///
    /// It does not touch the listening state the way [`Voice::fetch_model`] does. Whisper gates
    /// the microphone, so a download that finishes is a reason to start; the voice gates only
    /// what is read back, and the speaking half is opened by the connection rather than by this.
    pub async fn fetch_voice(&self) -> Result<(), String> {
        let dir = match crate::tts::models_dir() {
            Some(dir) => dir,
            None => return Err(crate::model::Fault::NoCacheDirectory.to_string()),
        };
        // Refused rather than fetched into: `ZYRIS_TTS_MODELS` names somebody's own directory,
        // and writing 401 MB into it is this program overruling their choice. The screen offers
        // no button in that state either; this is the half that cannot be clicked around.
        if std::env::var_os(crate::tts::MODELS_ENV).is_some_and(|n| !n.is_empty()) {
            return Err(format!(
                "{} names the directory the voice is read from, so Zyris does not download into it",
                crate::tts::MODELS_ENV
            ));
        }
        std::fs::create_dir_all(&dir).map_err(|error| {
            format!("{} could not be created: {error}", dir.display())
        })?;
        crate::tts::fetch_missing(&dir, |_| {}).await.map_err(|fault| fault.to_string())
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

        let dir = stt::cache_dir()
            .ok_or_else(|| crate::model::Fault::NoCacheDirectory.to_string())?;
        let model = dir.join(stt::BASE.file);
        match std::fs::remove_file(&model) {
            Ok(()) => {}
            // Nothing to delete is not a failure: it is what the screen already says is there.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(crate::model::Fault::Storage { path: model, detail: error.to_string() }
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

        let apm = Arc::new(apm);
        let (device, audio, capture_delay, stop) = open_on_a_thread(choice.clone()).await?;

        // **A machine that can hear and not speak is a usable machine**, so nothing below is
        // allowed to refuse the microphone. No speaker, no voice models, no session configured:
        // each of them is a run that listens, transcribes and publishes exactly as before, and
        // says why it is not talking in the log rather than by failing to start.
        let mut tasks = Vec::new();
        let (speaking, speaker_stop) = match self.open_speaker(apm.clone(), &capture_delay).await {
            Ok((speaking, stop, started)) => {
                tasks = started;
                (Some(speaking), Some(stop))
            }
            Err(reason) => {
                tracing::info!(%reason, "nothing will be read aloud on this run");
                (None, None)
            }
        };

        let mut session = Session::new(
            audio,
            self.keys.subscribe(),
            apm,
            Arc::new(stt),
            self.events.clone(),
        );
        if let Some(speaking) = speaking {
            session = session.speaking(speaking);
        }
        // **Not `if let Some(speaking)` above.** Sending what was heard needs the feed and
        // nothing else, so a machine whose speaker would not open, or whose voice has not been
        // downloaded, still talks to the agent — it just does not hear the answer back.
        if let Some(feed) = &self.feed {
            session = session.conversation(feed.clone());
        }
        session = session.tracing(self.traces.clone());
        Ok((
            Running { stop, session: tokio::spawn(session.run()), speaker_stop, tasks },
            device,
        ))
    }

    /// Open the speaker, load the voice, and start the three things that keep it fed.
    ///
    /// The `Err` is a sentence for the log, not for a person: every reason it can fail is
    /// something a screen already shows or task 6 will.
    async fn open_speaker(
        &self,
        apm: Arc<Apm>,
        capture_delay: &Arc<AtomicU64>,
    ) -> Result<
        (Arc<Speaking>, std::sync::mpsc::Sender<()>, Vec<tokio::task::JoinHandle<()>>),
        String,
    > {
        let feed = self
            .feed
            .clone()
            .ok_or_else(|| format!("no Attacca session is named in {SETTINGS_FILE}"))?;
        let dir = match crate::tts::state() {
            crate::tts::VoiceState::Ready { dir } => dir,
            crate::tts::VoiceState::Incomplete { bytes, .. } => {
                return Err(format!("the voice has not been downloaded yet ({bytes} bytes)"));
            }
            crate::tts::VoiceState::Unreadable { detail, .. } => return Err(detail),
            crate::tts::VoiceState::Nowhere { reason } => return Err(reason),
        };
        let voice = crate::tts::DEFAULT_VOICE;
        let tts = tokio::task::spawn_blocking(move || crate::tts::Tts::load(&dir, voice))
            .await
            .map_err(|_| "loading the voice stopped before it finished".to_string())?
            .map_err(|fault| fault.to_string())?;

        let (speaker, tap, rate, stop) = open_speaker_on_a_thread().await?;

        let mut tasks = Vec::new();
        // The render side of the echo canceller. Started before anything can be queued, so that
        // the first thing ever played is also the first thing the canceller is told about.
        let render = Render::new(apm.clone(), rate).map_err(|problem| problem.reason)?;
        tasks.push(tokio::spawn(render.run(tap)));
        tasks.push(tokio::spawn(declare_stream_delay(
            apm,
            capture_delay.clone(),
            speaker.clone(),
        )));

        let speaking = Speaking::new(
            Arc::new(std::sync::Mutex::new(tts)),
            Arc::new(speaker),
            feed.clone(),
            self.events.clone(),
        );
        let speaking = speaking.tracing(self.traces.clone());
        tasks.push(tokio::spawn(speaking.clone().run(feed.events())));
        Ok((speaking, stop, tasks))
    }

    /// Stop whatever is listening. Safe to call when nothing is.
    async fn halt(&self, live: &mut Live) {
        if let Some(running) = live.running.take() {
            // Dropping the sender would do it too; sending says which of the two happened to
            // anybody reading the thread.
            let _ = running.stop.send(());
            if let Some(stop) = &running.speaker_stop {
                let _ = stop.send(());
            }
            for task in &running.tasks {
                task.abort();
            }
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

/// How many diagnostic steps are held for a window that is not reading fast enough.
///
/// Deeper than the key or the product stream: a single turn is a dozen steps and an answer of
/// ten sentences is fifty, and the middle of the pipeline is exactly what somebody watching it
/// is looking for. A lag here costs nothing but a gap in a log.
const TRACE_CAPACITY: usize = 512;

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
    (
        String,
        tokio::sync::mpsc::UnboundedReceiver<Captured>,
        Arc<AtomicU64>,
        std::sync::mpsc::Sender<()>,
    ),
    String,
> {
    let (ready, opened) = tokio::sync::oneshot::channel();
    let (stop, told_to_stop) = std::sync::mpsc::channel::<()>();

    std::thread::Builder::new()
        .name("zyris-microphone".to_string())
        .spawn(move || match Capture::open(&choice, APM_FRAME) {
            Ok((capture, audio)) => {
                let device = capture.device().to_string();
                // The `Capture` cannot leave this thread, so what leaves is the one number
                // anything outside wants from it: `callback - capture`, for the echo canceller.
                if ready.send(Ok((device, audio, capture.delay_slot()))).is_err() {
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

    let (device, audio, delay) = opened
        .await
        .map_err(|_| "the microphone thread stopped before it opened anything".to_string())??;
    Ok((device, audio, delay, stop))
}

/// Hold a `cpal::Stream` for the speaker on a thread of its own and hand the queue back.
///
/// The same shape [`open_on_a_thread`] has, and for the same reason: `cpal::Stream` is not
/// `Send` on every platform, so a `Playback` cannot be held by anything that is. What crosses
/// the thread boundary is a [`Speaker`] — channels and atomics — the render tap, the rate the
/// stream was actually opened at, and the handle that closes it.
async fn open_speaker_on_a_thread() -> Result<
    (Speaker, tokio::sync::mpsc::UnboundedReceiver<Vec<f32>>, u32, std::sync::mpsc::Sender<()>),
    String,
> {
    let (ready, opened) = tokio::sync::oneshot::channel();
    let (stop, told_to_stop) = std::sync::mpsc::channel::<()>();

    std::thread::Builder::new()
        .name("zyris-speaker".to_string())
        .spawn(move || match Playback::open(&Choice::Default) {
            Ok((playback, tap)) => {
                let rate = playback.config().sample_rate;
                if ready.send(Ok((playback.handle(), tap, rate))).is_err() {
                    return;
                }
                let _ = told_to_stop.recv();
            }
            Err(problem) => {
                let _ = ready.send(Err(problem.reason));
            }
        })
        .map_err(|error| format!("a thread for the speaker could not be started: {error}"))?;

    let (speaker, tap, rate) = opened
        .await
        .map_err(|_| "the speaker thread stopped before it opened anything".to_string())??;
    Ok((speaker, tap, rate, stop))
}

/// How often the delay watch looks for both streams having reported a timestamp.
const DELAY_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// How long it keeps looking. Ten seconds: a stream that has delivered no callback by then is
/// one nobody is listening to or speaking through.
const DELAY_TRIES: usize = 40;

/// Tell the echo canceller the round trip, once both streams have said what their halves are.
///
/// **Neither half is knowable at construction** — a timestamp exists only once a callback has
/// run — and the two streams open at different times, so this waits for both rather than
/// declaring half a delay. While it is waiting, AEC3 is estimating the delay itself, which is
/// what step 7 shipped and is a working state rather than a broken one.
async fn declare_stream_delay(apm: Arc<Apm>, capture: Arc<AtomicU64>, speaker: Speaker) {
    for _ in 0..DELAY_TRIES {
        if let (Some(capture), Some(playback)) = (read_delay(&capture), speaker.stream_delay()) {
            let round_trip = capture + playback;
            apm.set_stream_delay(Some(round_trip));
            tracing::info!(
                ?capture,
                ?playback,
                ?round_trip,
                "the echo canceller was told what the loudspeaker path costs"
            );
            return;
        }
        tokio::time::sleep(DELAY_POLL).await;
    }
    tracing::info!(
        "neither audio stream reported a timestamp, so the echo canceller goes on estimating the \
         delay between the speaker and the microphone itself"
    );
}

/// Record one wake word take, on the thread this is called on.
///
/// Blocking from end to end: it owns a `cpal::Stream`, which cannot cross an `await`.
/// How long one take may wait for the microphone to say anything.
///
/// **A device can open and then deliver nothing**, and cpal's `null` is the documented one that
/// does — `capture.rs` records that it reports itself as a working input. Without a bound the
/// recording waits on it forever, on a `spawn_blocking` thread nothing can cancel, holding the
/// microphone open with the Voice tab's button turning. Three seconds because a microphone that
/// has said nothing in three is not about to.
const SILENT_DEVICE: std::time::Duration = std::time::Duration::from_secs(3);

/// How long one take may run in total, however steadily the device delivers.
///
/// [`SILENT_DEVICE`] bounds a gap and this bounds the sum, which is a different failure: a device
/// that answers every two seconds with one sample never trips a gap and never fills a take
/// either. Four times [`wake::MAX_TAKE`] is slack for a machine under load rather than a
/// tolerance anybody should reach.
const TAKE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(wake::MAX_TAKE.as_secs() * 4);

fn record_one(choice: &Choice) -> Result<(Vec<f32>, crate::apm::Conditioning), String> {
    let apm = Apm::new().map_err(|fault| fault.to_string())?;
    let (_capture, audio) = Capture::open(choice, APM_FRAME).map_err(|problem| problem.reason)?;
    record_from(audio, apm)
}

/// The recording itself, with the device it came from left outside.
///
/// Separated so a test can hand it a channel nothing ever sends on, which is the whole of what
/// [`SILENT_DEVICE`] is for and is unreachable while this function opens its own `Capture`.
fn record_from(
    mut audio: tokio::sync::mpsc::UnboundedReceiver<Captured>,
    apm: Apm,
) -> Result<(Vec<f32>, crate::apm::Conditioning), String> {
    let mut to_apm = Chunker::new(APM_FRAME);
    let mut to_vad = Chunker::new(VAD_FRAME);
    let mut endpointer = Endpointer::new();
    let mut kept: Vec<f32> = Vec::new();
    // Not `samples_in(MAX_TAKE)`: this loop can only stop on a frame boundary, and that figure
    // is not one. See `wake::longest_take`.
    let longest = wake::longest_take(VAD_FRAME);

    // `Handle::block_on` is legal here and only here: `record_one` is called from
    // `spawn_blocking`, which is not a runtime worker thread. `blocking_recv` has no deadline
    // of its own, which is the bug this replaces.
    let runtime = tokio::runtime::Handle::current();
    let started = std::time::Instant::now();

    loop {
        if started.elapsed() >= TAKE_DEADLINE {
            return Err("the microphone delivered too slowly to finish a recording".to_string());
        }
        // The timeout is built *inside* `block_on`: `tokio::time::timeout` constructs its
        // `Sleep` eagerly, and a blocking thread has a handle but no runtime context to build
        // one in. Built outside, it panics with "there is no reactor running".
        let waited =
            runtime.block_on(async { tokio::time::timeout(SILENT_DEVICE, audio.recv()).await });
        let captured = match waited {
            Ok(Some(captured)) => captured,
            Ok(None) => break,
            Err(_elapsed) => {
                return Err(
                    "the microphone opened and then delivered nothing; if this is the right \
                     device, something else may be holding it"
                        .to_string(),
                );
            }
        };
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

/// What is on disk where the voice should be, as the screen renders it.
///
/// Four answers in and four out, plus the arm no `voice` build can produce. The arm that has to
/// survive is `Incomplete`: a download button in front of somebody a download cannot help is the
/// confident false negative `tts::VoiceState` was given four arms to prevent, and folding it
/// into `Unreadable` here would put it back.
fn voice_model_view(state: crate::tts::VoiceState) -> crate::view::VoiceModelView {
    use crate::tts::VoiceState;
    match state {
        VoiceState::Ready { dir } => crate::view::VoiceModelView::Ready { dir: show(&dir) },
        // Not `tts::total_bytes()`: what is left to fetch, which after fifteen of sixteen files
        // is a few megabytes and not four hundred.
        VoiceState::Incomplete { dir, missing, bytes } => {
            crate::view::VoiceModelView::Incomplete { dir: show(&dir), missing: missing.len(), bytes }
        }
        VoiceState::Unreadable { dir, detail } => {
            crate::view::VoiceModelView::Unreadable { dir: show(&dir), reason: detail }
        }
        VoiceState::Nowhere { reason } => crate::view::VoiceModelView::Nowhere { reason },
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

    pub(super) fn tempdir() -> PathBuf {
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
        assert_eq!(
            Settings::default(),
            Settings { listen: false, device: Choice::Default, session: None }
        );

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

    /// **The voice is a separate card from the session, because the two fail apart.** Before
    /// this, `SpeakingState` was the only thing the screen read about speech: a machine with a
    /// session named and not one byte of the voice on disk rendered as *answers from this
    /// session are read aloud as they arrive*, which is precisely what was not happening. The
    /// two answers here are independent, so both are asserted from one view.
    #[test]
    fn a_named_session_does_not_say_the_voice_is_there() {
        let view = crate::view::VoiceView {
            speaking: SpeakingState::Session { id: "s-1".into() },
            voice_model: voice_model_view(crate::tts::VoiceState::Incomplete {
                dir: "/c/supertonic-3".into(),
                missing: vec!["vocoder.onnx".into(), "tts.json".into()],
                bytes: 300,
            }),
            ..crate::view::VoiceView::unavailable("stand-in".into())
        };

        assert_eq!(view.speaking, SpeakingState::Session { id: "s-1".into() });
        assert_eq!(
            view.voice_model,
            crate::view::VoiceModelView::Incomplete {
                dir: "/c/supertonic-3".into(),
                missing: 2,
                bytes: 300,
            }
        );
    }

    /// What is left to fetch, not what the whole snapshot costs. Fifteen of sixteen files down
    /// and the screen must not ask for 401 MB again — `tts::total_bytes()` in that position
    /// would be a number that is wrong exactly when somebody is looking at it.
    #[test]
    fn an_unfinished_download_asks_for_what_is_left_and_not_for_all_of_it() {
        let view = voice_model_view(crate::tts::VoiceState::Incomplete {
            dir: "/c/supertonic-3".into(),
            missing: vec!["tts.json".into()],
            bytes: 4_000,
        });

        assert_eq!(
            view,
            crate::view::VoiceModelView::Incomplete {
                dir: "/c/supertonic-3".into(),
                missing: 1,
                bytes: 4_000,
            }
        );
        assert!(4_000 < crate::tts::total_bytes(), "the stand-in has to be the smaller number");
    }

    /// Four states about the voice and each is a different sentence and a different set of
    /// buttons. The one that has to survive is `Incomplete`: folded into `Unreadable` it puts a
    /// Download button in front of somebody a download cannot help, which is the confident
    /// false negative `tts::VoiceState` was given four arms to prevent.
    #[test]
    fn the_four_answers_about_the_voice_stay_four() {
        let dir: PathBuf = "/c/supertonic-3".into();
        let views = [
            voice_model_view(crate::tts::VoiceState::Ready { dir: dir.clone() }),
            voice_model_view(crate::tts::VoiceState::Incomplete {
                dir: dir.clone(),
                missing: vec!["tts.json".into()],
                bytes: 4_000,
            }),
            voice_model_view(crate::tts::VoiceState::Unreadable {
                dir: dir.clone(),
                detail: "a directory is in the way".into(),
            }),
            voice_model_view(crate::tts::VoiceState::Nowhere {
                reason: "no cache directory".into(),
            }),
        ];

        for (at, view) in views.iter().enumerate() {
            for other in &views[at + 1..] {
                assert_ne!(view, other, "two states about the voice render the same");
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

#[cfg(test)]
mod recording_a_take {
    use super::*;

    /// A device that opens and never says anything is refused, rather than held forever.
    ///
    /// **This is why `record_from` exists.** While the recording opened its own `Capture` there
    /// was no way to hand it a device that delivers nothing — which is the failure being guarded
    /// against, and cpal's `null` is the documented one that does it: `capture.rs` records that
    /// it reports `supports_input() == true` and hands back a perfectly ordinary configuration.
    /// The bug it replaces had no deadline anywhere, so the take waited on a `spawn_blocking`
    /// thread nothing can cancel, holding the microphone, with the Voice tab's button turning.
    ///
    /// The channel is kept alive deliberately. Dropping the sender would end the recording
    /// through the ordinary "the stream closed" path and prove nothing about the deadline.
    #[test]
    fn a_device_that_delivers_nothing_is_given_up_on() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_time()
            .build()
            .expect("a runtime");

        let (_sender, audio) = tokio::sync::mpsc::unbounded_channel::<Captured>();
        let Ok(apm) = Apm::new() else {
            eprintln!("skipped: no audio processor on this machine");
            return;
        };

        let began = std::time::Instant::now();
        // Inside the `async` block, not as the argument: an argument is evaluated before
        // `block_on` enters the runtime, and `spawn_blocking` needs the context to exist.
        let outcome = runtime
            .block_on(async move { tokio::task::spawn_blocking(move || record_from(audio, apm)).await });
        let took = began.elapsed();

        let outcome = outcome.expect("the recording thread did not panic");
        let reason = outcome.expect_err("a device that says nothing cannot produce a take");
        assert!(reason.contains("delivered nothing"), "{reason}");

        // The deadline is what ended it, not something else that happened to be quicker or a
        // test that would sit here for a minute if the bound came back.
        assert!(took >= SILENT_DEVICE, "gave up after {took:?}, before the deadline");
        assert!(took < SILENT_DEVICE * 3, "took {took:?}, which is not a bounded wait");
    }
}

#[cfg(test)]
mod the_delay_watch {
    use super::*;

    /// **Half a delay is a confident wrong number**, and the whole point of the watch is that it
    /// does not declare one.
    ///
    /// The two streams open at different times — the microphone when somebody turns listening on,
    /// the speaker when there is an answer — and a timestamp exists only once a callback has run.
    /// A version that declared whichever half arrived first would hand AEC3 a path length that is
    /// wrong by the other half, which cancels less while going on reporting that it is working.
    #[tokio::test]
    async fn neither_half_of_the_delay_is_declared_on_its_own() {
        let apm = Arc::new(Apm::new().expect("a processor this machine can build"));
        let capture = Arc::new(AtomicU64::new(crate::capture::UNKNOWN_DELAY));
        let (speaker, mut fill, _tap) = crate::playback::offline(441);

        let watching = tokio::spawn(declare_stream_delay(apm.clone(), capture.clone(), speaker));

        // The speaker reports its half and the microphone has not opened yet.
        let at = cpal::StreamInstant::ZERO + std::time::Duration::from_millis(500);
        let mut out = vec![0.0f32; 64];
        fill.deliver(
            &mut out,
            1,
            Some(cpal::OutputStreamTimestamp {
                callback: at,
                playback: at + std::time::Duration::from_millis(43),
            }),
        );
        tokio::time::sleep(DELAY_POLL * 2).await;
        assert_eq!(apm.stream_delay(), None, "one half is not a round trip");

        // And now the microphone's.
        crate::capture::store_delay(&capture, std::time::Duration::from_millis(21));
        let waited = tokio::time::Instant::now();
        while apm.stream_delay().is_none() {
            assert!(
                waited.elapsed() < std::time::Duration::from_secs(10),
                "both halves are in and nothing was declared"
            );
            tokio::time::sleep(DELAY_POLL).await;
        }

        assert_eq!(
            apm.stream_delay(),
            Some(std::time::Duration::from_millis(64)),
            "the round trip is the sum, which is what `EchoCanceller::Full` asks for"
        );
        watching.await.expect("the watch ends once it has declared");
    }

    /// A watch that nothing ever reports to ends rather than polling for the life of the
    /// process. `DELAY_TRIES` bounds it; the assertion is that the bound exists and is sane.
    #[test]
    fn the_watch_gives_up_rather_than_polling_forever() {
        assert!(DELAY_TRIES > 0);
        assert!(
            DELAY_POLL * DELAY_TRIES as u32 >= std::time::Duration::from_secs(5),
            "a stream on a loaded machine may take seconds to deliver its first callback"
        );
        assert!(
            DELAY_POLL * DELAY_TRIES as u32 <= std::time::Duration::from_secs(60),
            "and one that has delivered none by then is a stream nobody is using"
        );
    }

    /// The setting a session id comes from, and the default that means a machine which listens
    /// and does not answer.
    #[test]
    fn a_machine_with_no_session_named_has_no_feed_to_read_aloud_from() {
        let (events, _) = broadcast::channel(4);
        let engine = Engine::new(None, events);

        assert!(
            engine.feed.is_none(),
            "there is no screen for choosing a session yet, so an absent one has to be an \
             ordinary state rather than a failure"
        );
    }

    /// And one that does name a session gets a feed for exactly that session.
    #[test]
    fn a_named_session_is_the_one_the_feed_listens_to() {
        let dir = tempdir();
        std::fs::create_dir_all(&dir).expect("a directory");
        std::fs::write(
            dir.join(SETTINGS_FILE),
            br#"{"listen":false,"device":{"kind":"default"},"session":"s-123"}"#,
        )
        .expect("written");
        let (events, _) = broadcast::channel(4);

        let engine = Engine::new(Some(&dir), events);

        assert_eq!(
            engine.feed.as_ref().map(|feed| feed.session_id().to_string()),
            Some("s-123".to_string())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::tests::tempdir;
}
