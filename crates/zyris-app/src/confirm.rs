//! The question this machine asks before it sends a file to a machine nobody here has approved,
//! and the slot a person's answer comes back through.
//!
//! This is its own module because it owns a piece of state unlike anything else in the app: a
//! question that is *waiting*. Every other value the window reads is a fact the core already
//! knows and can restate at any time. This one has a caller blocked on it, a deadline, and
//! exactly one answer that counts — and it stops existing the moment that answer arrives.
//!
//! # Where it is consulted, and where it is not
//!
//! [`zyris_tools::PeerConfirmer`] reaches exactly one place: `LocalFileTransfer::send_to`, on the
//! **sending** side, through `TofuStore::authorize`. Nothing on the receiving side asks it
//! anything. So approving a peer here makes this machine willing to *send* to that name; it does
//! not add a door on files arriving. See `zyris_tools::transfer`'s "What gates a transfer, in
//! each direction".
//!
//! # Only an unknown peer is ever offered
//!
//! `TofuStore::authorize` settles a name it has already pinned by itself, and refuses a *changed*
//! key outright without asking anyone — upstream's reason being that "asking 'are you sure?'
//! about a substitution would just give an attacker a second, quieter attempt at the same slug".
//! So nothing in this module has, or may grow, a path for approving a key that replaced a pinned
//! one: it will never be handed one, and inventing a way to say yes to it would undo that.
//!
//! # Why a question here can expire
//!
//! `send_to` wraps the whole of its work — the file hash, the peer lookup, `authorize`, the dial
//! — in `tokio::time::timeout(DEFAULT_WIRE_DEADLINE)`, 55 seconds, because Attacca cuts a node
//! call off at 60. So the person is not the only clock in the room: past that point the caller's
//! future is **dropped mid-`confirm`**, and an answer given afterwards has nobody left to reach.
//! Two consequences shape everything below — [`ANSWER_DEADLINE`], and the withdrawal that happens
//! when a `confirm` future is cancelled rather than finished.
//!
//! # Why a question cannot follow another straight away
//!
//! Refusing a second question *while one waits* is not enough on its own, and the gap it leaves is
//! the same attack [`WindowConfirmer`] refuses to queue for, reached by retrying instead. The
//! instant a question ends — answered, expired, or its caller cut off — the slot is free, and a
//! sender that simply asks again lands a new label and a new fingerprint in the same place on the
//! screen, under the same two buttons, in the moment a person is least likely to look. So the slot
//! stays shut for [`ASK_COOL_OFF`] after every question leaves it, and the window keeps its buttons
//! dead for a shorter beat whenever the question it is showing changes. Neither guard is the whole
//! of it: this one decides when a replacement may exist at all, and the window's covers a click
//! already on its way down when one does.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::oneshot;
// tokio's clock rather than `std`'s, so [`ASK_COOL_OFF`] can be tested by moving time instead of
// by sitting out two real seconds. Outside a runtime — which is where `answer` runs, on Tauri's
// IPC thread — it reads the same monotonic clock `std::time::Instant` does, so a process that
// never pauses the clock cannot tell the two apart.
use tokio::time::Instant;

/// How long a question stays on the screen before it answers itself with "no".
///
/// **Bounded from above by the caller, not by patience.** `send_to` gives its whole self 55
/// seconds (`zyris_tools::WIRE_DEADLINE`, upstream's `DEFAULT_WIRE_DEADLINE`), of which the file
/// hash and the peer lookup have already spent some before `confirm` is ever called. A wait
/// longer than that cannot help anybody: the caller is cut off first, this future is dropped, and
/// a person who answers at second 70 is answering a question that stopped existing at second 55.
///
/// 45 leaves about ten seconds of that budget for the file that took a moment to hash and for the
/// refusal to travel back out. Coming in under the wire that way is the whole point: an expiry
/// **this** module owns is reported to the agent as `peer_not_confirmed` — "a person did not
/// approve this" — while being cut off by the caller's own deadline produces `pending: true` and
/// "the peer had not been reached yet", which says nothing about the person who was never there.
///
/// **Failing closed is not a tuning choice.** A refusal costs a retry, and the second question is
/// answered in seconds by the person who is now at the screen. An approval nobody gave costs a
/// pin, and a pin is what every later send to that name is measured against.
pub const ANSWER_DEADLINE: Duration = Duration::from_secs(45);

