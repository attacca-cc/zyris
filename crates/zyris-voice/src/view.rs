//! Everything the Voice screen renders, in **both** feature states.
//!
//! # Why this module is not behind the feature
//!
//! `zyris-app` may contain no `#[cfg(feature = "voice")]` — there is a test that reads its own
//! source to keep it so — and every other module in this crate that knows anything about a
//! microphone, a model file or a wake word is behind `voice`. So a window asking "what is on
//! this machine?" could not name the answer at all in the off build.
//!
//! The same accommodation [`crate::Push`] makes for the key coming in, made for the screen going
//! out: the types are declared here, where both builds can see them, and only the *assembly* of
//! a value reads the feature. [`crate::Voice::look`] is the one function with two arms.
//!
//! # Three answers, everywhere, and never an empty list for a failed read
//!
//! Every list and every state in here separates **could not read** from **read, and there is
//! nothing** from **not available on this build**. This workspace has now had to make that
//! separation five times — the audit tail, the inbox, the MCP server list, the wake word store
//! and the model on disk — and each time the version that shipped first was the one that
//! rendered a failed read as an empty list. A screen cannot recover a distinction its data
//! does not carry, so it is carried here.

use std::path::Path;

/// One thing a person could choose to listen with.
///
/// Declared here rather than in [`crate::capture`], where it is produced: a window lists these
/// and has to be able to name the type in a build with no `cpal` in it. `capture` re-exports it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputDevice {
    /// `cpal`'s own `DeviceId`, as a string. Stable across runs and reboots where the platform
    /// can manage it, which is why a choice is stored as this and not as a name — two devices
    /// on this machine are both called "Built-in Audio Analog Stereo".
    pub id: String,
    /// What a person reads.
    pub name: String,
    /// The host's default input. Exactly one entry has this, unless the host has no default.
    pub is_default: bool,
    pub direction: Direction,
}

/// Which end of a device this is.
///
/// Not a filter — see [`crate::capture`]. `Duplex` on PipeWire means a loudspeaker whose monitor
/// can also be captured, and `Unknown` is what ALSA says about its own `default`. Both are shown
/// to a person rather than hidden, because on this machine two of the four entries record the
/// loudspeakers and the two pairs are **spelled identically**: the direction is the only thing
/// on the screen that tells them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Direction {
    /// A microphone.
    Input,
    /// A loudspeaker. Capturing one records what the computer is playing.
    Output,
    /// Both ends of one device. On PipeWire this is how a sink's monitor appears.
    Duplex,
    /// The backend will not say without opening it.
    Unknown,
}

/// Which microphone to open.
///
/// [`Choice::Default`] is not a shorthand for "whatever is default right now": a stream built on
/// the default device *follows* the default when it changes, and one built on a named device
/// does not. See `capture::Capture::follows_default`.
///
/// `Deserialize` as well as `Serialize` because this is the one thing on the screen that is
/// **stored**: it is written into the settings file beside whether to listen at all.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Choice {
    /// Whatever the system calls the default input, now and after it changes.
    #[default]
    Default,
    /// This exact device, by [`InputDevice::id`].
    #[serde(rename_all = "camelCase")]
    Device { id: String },
}

/// What this machine could be listened to with.
///
/// **Three answers and only one of them is a list.** `Listed` with an empty vector is a computer
/// with no microphone, which reads differently from a sound server that would not answer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum DeviceList {
    /// What the host said. Empty means the host answered and listed nothing.
    #[serde(rename_all = "camelCase")]
    Listed { devices: Vec<InputDevice> },
    /// The host could not be asked. Not an empty list.
    #[serde(rename_all = "camelCase")]
    Unreadable { reason: String },
    /// This build has no audio stack, so there is nothing to enumerate with.
    #[serde(rename_all = "camelCase")]
    NotHere { reason: String },
}

/// What is on disk where the speech model should be.
///
/// [`crate::stt::ModelState`] with the paths rendered as strings and one extra arm for the build
/// that has no `stt` module at all.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ModelView {
    /// The file is there and is the size it should be.
    #[serde(rename_all = "camelCase")]
    Ready { path: String, bytes: u64 },
    /// Nothing is there yet. `bytes` is how big the download is, so the screen can say what it
    /// is about to ask for before anybody clicks.
    #[serde(rename_all = "camelCase")]
    Absent { path: String, bytes: u64 },
    /// Something is there and it is the wrong size. Fetching again replaces it.
    #[serde(rename_all = "camelCase")]
    Damaged { path: String, bytes: u64, expected: u64 },
    /// Something is there and it could not be looked at — a directory in the way, a permission
    /// this user does not have. **Not the same as absent**, and a download would not fix it.
    #[serde(rename_all = "camelCase")]
    Unreadable { path: String, reason: String },
    /// There is no directory to keep it in and none was named.
    #[serde(rename_all = "camelCase")]
    Nowhere { reason: String },
    /// This build has no speech recognition in it, so there is no model it would use.
    #[serde(rename_all = "camelCase")]
    NotHere { reason: String },
}