/// How long the slot stays shut after a question leaves it, however it left.
///
/// **This is a guard against a swap, not a rate limit.** The thing it has to make impossible is a
/// second question taking the screen while a person's hand is still on the first one: they have
/// finished reading Q1's fingerprint, they are pressing Approve, and if Q2 can appear in that
/// instant — same place, same two buttons, a slug the other side chose to be the same width — the
/// click they decided on for Q1 pins a key nobody read. Refusing only *while one waits* does not
/// cover it, because the dangerous moment begins exactly when the waiting stops.
///
/// **Two seconds, because that is one full recheck of the window plus room for the answer to get
/// there.** `PeerConfirm.tsx` re-asks whether its question is still waiting once a second
/// (`RECHECK_MS`), and replaces both buttons with a sentence the moment it learns one has ended.
/// A cool-off longer than that poll guarantees the screen has had a whole cycle in which to stop
/// offering an Approve button *before* anything else can be installed behind it — so the two
/// guards overlap rather than leaving a seam between them. The window's own lockout
/// (`SWAP_LOCKOUT_MS`, 750 ms) is the shorter backstop for the case where a swap does reach the
/// screen anyway, and it is deliberately the smaller of the two.
///
/// **Nothing checks these two numbers against each other**, any more than anything checks the
/// event name or the field names that cross the same boundary. Shortening this below `RECHECK_MS`
/// takes the overlap away, and the only thing that would notice is somebody reading both files.
///
/// **What it costs is bounded and the cost is on the right side.** A legitimate second caller is
/// refused for two seconds out of the 55 it has, is told `peer_not_confirmed`, and asks again;
/// nothing is pinned either way. It also puts a ceiling on how often a retrying sender can make
/// `gui::raise_the_window_for_a_peer_question` bring the window to the front, which is otherwise
/// once per attempt for as long as it cares to keep asking.
pub const ASK_COOL_OFF: Duration = Duration::from_secs(2);

/// What the window shows: the name a person picked for the other machine, and the fingerprint to
/// compare against what that machine displays on its own screen.
///
/// **Both strings are passed through untouched**, which is the one thing this type has to get
/// right. `fingerprint` is upstream's rendering — 128 bits as eight space-separated groups of
/// four uppercase hex digits — and it is meant to be read character by character beside another
/// screen. Anything that re-cases, re-groups or truncates it makes that comparison fail for a
/// reason that has nothing to do with the keys.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Question {
    /// Which question an answer is answering. Handed out by [`Pending`], never reused, and never
    /// zero — so a window that posts a default-initialised id cannot approve anything.
    pub id: u64,
    /// The `peer_slug`: a name the *user* chose, which is what the ledger is keyed on.
    pub label: String,
    /// The peer's fingerprint, exactly as upstream rendered it.
    pub fingerprint: String,
}

/// One question, the caller waiting on it, and the id that ties an answer to it.
struct Waiting {
    question: Question,
    /// Dropped without sending when the question is withdrawn, which is how a cancelled or
    /// expired `confirm` tells the waiting side that no answer is coming.
    answer: oneshot::Sender<bool>,
}

/// What the slot holds: the question waiting in it, and when the last one left.
///
/// The two are one value behind one lock because [`Pending::ask`] reads both to make a single
/// decision. Kept apart they would be two locks to take in the right order, for a question that is
/// only ever asked while the slot is already held.
#[derive(Default)]
struct Slot {
    waiting: Option<Waiting>,
    /// When a question last left this slot — answered, expired, or withdrawn — or `None` while no
    /// question has ever been in it. Every path that empties the slot stamps this, and
    /// [`Pending::ask`] refuses for [`ASK_COOL_OFF`] afterwards.
    vacated: Option<Instant>,
}

/// The slot a question sits in while it waits, reachable from both sides.
///
/// Cheap to clone and shared by handle: the confirmer running on a tokio worker and the window
/// answering from a Tauri command are looking at the same slot, not at two copies of it.
///
/// A `std::sync::Mutex` rather than tokio's, deliberately. Nothing here awaits while holding it —
/// every critical section is a field move — and one of the two writers is [`Withdraw::drop`],
/// which runs when a future is *cancelled* and so cannot await anything at all.
#[derive(Clone, Default)]
pub struct Pending {
    slot: Arc<Mutex<Slot>>,
    /// Monotonic and never reused. Counted from 1 rather than 0, so that zero — what an
    /// uninitialised field on the other side of the wire holds — can never name a question. See
    /// [`Question::id`].
    next_id: Arc<AtomicU64>,
}

/// Why [`Pending::ask`] would not install a question.
///
/// Told apart rather than folded into one `None`, because the agent's side of either is a bare
/// `peer_not_confirmed` and the log line is the only place the difference can be read. "Somebody
/// else's question is on the screen" and "the last question ended a moment ago" call for different
/// things from whoever is looking at the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Busy {
    /// A question is on the screen right now, and a second one would have to queue behind it. See
    /// [`WindowConfirmer`] for why that is refused rather than queued.
    AnotherIsWaiting,
    /// One left the screen less than [`ASK_COOL_OFF`] ago. See that constant.
    OneJustEnded,
    /// The lock is poisoned, so the slot cannot be read at all. Only reachable after a panic
    /// somewhere else in this process; refusing is the answer that pins nothing.
    SlotUnreadable,
}

impl Busy {
    /// What to say in the log. Not `Display`: this is one field of a structured event rather than
    /// a sentence, and the sentence around it is the caller's.
    fn reason(self) -> &'static str {
        match self {
            Busy::AnotherIsWaiting => "another peer is already waiting to be approved",
            Busy::OneJustEnded => "another peer's question ended a moment ago",
            Busy::SlotUnreadable => "the question slot could not be read",
        }
    }
}

impl Pending {
    pub fn new() -> Pending {
        Pending::default()
    }

    /// What the window should be showing, if anything.
    ///
    /// A window that came up *after* the question was asked has no other way to find out about
    /// it, and a window that was already open needs this after a reload. Returns `None` the
    /// instant the question is answered, expires or is withdrawn — a question that is not waiting
    /// is not a question, and leaving it visible would put a live-looking button in front of
    /// somebody whose click cannot land.
    pub fn question(&self) -> Option<Question> {
        self.slot.lock().ok()?.waiting.as_ref().map(|waiting| waiting.question.clone())
    }

    /// Answers the question with `id`, and says whether that reached anyone.
    ///
    /// **`false` means nothing happened, and that is the answer this method exists to give.** A
    /// window left open from a question that already expired, a second click on a button that was
    /// never redrawn, a reply racing the caller's own deadline: every one of them arrives here
    /// with an id that is no longer waiting, and every one of them has to change nothing. The
    /// failure this whole module is built against is an approval landing on a question the person
    /// was not looking at.
    ///
    /// So the id is checked rather than assumed — an answer that names a question other than the
    /// one waiting is refused outright and the waiting one is left alone — and the send itself has
    /// to succeed: a caller whose future was already dropped is not "still waiting" in any sense a
    /// window should be told `true` about.
    pub fn answer(&self, id: u64, approved: bool) -> bool {
        let Ok(mut slot) = self.slot.lock() else {
            return false;
        };
        // Peeked before it is taken. Taking first and putting a mismatch back would be the same
        // thing on a good day and a dropped question on a panic between the two.
        if slot.waiting.as_ref().is_none_or(|waiting| waiting.question.id != id) {
            return false;
        }
        let Some(waiting) = slot.waiting.take() else {
            return false;
        };
        // Stamped whether or not the answer reaches anybody below. The cool-off is about what the
        // *screen* just did, and a question whose caller had already gone still occupied that
        // screen until this moment.
        slot.vacated = Some(Instant::now());
        waiting.answer.send(approved).is_ok()
    }

    /// Installs a question, or refuses to.
    ///
    /// Two refusals, and the second is the one this method exists for. A question already waiting
    /// is refused rather than queued — see [`WindowConfirmer`] — and a question that has only just
    /// *stopped* waiting shuts the slot for [`ASK_COOL_OFF`] afterwards, because the instant after
    /// one ends is exactly when a replacement would be read least carefully. See that constant.
    fn ask(
        &self,
        label: &str,
        fingerprint: &str,
    ) -> Result<(Question, oneshot::Receiver<bool>), Busy> {
        let Ok(mut slot) = self.slot.lock() else {
            return Err(Busy::SlotUnreadable);
        };
        if slot.waiting.is_some() {
            return Err(Busy::AnotherIsWaiting);
        }
        if slot.vacated.is_some_and(|left| left.elapsed() < ASK_COOL_OFF) {
            return Err(Busy::OneJustEnded);
        }
        // Inside the lock, so two callers racing here cannot both take an id and cannot both
        // believe they installed one.
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let question = Question {
            id,
            label: label.to_string(),
            fingerprint: fingerprint.to_string(),
        };
        let (answer, receiver) = oneshot::channel();
        slot.waiting = Some(Waiting { question: question.clone(), answer });
        Ok((question, receiver))
    }

    /// Clears the slot if — and only if — it still holds the question with `id`.
    ///
    /// The guard is the id, not the slot being occupied. By the time a withdrawal runs the
    /// question may already have been answered and a *different* one asked, and clearing that one
    /// would take a live question off the screen while its caller waited out the full deadline.
    fn withdraw(&self, id: u64) {
        let Ok(mut slot) = self.slot.lock() else {
            return;
        };
        if slot.waiting.as_ref().is_some_and(|waiting| waiting.question.id == id) {
            slot.waiting = None;
            // Stamped here too, and only here: this arm is the one that actually took a question
            // off the screen. The answered path ran the same `Drop` a moment after `answer`
            // emptied the slot, finds no match, and so cannot push the cool-off out past the
            // moment the person's own answer ended it.
            slot.vacated = Some(Instant::now());
        }
    }
}

/// Takes the question off the screen however `confirm` ends.
///
/// **Including the way it ends that has no code of its own: cancellation.** `send_to` drops the
/// whole of `send_inner` when its 55-second deadline fires, which drops the `confirm` future
/// mid-await. Without this, the question would stay in the slot with a dead receiver behind it —
/// visible to the window, blocking the next question for the rest of the process, and offering a
/// person an Approve button whose click cannot reach anybody. One guard covers that, the timeout
/// and the ordinary answered path, rather than three cleanups that can drift apart.
struct Withdraw {
    pending: Pending,
    id: u64,
}