/// How far along the wake word recording is.
///
/// [`crate::wake::Enrolment`] with the same extra arm.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum WakeState {
    /// Nobody has recorded anything.
    Nothing,
    /// Some takes, and more still wanted.
    #[serde(rename_all = "camelCase")]
    Partial { recorded: usize },
    /// All of them.
    #[serde(rename_all = "camelCase")]
    Complete { recorded: usize },
    /// There is something there and it could not be read. Not an empty store.
    #[serde(rename_all = "camelCase")]
    Unreadable { reason: String },
    /// This build cannot record anything.
    #[serde(rename_all = "camelCase")]
    NotHere { reason: String },
}

/// The wake word, as the screen shows it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeView {
    pub state: WakeState,
    /// Where the takes are kept, for somebody who wants to delete them by hand. `None` on a
    /// machine that names no data directory, and on a build with no wake word module.
    pub dir: Option<String>,
    /// How many takes are wanted in all. `0` where none can be recorded.
    pub wanted: usize,
    /// The longest one take may be, in seconds.
    pub seconds: u64,
    /// **The sentence the screen must render**, taken from `wake::NOTHING_READS_THESE` rather
    /// than written again in TypeScript: that constant has a test on each of its claims, and a
    /// second copy of it in the window is a claim with nothing to keep it true. Empty string in
    /// a build that records nothing, which has no such claim to make.
    pub note: String,
}

/// Whether a microphone is open right now, and why not.
///
/// **Four answers.** "Nobody has turned this on" and "it was turned on and would not start" are
/// the two a person acts on differently, and a screen that showed one switch in one position
/// for both would be hiding the second.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ListeningState {
    /// Nothing is listening, and nobody asked it to.
    Off,
    /// Asked to listen, and getting there: the model is being loaded, or the download is
    /// running. `detail` says which.
    #[serde(rename_all = "camelCase")]
    Starting { detail: String },
    /// A microphone is open and the push-to-talk key is armed.
    #[serde(rename_all = "camelCase")]
    On { device: String },
    /// It was asked to listen and it is not listening. This is the state a switch must not hide.
    #[serde(rename_all = "camelCase")]
    Failed { reason: String },
}

/// Whether this machine reads an answer aloud, and why not when it does not.
///
/// **Separate from [`ListeningState`] because the two halves fail apart.** A machine with a
/// microphone open and no session configured hears everything, transcribes it, publishes
/// `VoiceEvent::Heard`, and never says a word — which is a working half, not a broken whole, and
/// a screen that showed only the listening half would leave a person wondering why it is silent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum SpeakingState {
    /// The audio stack is not in this build, or this machine cannot do speech at all.
    #[serde(rename_all = "camelCase")]
    NotHere { reason: String },
    /// There is no session **yet**. One is made on the first connection, so this is *not yet*
    /// rather than *not going to*: the screen says which, because a person looking at a
    /// machine that has not connected has nothing to do and a person whose account has no
    /// agent does.
    #[serde(rename_all = "camelCase")]
    NoSessionYet,
    /// There is no session and the account's agents are why. Either none — nothing to create
    /// against — or several, which is a choice Zyris does not make: `list_agents` does not
    /// document its order, so picking the first would quietly change which agent this machine
    /// talks to the day somebody adds one. `settings` is where to name one.
    #[serde(rename_all = "camelCase")]
    NoAgent { agents: Vec<String>, settings: String },
    /// Answers from this session are read aloud as they arrive.
    #[serde(rename_all = "camelCase")]
    Session { id: String },
}

/// What is on disk where the voice that reads the answers should be.
///
/// [`crate::tts::VoiceState`] with the directory rendered as a string and one extra arm for the
/// build that has no `tts` module at all — the same mapping [`ModelView`] is of `stt`.
///
/// **A field of its own rather than two more arms on [`SpeakingState`], because the session and
/// the voice are independent facts.** A machine may name a session and have none of the 401 MB
/// on disk, or have every byte of it and name no session, and an enum holding both would have to
/// choose which of the two to be silent about. Before this existed the screen chose the wrong
/// one: a session with no voice rendered as *answers are read aloud as they arrive*, which is
/// exactly what was not happening.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum VoiceModelView {
    /// Every one of the sixteen files is there at the size it should be.
    #[serde(rename_all = "camelCase")]
    Ready { dir: String },
    /// Some are absent or the wrong size. `bytes` is what fetching **those** costs, not what the
    /// whole snapshot costs: a download resumed with fifteen of sixteen files already down must
    /// not ask for 401 MB again on the screen.
    #[serde(rename_all = "camelCase")]
    Incomplete { dir: String, missing: usize, bytes: u64 },
    /// Something is in the way and could not be looked at. **Not the same as absent**, and a
    /// download would not fix it.
    #[serde(rename_all = "camelCase")]
    Unreadable { dir: String, reason: String },
    /// There is no directory to keep them in and none was named.
    #[serde(rename_all = "camelCase")]
    Nowhere { reason: String },
    /// This build has no speech synthesis in it, so there is no voice it would use.
    #[serde(rename_all = "camelCase")]
    NotHere { reason: String },
}