impl Drop for Withdraw {
    fn drop(&mut self) {
        self.pending.withdraw(self.id);
    }
}

/// What puts a question in front of a person. Called once per question, after it is installed.
///
/// A callback rather than a Tauri handle, so this module owns the waiting and nothing else: the
/// window, the tray and the event that carries the question are the GUI's business, and a test
/// can record what it was handed without building an app.
pub type Show = Arc<dyn Fn(&Question) + Send + Sync>;

/// The [`zyris_tools::PeerConfirmer`] a windowed run installs: it shows the question and waits for
/// a person.
///
/// # One question at a time, and the second is refused
///
/// Two agents can call `send_to` against two unpinned peers at once. This refuses the second
/// immediately rather than queueing it, for two reasons that both point the same way.
///
/// A queued question would land on the screen in the same place, with the same two buttons, in the
/// instant after a person clicked Approve on the first — which is the moment they are least likely
/// to read it. Training somebody to click through a dialog is exactly how a fingerprint stops
/// being compared, and a fingerprint nobody compares is worth nothing. Whoever can provoke a
/// second `send_to` gets to choose that moment.
///
/// And a queued question would be answering on borrowed time anyway: its own caller's 55 seconds
/// are already running, so what it inherits is whatever is left after the first question is
/// settled — unpredictable, often short, and spent on a screen the person is no longer reading
/// carefully.
///
/// **The second caller is refused, not dropped.** It gets `false` at once, which `authorize` turns
/// into `peer_not_confirmed`; nothing is pinned, and calling again once the first question is
/// answered *and the slot has cooled off* asks properly. The first question is untouched — it
/// keeps its slot and its answer still reaches its own caller.
///
/// # And the second is refused for a moment after the first ends, too
///
/// Refusing only while a question waits would hand back the same moment by another road. The slot
/// is free the instant Q1 ends, so a sender that keeps calling gets its question onto the screen a
/// millisecond later — in the same place, with the same two buttons, while a person who has
/// finished reading Q1's fingerprint is pressing Approve. The retry reaches exactly the moment the
/// queue was refused for. [`ASK_COOL_OFF`] shuts the slot for a beat after every question leaves
/// it, and `PeerConfirm.tsx` keeps both buttons dead for a shorter beat whenever the question on
/// the screen changes; the first decides whether a replacement may exist, the second catches a
/// click already on its way down when one does.
pub struct WindowConfirmer {
    pending: Pending,
    show: Show,
}

impl WindowConfirmer {
    pub fn new(pending: Pending, show: Show) -> WindowConfirmer {
        WindowConfirmer { pending, show }
    }
}