/// Everything the Voice screen reads off this machine, in one answer.
///
/// One structure rather than five commands, for the reason `bridge::AutostartView` gives: the
/// answers have to agree with each other. "Listening, on the built-in microphone" and a device
/// list fetched a round trip later can describe two different moments, and this is exactly the
/// screen where that would show.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceView {
    /// Whether speech can work here at all. The first thing the screen reads: everything below
    /// it is detail about a machine that has already been said to be able or unable to listen.
    pub support: crate::VoiceSupport,
    pub listening: ListeningState,
    pub devices: DeviceList,
    /// Which microphone is stored as the one to open. Not "which one is open" — see
    /// [`ListeningState::On`] for that.
    pub chosen: Choice,
    pub model: ModelView,
    /// The value of `ZYRIS_WHISPER_MODEL`, when it is set to something.
    ///
    /// On the screen it is the difference between a file Zyris downloaded and one an operator
    /// pointed it at: the first has a Delete button and the second must not, because deleting
    /// a file somebody named themselves is this program throwing away their choice. It is also
    /// why a file of the wrong size is not called damaged — see `stt::inspect`.
    pub model_env: Option<String>,
    pub wake: WakeView,
    /// Whether anything is read aloud, and what stops it if not.
    pub speaking: SpeakingState,
    /// What is on disk where the voice should be. Read every time, like [`VoiceView::model`].
    pub voice_model: VoiceModelView,
    /// The value of `ZYRIS_TTS_MODELS`, when it is set to something.
    ///
    /// [`VoiceView::model_env`]'s opposite number, and it buys the same thing: a directory an
    /// operator pointed Zyris at gets no Download button, because those files are theirs to
    /// manage and a fetch into them is this program overwriting a choice somebody made.
    pub voice_model_env: Option<String>,
}

impl VoiceView {
    /// The view of a machine that cannot listen at all, from top to bottom.
    ///
    /// Every arm carries the same sentence, because on such a machine every one of them is the
    /// same fact — and saying it five times is better than a screen with four blanks and one
    /// sentence, which reads as four things that are about to work.
    pub fn unavailable(reason: String) -> VoiceView {
        VoiceView {
            support: crate::VoiceSupport::Unavailable { reason: reason.clone() },
            listening: ListeningState::Off,
            devices: DeviceList::NotHere { reason: reason.clone() },
            chosen: Choice::Default,
            model: ModelView::NotHere { reason: reason.clone() },
            model_env: None,
            speaking: SpeakingState::NotHere { reason: reason.clone() },
            voice_model: VoiceModelView::NotHere { reason: reason.clone() },
            voice_model_env: None,
            wake: WakeView {
                state: WakeState::NotHere { reason },
                dir: None,
                wanted: 0,
                seconds: 0,
                note: String::new(),
            },
        }
    }

    /// The view a build with no audio stack has. [`crate::NOT_COMPILED_IN`] is the sentence.
    pub fn not_compiled_in() -> VoiceView {
        VoiceView::unavailable(crate::NOT_COMPILED_IN.to_string())
    }
}

/// A path as a person reads it.
///
/// Lossy on purpose: a path that is not valid UTF-8 is still a path somebody has to be shown,
/// and refusing to render it would blank the one line on the screen that says where to look.
pub fn show(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire shape the window switches on, pinned here rather than discovered in TypeScript.
    ///
    /// Only the two that are easiest to get wrong: a tag called `state` on a field also called
    /// `state` (the MCP screen has the same shape and reads `server.state.state`), and the
    /// device list, whose `notHere` arm is the one a build with no audio stack always takes.
    #[test]
    fn the_screens_answers_are_tagged_on_state_in_camel_case() {
        let json = serde_json::to_string(&DeviceList::NotHere { reason: "no".into() })
            .expect("serializable");
        assert_eq!(json, r#"{"state":"notHere","reason":"no"}"#);

        let json = serde_json::to_string(&ListeningState::On { device: "Mic".into() })
            .expect("serializable");
        assert_eq!(json, r#"{"state":"on","device":"Mic"}"#);
    }

    /// **A build with no audio stack must not answer anything that reads as an empty list.**
    /// `devices: []` there would say this computer has no microphone, which is a claim about
    /// the computer rather than about the build.
    #[test]
    fn a_build_with_no_audio_stack_says_so_in_every_answer_rather_than_answering_empty() {
        let view = VoiceView::not_compiled_in();

        assert!(matches!(view.devices, DeviceList::NotHere { .. }));
        assert!(matches!(view.model, ModelView::NotHere { .. }));
        assert!(matches!(view.wake.state, WakeState::NotHere { .. }));
        // And the wake note is empty rather than carrying a claim about something this build
        // cannot do: the sentence is about takes nothing reads, and there are no takes.
        assert_eq!(view.wake.note, "");
    }

    /// The reason is the one sentence `zyris-app` already logs for such a build, not a second
    /// wording of it.
    #[test]
    fn the_reason_a_build_gives_is_the_crates_own_sentence() {
        let view = VoiceView::not_compiled_in();

        assert_eq!(
            view.support,
            crate::VoiceSupport::Unavailable { reason: crate::NOT_COMPILED_IN.to_string() }
        );
    }
}