#[zyris_tools::async_trait]
impl zyris_tools::PeerConfirmer for WindowConfirmer {
    async fn confirm(&self, label: &str, fingerprint: &str) -> bool {
        let (question, answer) = match self.pending.ask(label, fingerprint) {
            Ok(installed) => installed,
            // Said out loud because the agent's side of this is a bare `peer_not_confirmed`, and
            // neither "somebody else's question is on the screen" nor "one ended a moment ago" is
            // something it can work out.
            Err(why) => {
                tracing::warn!(
                    %label,
                    reason = why.reason(),
                    "refusing to send to an unapproved machine: nothing was asked and nothing was \
                     pinned. Answer whatever is on the screen, then ask again"
                );
                return false;
            }
        };

        // Held for the rest of this function, and dropped by cancellation too. See `Withdraw`.
        let _withdraw = Withdraw { pending: self.pending.clone(), id: question.id };

        tracing::info!(
            %label,
            fingerprint = %question.fingerprint,
            "asking whether to send to a machine this one has not approved"
        );
        (self.show)(&question);

        match tokio::time::timeout(ANSWER_DEADLINE, answer).await {
            Ok(Ok(approved)) => approved,
            // The sender was dropped without an answer. Nothing does that today except a
            // withdrawal, and a withdrawn question is one nobody answered.
            Ok(Err(_)) => false,
            Err(_) => {
                tracing::info!(
                    %label,
                    seconds = ANSWER_DEADLINE.as_secs(),
                    "nobody answered in time, so this machine was not approved and nothing was \
                     pinned; sending again asks again"
                );
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zyris_tools::PeerConfirmer as _;

    /// Records every question it is shown, so a test can assert on what a person would have seen
    /// rather than on the arguments it passed in a moment earlier.
    fn recorder() -> (Show, Arc<Mutex<Vec<Question>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let show: Show = Arc::new(move |question: &Question| {
            sink.lock().unwrap().push(question.clone());
        });
        (show, seen)
    }

    /// Waits for `confirm` to install its question. The spawned task has to be polled before the
    /// slot is filled, and how many yields that takes is the scheduler's business, so this loops
    /// rather than assuming one.
    async fn wait_for_question(pending: &Pending) -> Question {
        for _ in 0..1000 {
            if let Some(question) = pending.question() {
                return question;
            }
            tokio::task::yield_now().await;
        }
        panic!("no question was ever asked");
    }

    /// The same, for a caller that reads the ledger off a real disk before it asks. A spin on
    /// `yield_now` can run out of patience while a file read is in flight and nothing is ready to
    /// poll; this waits in wall-clock time instead, for up to two seconds.
    async fn wait_for_question_patiently(pending: &Pending) -> Question {
        for _ in 0..2000 {
            if let Some(question) = pending.question() {
                return question;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("no question was ever asked");
    }

    #[tokio::test]
    async fn an_answer_of_yes_reaches_the_caller() {
        let pending = Pending::new();
        let (show, _seen) = recorder();
        let confirmer = WindowConfirmer::new(pending.clone(), show);

        let asked = tokio::spawn(async move { confirmer.confirm("laptop", "AB12 CD34").await });
        let question = wait_for_question(&pending).await;

        assert!(pending.answer(question.id, true), "the question was still waiting");
        assert!(asked.await.unwrap(), "yes has to reach the caller as yes");
    }

    #[tokio::test]
    async fn an_answer_of_no_reaches_the_caller_and_pins_nothing() {
        // `TofuStore::authorize` pins on `true` and only on `true`: a `false` becomes
        // `TofuError::Refused` before `pin_preapproved` is reached. So "pins nothing" is exactly
        // "the caller was told false", and this is the half of the pair that must not drift into
        // returning a hopeful default.
        let pending = Pending::new();
        let (show, _seen) = recorder();
        let confirmer = WindowConfirmer::new(pending.clone(), show);

        let asked = tokio::spawn(async move { confirmer.confirm("laptop", "AB12 CD34").await });
        let question = wait_for_question(&pending).await;

        assert!(pending.answer(question.id, false), "the question was still waiting");
        assert!(!asked.await.unwrap(), "no has to reach the caller as no");
    }

    #[tokio::test]
    async fn the_window_is_shown_the_name_and_the_fingerprint_it_was_given() {
        // The person is comparing this against the other machine's own screen, character by
        // character. A question that re-cases, re-groups or truncates either string makes that
        // comparison fail for a reason that has nothing to do with the keys — and a fingerprint
        // that fails for the wrong reason is one people stop reading.
        const LABEL: &str = "Kitchen-Pi";
        const FINGERPRINT: &str = "9F2A 41C7 0E83 BB15 6D04 A97E 22C1 5FB8";

        let pending = Pending::new();
        let (show, seen) = recorder();
        let confirmer = WindowConfirmer::new(pending.clone(), show);

        let asked = tokio::spawn(async move { confirmer.confirm(LABEL, FINGERPRINT).await });
        let question = wait_for_question(&pending).await;

        // Both surfaces, because they are two different paths to the same strings: the one a
        // window already open is handed, and the one a window that opened late reads back.
        let shown = seen.lock().unwrap().clone();
        assert_eq!(shown.len(), 1, "one question should have been shown: {shown:?}");
        assert_eq!(shown[0].label, LABEL);
        assert_eq!(shown[0].fingerprint, FINGERPRINT);
        assert_eq!(question.label, LABEL);
        assert_eq!(question.fingerprint, FINGERPRINT);
        assert_eq!(shown[0].id, question.id, "both surfaces must name the same question");

        pending.answer(question.id, false);
        let _ = asked.await;
    }

    #[tokio::test]
    async fn an_answer_to_a_question_that_is_no_longer_waiting_is_refused() {
        // A stale window, a double click, a reply after a timeout. Each must return `false` and
        // change nothing: an approval landing on a question the person was not looking at is the
        // failure this module exists to prevent.
        let pending = Pending::new();
        let (show, _seen) = recorder();
        let confirmer = WindowConfirmer::new(pending.clone(), show);

        let asked = tokio::spawn(async move { confirmer.confirm("laptop", "AB12 CD34").await });
        let question = wait_for_question(&pending).await;

        // An id nobody handed out — a window rendering from a stale value, or a default-zero one.
        assert!(!pending.answer(0, true), "zero is never a question");
        assert!(
            !pending.answer(question.id + 1, true),
            "an id that was never asked must not answer the one that was"
        );
        // The positive control: this is the real answer, and it has to work — otherwise every
        // assertion above would also hold for an `answer` that simply always says no.
        assert!(pending.answer(question.id, false), "the real answer reaches the caller");
        assert!(!asked.await.unwrap(), "and it is the answer that was given");

        // The double click: the same id, a second time, now saying yes.
        assert!(!pending.answer(question.id, true), "a second click answers nothing");
        assert!(pending.question().is_none(), "an answered question is no longer shown");

        // The narrowest shape of "no longer waiting", and the one no amount of window discipline
        // can prevent: the caller's future was dropped a moment ago and the slot has not been
        // cleared yet, because the receiver dies a step before `Withdraw::drop` runs. The id
        // matches and the question is right there, so every check above passes — and the answer
        // still reaches nobody. Reporting `true` here would tell a window its click landed while
        // the send it was approving had already given up.
        let pending = Pending::new();
        let (question, receiver) = pending.ask("laptop", "AB12 CD34").unwrap();
        drop(receiver);
        assert!(
            !pending.answer(question.id, true),
            "an answer with nobody left to receive it is not an answer"
        );
        assert!(pending.question().is_none(), "and it does not stay on the screen either");
    }

    #[tokio::test]
    async fn a_second_question_while_one_waits_does_not_lose_the_first() {
        // The decision, asserted rather than inherited: the second is refused at once, and the
        // first keeps its slot, its screen and its answer. See `WindowConfirmer`'s docs.
        let pending = Pending::new();
        let (show, seen) = recorder();
        let confirmer = Arc::new(WindowConfirmer::new(pending.clone(), show));

        let first = {
            let confirmer = confirmer.clone();
            tokio::spawn(async move { confirmer.confirm("laptop", "AB12 CD34").await })
        };
        let question = wait_for_question(&pending).await;

        // **Immediately**, which is half the decision and needs asserting on its own. A queue
        // that eventually refused would also return `false` here — a second or so after the first
        // question timed out, three quarters of a minute from now, by which time the second
        // caller has been cut off by its own deadline anyway. The bound is a second because the
        // refusal takes no await at all; anything approaching it means this queued.
        let second = tokio::time::timeout(
            Duration::from_secs(1),
            confirmer.confirm("desktop", "EF56 7890"),
        )
        .await
        .expect("the second question is refused at once rather than queued behind the first");
        assert!(!second, "and the refusal is a no");

        // The first is untouched: still on the screen, still the one an answer reaches.
        assert_eq!(
            pending.question().as_ref().map(|q| q.id),
            Some(question.id),
            "the refused second question must not disturb the first"
        );
        assert!(pending.answer(question.id, true), "the first is still waiting");
        assert!(first.await.unwrap(), "and its answer still reaches its own caller");

        // A person was shown one question, not two. The second never reached a screen.
        let shown = seen.lock().unwrap().clone();
        assert_eq!(shown.len(), 1, "only the first question is shown: {shown:?}");
        assert_eq!(shown[0].label, "laptop");
    }

    #[tokio::test]
    async fn a_question_nobody_answers_expires_as_a_refusal() {
        // The window nobody is sitting at. It must answer itself, and the answer must be no.
        let pending = Pending::new();
        let (show, _seen) = recorder();
        let confirmer = WindowConfirmer::new(pending.clone(), show);

        let waiting = pending.clone();
        let asked = tokio::spawn(async move { confirmer.confirm("laptop", "AB12 CD34").await });
        let question = wait_for_question(&waiting).await;

        // The clock is moved rather than waited out: pausing after the question is installed
        // leaves the deadline registered where `confirm` put it, and advancing past it fires the
        // real `ANSWER_DEADLINE` instead of a shortened one smuggled in for the test.
        tokio::time::pause();
        tokio::time::advance(ANSWER_DEADLINE + Duration::from_secs(1)).await;

        assert!(!asked.await.unwrap(), "an unanswered question expires as a refusal");
        assert!(pending.question().is_none(), "an expired question stops being shown");
        assert!(
            !pending.answer(question.id, true),
            "and an approval arriving after it cannot pin anything"
        );
    }

    #[tokio::test]
    async fn a_question_whose_caller_gave_up_stops_being_shown() {
        // `send_to` drops the whole of `send_inner` when its own 55-second deadline fires, which
        // drops this future mid-await. Nothing in `confirm` runs after that, so the slot is
        // cleared by `Withdraw::drop` or not at all — and "not at all" means a dead question on
        // the screen, no further question ever asked, and an Approve button that reaches nobody.
        let pending = Pending::new();
        let (show, _seen) = recorder();
        let confirmer = WindowConfirmer::new(pending.clone(), show);

        let waiting = pending.clone();
        let asked = tokio::spawn(async move { confirmer.confirm("laptop", "AB12 CD34").await });
        let question = wait_for_question(&waiting).await;

        asked.abort();
        let _ = asked.await;
        for _ in 0..1000 {
            if pending.question().is_none() {
                break;
            }
            tokio::task::yield_now().await;
        }

        assert!(pending.question().is_none(), "a cancelled question stops being shown");
        assert!(
            !pending.answer(question.id, true),
            "and cannot be approved after the fact"
        );
        // The slot is free again, which is the part that would otherwise stay broken for the rest
        // of the process. The clock is moved past the cool-off first: a withdrawal empties the
        // screen exactly as an answer does and shuts the slot for the same beat. That is the
        // subject of the test below rather than of this one.
        tokio::time::pause();
        tokio::time::advance(ASK_COOL_OFF).await;
        let confirmer = WindowConfirmer::new(pending.clone(), recorder().0);
        let next = tokio::spawn(async move { confirmer.confirm("desktop", "EF56 7890").await });
        let after = wait_for_question(&pending).await;
        assert_ne!(after.id, question.id, "the next question is a new one");
        pending.answer(after.id, false);
        let _ = next.await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_question_cannot_take_the_screen_the_instant_the_last_one_left() {
        // The attack the cool-off exists for, and the one `a_second_question_while_one_waits_\
        // does_not_lose_the_first` does not reach: the same sender waits for the first question to
        // *stop* waiting and then asks again. Nothing is queued, so nothing there refuses it — the
        // slot is free a microsecond after Approve is pressed, and a replacement lands in the same
        // place, under the same two buttons, while a person's hand is still coming down.
        let pending = Pending::new();
        let (show, seen) = recorder();
        let confirmer = Arc::new(WindowConfirmer::new(pending.clone(), show));

        let first = {
            let confirmer = confirmer.clone();
            tokio::spawn(async move { confirmer.confirm("laptop", "AB12 CD34").await })
        };
        let question = wait_for_question(&pending).await;
        assert!(pending.answer(question.id, true), "the first question is answered normally");
        assert!(first.await.unwrap(), "and its answer reaches its own caller");

        // Immediately, the way a retry loop would. The bound is the same one the queue test uses
        // and for the same reason: the refusal takes no await at all, so anything approaching it
        // means this waited for the cool-off rather than refusing through it.
        let replacement = tokio::time::timeout(
            Duration::from_secs(1),
            confirmer.confirm("laptop-2", "AB12 CD34"),
        )
        .await
        .expect("a replacement is refused at once rather than held until the slot reopens");
        assert!(!replacement, "and the refusal is a no, so nothing is pinned");
        assert!(
            pending.question().is_none(),
            "nothing may take the screen in the instant after an answer"
        );

        // The other half, and the reason this is not only about the buttons: `show` is what
        // publishes the event `gui::raise_the_window_for_a_peer_question` brings the window
        // forward on. A sender that could retry freely would raise it once per attempt.
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "a refused replacement must not reach a screen, or raise a window"
        );

        // And the cool-off is a beat, not a door: past it the next caller is asked properly.
        tokio::time::advance(ASK_COOL_OFF).await;
        let next = {
            let confirmer = confirmer.clone();
            tokio::spawn(async move { confirmer.confirm("desktop", "EF56 7890").await })
        };
        let after = wait_for_question(&pending).await;
        assert_ne!(after.id, question.id, "the next question is a new one");
        assert_eq!(after.label, "desktop");
        assert!(pending.answer(after.id, false), "and it can be answered");
        assert!(!next.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn an_expired_question_shuts_the_slot_for_the_same_beat_an_answered_one_does() {
        // The expiry path empties the slot from inside `Withdraw::drop` rather than from `answer`,
        // so it is a second place the stamp has to be written — and the person at the screen is in
        // the worse position of the two: nothing they did ended the question, so they have no
        // reason to expect the screen to change at all.
        let pending = Pending::new();
        let (show, seen) = recorder();
        let confirmer = Arc::new(WindowConfirmer::new(pending.clone(), show));

        let first = {
            let confirmer = confirmer.clone();
            tokio::spawn(async move { confirmer.confirm("laptop", "AB12 CD34").await })
        };
        wait_for_question(&pending).await;
        tokio::time::advance(ANSWER_DEADLINE + Duration::from_secs(1)).await;
        assert!(!first.await.unwrap(), "an unanswered question expires as a refusal");

        let replacement = tokio::time::timeout(
            Duration::from_secs(1),
            confirmer.confirm("laptop-2", "AB12 CD34"),
        )
        .await
        .expect("a replacement after an expiry is refused at once too");
        assert!(!replacement);
        assert!(pending.question().is_none());
        assert_eq!(seen.lock().unwrap().len(), 1, "and it never reached a screen");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_approval_pins_the_key_that_was_on_the_screen_and_a_refusal_pins_nothing() {
        // **The half of TOFU nothing else in this repository covers.** Everywhere else asserts
        // that a refusal refuses; this drives the real `TofuStore::authorize` — the one call
        // `LocalFileTransfer::send_to` makes — through a person saying yes and a person saying no,
        // and looks at the ledger afterwards. An approval that quietly pinned nothing would leave
        // every send asking again forever, and a refusal that pinned anyway would read as working
        // right up until the same peer sailed through as already known.
        //
        // One ledger and two slots. The slot is plumbing — two questions through one would run
        // into `ASK_COOL_OFF`, which is a different test's subject — while the ledger is what this
        // is about, so both answers are measured against the same one. `authorize` takes its
        // confirmer as an argument, which is how `send_to` reaches it too, so the one `Peering`
        // binds with a `DenyUnknown` that is never consulted.
        let here = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();

        // A real peer with a real key, because `authorize` canonicalizes and fingerprints what it
        // is given before it consults anybody: a string that merely looks like an endpoint id is
        // turned away for that reason and never reaches the decision this test is about.
        let stranger =
            zyris_tools::Peering::bind(elsewhere.path(), Arc::new(zyris_tools::DenyUnknown))
                .await
                .unwrap();
        let stranger_id = stranger.endpoint().id().to_string();

        let ledger = zyris_tools::Peering::bind(here.path(), Arc::new(zyris_tools::DenyUnknown))
            .await
            .unwrap();

        // No, first: it has to leave the ledger exactly as it found it, and running it second
        // would be measuring it against a slug that is already pinned.
        let refusing = Pending::new();
        let (show, refused) = recorder();
        let answering = tokio::spawn({
            let refusing = refusing.clone();
            async move {
                let question = wait_for_question_patiently(&refusing).await;
                assert!(refusing.answer(question.id, false), "the question was still waiting");
            }
        });
        let outcome = ledger
            .tofu()
            .authorize(&WindowConfirmer::new(refusing.clone(), show), "laptop", &stranger_id)
            .await;
        answering.await.unwrap();

        assert!(outcome.is_err(), "a person said no, so the send must fail: {outcome:?}");
        assert!(
            ledger.tofu().pins().await.unwrap().is_empty(),
            "a refusal pinned a key anyway, which is this whole module failing open"
        );
        assert_eq!(refused.lock().unwrap().len(), 1, "and somebody was asked exactly once");

        // Yes, on the same ledger and the same name.
        let approving = Pending::new();
        let (show, approved) = recorder();
        let answering = tokio::spawn({
            let approving = approving.clone();
            async move {
                let question = wait_for_question_patiently(&approving).await;
                assert!(approving.answer(question.id, true), "the question was still waiting");
            }
        });
        let outcome = ledger
            .tofu()
            .authorize(&WindowConfirmer::new(approving.clone(), show), "laptop", &stranger_id)
            .await;
        answering.await.unwrap();

        assert!(outcome.is_ok(), "a person said yes, so the send must be allowed: {outcome:?}");
        assert_eq!(
            ledger.tofu().pins().await.unwrap(),
            vec![("laptop".to_string(), stranger_id.clone())],
            "an approval has to pin exactly the key that was offered, under the name it was \
             offered as, and nothing else"
        );

        // **And it is the key the person actually read.** A pin is only worth what the comparison
        // in front of it was worth, so the fingerprint on the screen has to be the one that
        // machine reports for itself — the same string `Peering::fingerprint` hands its own Status
        // screen. Anything that re-derived it from somewhere else would pin a key nobody compared.
        let shown = approved.lock().unwrap().clone();
        assert_eq!(shown.len(), 1, "one question should have been shown: {shown:?}");
        assert_eq!(shown[0].label, "laptop");
        assert_eq!(
            shown[0].fingerprint,
            stranger.fingerprint(),
            "the person was shown a fingerprint the other machine does not report for itself"
        );
    }

    #[test]
    fn the_cool_off_covers_a_whole_recheck_of_the_window() {
        // Why two seconds rather than a round number that felt safe. `PeerConfirm.tsx` re-asks
        // once a second whether the question it is showing is still waiting, and that poll is the
        // only thing that takes the Approve button away when a question ends with no answer. A
        // cool-off shorter than it would let a replacement be installed while the screen still
        // offered the *previous* question's buttons, which is the whole failure.
        //
        // The other side of this is a number in a different language that nothing can read from
        // here; `ASK_COOL_OFF`'s own documentation says so. This asserts the half that is
        // checkable: the margin over one poll is real rather than incidental.
        assert!(
            ASK_COOL_OFF >= Duration::from_millis(1000) * 2,
            "ASK_COOL_OFF is {ASK_COOL_OFF:?}, which no longer covers a whole RECHECK_MS cycle \
             and the round trip that answers it"
        );
        // And it stays far enough inside the caller's budget to be a beat rather than a refusal.
        assert!(ASK_COOL_OFF * 4 < ANSWER_DEADLINE);
    }

    #[test]
    fn the_answer_deadline_fits_inside_the_call_the_caller_is_making() {
        // Not a style rule. `send_to` wraps `authorize` — and so this wait — in a timeout of
        // `WIRE_DEADLINE`, after spending some of it hashing the file. A deadline at or past that
        // one never fires: the caller is cut off first, and what the agent is told is "the peer
        // had not been reached yet" rather than "nobody approved this". Read from the library
        // rather than copied, so upstream shortening it is caught here.
        assert!(
            ANSWER_DEADLINE < zyris_tools::WIRE_DEADLINE,
            "a question that outlives its caller's {:?} deadline can only expire unheard, but \
             ANSWER_DEADLINE is {ANSWER_DEADLINE:?}",
            zyris_tools::WIRE_DEADLINE,
        );
    }
}
