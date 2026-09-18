//! What is read aloud, and what is not.
//!
//! # Two texts, and why they are two types
//!
//! An answer on the screen and the same answer in the air are not the same text. The screen
//! gets the markdown, the URL, the path and the code block; the speaker gets a sentence a
//! person can follow without looking. **That difference is carried by the types, not by a
//! convention**: [`Filter::read`] hands back a [`Shown`] and an [`Aloud`] together, [`Aloud`]
//! has no public constructor, and [`crate::split::Splitter`] accepts nothing else. So a delta
//! cannot reach the voice without passing through here — not because somebody remembered to
//! call this, but because there is no other way to make the argument.
//!
//! It is also what makes stripping honest. Dropping a hundred-character URL out of the speech
//! would be the silent loss this project refuses everywhere else — `tts::Spoken::unvoiced`,
//! `ModelState`, the audit log — except that the screen still has it. **The trace is
//! [`Shown`]**, and that is the whole justification for rules 3 and 4 below.
//!
//! # The four rules, ordered by how badly each fails
//!
//! 1. **A [`Kind::Reasoning`] delta never reaches the speaker.** This is not text parsing: the
//!    protocol already says which half of a turn a delta belongs to, and reading a model's
//!    private working-out aloud is the loudest failure on this list. It is the first rule and
//!    it is decided before a character is looked at.
//! 2. **A code fence is [`FENCE_WORD`] and never its contents.** Read literally, a shell
//!    snippet is a minute of punctuation.
//! 3. **An aside in parentheses is skipped.** With a bound — see [`MAX_ASIDE`]; an unclosed
//!    bracket must not silence the rest of a turn.
//! 4. **A URL is [`LINK_WORD`], a path is [`PATH_WORD`], and markdown punctuation is
//!    dropped.** One word each, for the same reason the fence gets one: they are things a
//!    person reads and nobody says.
//!
//! Rules 2 and 4 replace rather than delete. `see https://example.com/a/b for the rest` becomes
//! `see link for the rest`, which is a sentence; deleting the token leaves `see for the rest`,
//! which is not.
//!
//! # What this module does not do
//!
//! The punctuation *substitutions* — dashes, quotes, `@`, `|`, `/` as a separator — belong to
//! [`crate::tts::normalise`], which is the reference implementation's own table and runs on
//! every fragment anyway. Doing them twice would be two spellings of one rule. What is here is
//! the part that is about **structure**: a fence, an aside, a token that is a URL rather than a
//! word, and the line-leading markers that make a list a list.
//!
//! # State that outlives one delta
//!
//! A fence opens in one delta and closes three deltas later; an aside opens mid-token; a URL
//! arrives as `https://exa`, `mple.com/x`. So [`Filter`] is fed delta by delta and keeps what
//! it has not resolved yet. [`Filter::finish`] is what releases it at the end of a turn, and a
//! caller that forgets it loses the last word.

/// Which half of a turn a delta belongs to.
///
/// **A mirror of `zyris_proto::ZDeltaKind`, deliberately, and not a re-export.** The same
/// accommodation [`crate::Push`] makes for `hotkey::HotkeyEvent`: this crate has two
/// dependencies in the off build and the protocol stack is not one of them, so the distinction
/// that decides rule 1 has to be nameable here. `zyris-voice`'s caller maps between them in one
/// line, which is the place a new arm in the protocol enum shows up as a compile error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The answer. This is what is spoken.
    Assistant,
    /// The model's own working-out. **Never spoken** — see rule 1.
    Reasoning,
}

/// The word a code fence is read as.
pub const FENCE_WORD: &str = "code";

/// The word a URL is read as.
pub const LINK_WORD: &str = "link";

/// The word a filesystem path is read as.
pub const PATH_WORD: &str = "path";

/// How long an aside may run before the opening bracket is treated as a mistake.
///
/// **Without a bound, one unmatched `(` silences everything after it** — for the rest of the
/// turn, with no error and nothing on the screen to say why. That is the exact failure this
/// repository has written down five times under a different name. So the text inside an aside
/// is *held* rather than thrown away, and if no `)` arrives within this many characters it is
/// released and spoken: a wrong aside costs a few words that should have been skipped, where a
/// wrong silence costs the answer.
///
/// 200 characters is about two sentences — longer than any parenthetical anybody writes, and
/// short enough that the delay before the words come out is under one fragment's worth.
pub const MAX_ASIDE: usize = 200;

/// Text as it should appear on a screen: the delta, unchanged.
///
/// It exists so that [`Aloud`] can be the *other* one. A single `String` crossing this module
/// would be a convention, and a convention is what this type is here instead of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown(String);

impl Shown {
    /// What to render.
    pub fn text(&self) -> &str {
        &self.0
    }

    /// Whether this delta puts anything on the screen.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Text as it should be read aloud.
///
/// **There is no public constructor and no `From<String>`.** The only way to get one is
/// [`Filter::read`] or [`Filter::finish`], which is what makes "the spoken text and the shown
/// text are separate" a property of the program rather than a rule somebody follows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Aloud(String);

impl Aloud {
    /// What to say.
    pub fn text(&self) -> &str {
        &self.0
    }

    /// Whether this delta left anything to say. A whole delta of reasoning, of code, or of one
    /// URL leaves nothing, and that is not a failure.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// One delta, as both of the things it is.
///
/// Returned together rather than separately so that a caller cannot take one and forget the
/// other: the screen needs every delta including the reasoning, and the speaker needs the
/// filtered remainder of some of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    /// The delta as the screen should have it.
    pub shown: Shown,
    /// What of it, if anything, is to be spoken.
    pub aloud: Aloud,
}

/// The filter, across a whole turn.
///
/// Fed one delta at a time; keeps whatever the delta left unresolved — an open fence, an open
/// aside, half a word that might turn out to be a URL. [`Filter::finish`] releases the rest.
#[derive(Debug, Default)]
pub struct Filter {
    /// Inside a ``` fence: everything is dropped and [`FENCE_WORD`] has already been said.
    fence: bool,
    /// Backticks seen in a row. Three toggle a fence; one or two are inline-code markers and
    /// are dropped.
    ticks: usize,
    /// Depth of open parentheses. Zero is "not in an aside".
    depth: usize,
    /// What the open aside has swallowed so far, in case it turns out never to close.
    aside: String,
    /// The token being read, held until whitespace so that it can be recognised as a URL or a
    /// path — both of which are only recognisable whole.
    word: String,
    /// Whether the next character begins a line, which is the only place `#`, `>` and a bullet
    /// mean anything.
    line_start: bool,
    /// Whether a word has been emitted with no separator after it yet.
    ///
    /// **Across deltas, not within one.** A delta routinely ends mid-token, so the word before
    /// it is emitted when the *next* delta arrives — and two `Aloud`s concatenated by a caller
    /// must not fuse `All` and `done` into one word. The output of one delta cannot carry that
    /// state; the filter can.
    owes_space: bool,
}

impl Filter {
    /// A filter at the start of a turn.
    pub fn new() -> Filter {
        Filter { line_start: true, ..Filter::default() }
    }

    /// Read one delta.
    ///
    /// Rule 1 is here and is the first thing that happens: a [`Kind::Reasoning`] delta is shown
    /// and **not looked at**. Not "looked at and found to contain nothing speakable" — the
    /// filter's state is not advanced by it either, because a fence opened inside the model's
    /// working-out has nothing to do with the fences in its answer.
    pub fn read(&mut self, kind: Kind, text: &str) -> Reading {
        let shown = Shown(text.to_string());
        if kind == Kind::Reasoning {
            return Reading { shown, aloud: Aloud::default() };
        }
        let mut out = String::with_capacity(text.len());
        for c in text.chars() {
            self.feed(c, &mut out);
        }
        Reading { shown, aloud: Aloud(out) }
    }

    /// Release everything still held, at the end of a turn.
    ///
    /// The last word of an answer is in [`Filter::word`] until a space arrives after it, and
    /// the last delta of a turn does not come with one. An aside still open at the end never
    /// closed, so it is spoken by [`MAX_ASIDE`]'s argument.
    pub fn finish(&mut self) -> Aloud {
        let mut out = String::new();
        if self.depth > 0 {
            let aside = std::mem::take(&mut self.aside);
            self.emit(&mut out, &aside);
            self.depth = 0;
        }
        self.flush_word(&mut out);
        self.fence = false;
        self.ticks = 0;
        self.line_start = true;
        Aloud(out)
    }

    /// Release the word being held, **without ending the turn**.
    ///
    /// What [`crate::split::IDLE_FLUSH`] needs, and it is not [`Filter::finish`]: a stream that
    /// has merely stalled may still be inside a fence or an aside, and finishing would clear
    /// both — the rest of a code block would then be read aloud because the answer paused in the
    /// middle of it.
    ///
    /// Nothing a fence or an aside is holding can escape through here, and that is structural
    /// rather than checked: a character inside a fence is dropped and a character inside an
    /// aside goes to [`Filter::aside`], so [`Filter::word`] is non-empty only outside both.
    ///
    /// **A stall in the middle of a word says the half that arrived**, and the other half begins
    /// the next fragment. That is the same split the fragment boundary already is, and it is the
    /// price of not dropping the last word of every stalled sentence — a delta ends without
    /// trailing whitespace almost every time, so without this the word before the stall is
    /// *always* the one left behind.
    /// Whether a word is being held that [`Filter::pause`] would release. What decides whether
    /// an idle stream has anything to say at all.
    pub fn holds_a_word(&self) -> bool {
        !self.word.is_empty()
    }

    pub fn pause(&mut self) -> Aloud {
        let mut out = String::new();
        self.flush_word(&mut out);
        Aloud(out)
    }

    /// One character.
    fn feed(&mut self, c: char, out: &mut String) {
        // Backticks first: three of them change what every later character means.
        if c == '`' {
            self.ticks += 1;
            if self.ticks == 3 {
                self.ticks = 0;
                self.flush_word(out);
                self.fence = !self.fence;
                if self.fence {
                    self.emit(out, FENCE_WORD);
                }
            }
            return;
        }
        // One or two backticks were an inline-code marker. They are dropped, and the character
        // that ended the run is still to be dealt with.
        self.ticks = 0;
        if self.fence {
            return;
        }
        if self.depth > 0 {
            self.in_aside(c, out);
            return;
        }
        match c {
            '(' => {
                self.flush_word(out);
                self.depth = 1;
                self.aside.clear();
            }
            // Emphasis, strikethrough, and the brackets around a link's label. Dropped wherever
            // they are: `**done**` is one word whether or not it began a line, and `[label]` is
            // its label. The target that follows in `(…)` is rule 3's problem, which is why a
            // markdown link needs no rule of its own.
            '*' | '~' | '[' | ']' => {}
            // A line-leading marker is structure; the same character inside a line is not.
            // `#` mid-sentence is a number sign and `-` mid-word is a hyphen.
            '#' | '>' | '-' | '+' if self.line_start && self.word.is_empty() => {}
            c if c.is_whitespace() => {
                self.flush_word(out);
                if !out.ends_with(' ') {
                    out.push(' ');
                }
                self.owes_space = false;
                self.line_start = c == '\n' || c == '\r';
            }
            c => {
                self.word.push(c);
                self.line_start = false;
            }
        }
    }

    /// One character, inside an aside.
    fn in_aside(&mut self, c: char, out: &mut String) {
        match c {
            '(' => {
                self.depth += 1;
                self.aside.push(c);
            }
            ')' => {
                self.depth -= 1;
                if self.depth == 0 {
                    // The aside closed, so it was an aside. Rule 3.
                    self.aside.clear();
                    // A boundary, so the words either side do not fuse.
                    if !out.is_empty() && !out.ends_with(' ') {
                        out.push(' ');
                    }
                    self.owes_space = false;
                } else {
                    self.aside.push(c);
                }
            }
            c => self.aside.push(c),
        }
        if self.aside.chars().count() > MAX_ASIDE {
            let aside = std::mem::take(&mut self.aside);
            self.emit(out, &aside);
            self.depth = 0;
        }
    }

    /// The token just ended: decide what it is, and say that.
    fn flush_word(&mut self, out: &mut String) {
        if self.word.is_empty() {
            return;
        }
        let word = std::mem::take(&mut self.word);
        let (core, tail) = split_tail(&word);
        let said = if core.is_empty() {
            core
        } else if is_url(core) {
            LINK_WORD
        } else if is_path(core) {
            PATH_WORD
        } else {
            core
        };
        if said.is_empty() && tail.is_empty() {
            return;
        }
        self.emit(out, said);
        out.push_str(tail);
    }

    /// Append a word, separated from whatever came before it — **including what came before it
    /// in an earlier delta**, which is what [`Filter::owes_space`] is for.
    fn emit(&mut self, out: &mut String, word: &str) {
        if self.owes_space && !out.ends_with(' ') {
            out.push(' ');
        }
        out.push_str(word);
        self.owes_space = true;
    }
}

/// Split the sentence punctuation off the end of a token.
///
/// Without it `see https://example.com/a.` classifies as a URL **including the full stop**, and
/// the sentence loses the thing the splitter cuts on. The characters here are exactly the ones
/// `split::ends_a_clause` and `tts::ends_a_sentence` read.
fn split_tail(word: &str) -> (&str, &str) {
    let end = word
        .char_indices()
        .rev()
        .take_while(|(_, c)| matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '"' | '\'' | ']' | '}'))
        .last()
        .map(|(i, _)| i)
        .unwrap_or(word.len());
    word.split_at(end)
}

/// Whether a whole token is a URL.
///
/// A scheme, or the `www.` everybody writes without one. Deliberately not "anything with a
/// dot in it": `1.5`, `e.g.` and `README.md` are not links.
pub fn is_url(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    lower.contains("://") || lower.starts_with("www.") || lower.starts_with("mailto:")
}

/// Whether a whole token is a filesystem path.
///
/// Checked **after** [`is_url`], because every URL would otherwise answer yes here.
///
/// The rule: it has a separator in it, **and** either it is rooted (`/`, `./`, `../`, `~/`, a
/// Windows drive) or its last segment has an extension. That second half is what makes
/// `crates/zyris-voice/src/speak.rs` a path while `and/or`, `24/7` and `read/write` are three
/// ordinary words — and those are common enough in an answer that a rule saying "a slash means
/// a path" would mispronounce a sentence a day.
pub fn is_path(word: &str) -> bool {
    if !word.contains('/') && !word.contains('\\') {
        return false;
    }
    let rooted = word.starts_with('/')
        || word.starts_with("./")
        || word.starts_with("../")
        || word.starts_with("~/")
        || word.starts_with('\\')
        || word
            .as_bytes()
            .first()
            .is_some_and(|c| c.is_ascii_alphabetic() && word.len() > 2 && &word[1..3] == ":\\");
    let last = word.rsplit(['/', '\\']).next().unwrap_or("");
    rooted || last.contains('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aloud(deltas: &[(Kind, &str)]) -> String {
        let mut filter = Filter::new();
        let mut said = String::new();
        for &(kind, text) in deltas {
            said.push_str(filter.read(kind, text).aloud.text());
        }
        said.push_str(filter.finish().text());
        said.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn assistant(text: &str) -> String {
        aloud(&[(Kind::Assistant, text)])
    }

    /// A stalled stream. The last word of a delta has no whitespace after it, so it is *always*
    /// the one still held — a pause that did not release it would leave every stalled sentence
    /// short of its last word, and put that word at the front of whatever came next.
    #[test]
    fn a_pause_releases_the_word_a_delta_ended_on() {
        let mut filter = Filter::new();
        assert_eq!(filter.read(Kind::Assistant, "One moment").aloud.text(), "One ");
        assert_eq!(filter.pause().text(), "moment");
    }

    /// And it is not [`Filter::finish`]. A stall inside a code block must not open the block up
    /// — the rest of it would then be read out, line by line, as the answer resumed.
    #[test]
    fn a_pause_inside_a_fence_says_nothing_and_leaves_the_fence_shut() {
        let mut filter = Filter::new();
        filter.read(Kind::Assistant, "Run ```cargo build");
        assert_eq!(filter.pause().text(), "", "a fence holds nothing a pause can release");
        assert_eq!(
            filter.read(Kind::Assistant, " --release\n").aloud.text(),
            "",
            "and the fence is still open"
        );
    }

    /// The same for an aside, which may still close.
    #[test]
    fn a_pause_inside_an_aside_leaves_it_open() {
        let mut filter = Filter::new();
        assert_eq!(filter.read(Kind::Assistant, "Done (nearly").aloud.text(), "Done ");
        assert_eq!(filter.pause().text(), "");
        assert_eq!(filter.read(Kind::Assistant, ") now. ").aloud.text(), " now. ");
    }

    /// **Rule 1, and the reason it is rule 1.**
    ///
    /// The protocol says which half of a turn a delta is; nothing in the text does. A filter
    /// that only read text would have no way to tell a model's private working-out from its
    /// answer, and would read it out — confidently, at length, and in the first person.
    #[test]
    fn reasoning_never_reaches_the_speaker() {
        let mut filter = Filter::new();
        let thinking = filter.read(Kind::Reasoning, "The user wants the file moved. I should ");
        assert_eq!(thinking.aloud.text(), "", "not one character of it is spoken");
        assert_eq!(
            thinking.shown.text(),
            "The user wants the file moved. I should ",
            "and every character of it is still on the screen"
        );

        let answer = filter.read(Kind::Assistant, "Moved. ");
        assert_eq!(answer.aloud.text().trim(), "Moved.", "and the answer after it still is");
    }

    /// Reasoning does not move the filter's state either.
    ///
    /// A fence opened inside the working-out is not a fence in the answer, and if it were
    /// allowed to toggle the flag the *answer* after it would go silent — a rule-1 miss that
    /// looks like a rule-2 bug.
    #[test]
    fn a_fence_inside_reasoning_does_not_silence_the_answer() {
        let said = aloud(&[
            (Kind::Reasoning, "let me try ```sh\nls\n"),
            (Kind::Assistant, "Here it is."),
        ]);
        assert_eq!(said, "Here it is.");
    }

    /// **Rule 2.** The fence is one word, whatever is in it.
    #[test]
    fn a_code_fence_is_one_word() {
        assert_eq!(
            assistant("Run this:\n```sh\nrm -rf /tmp/x && echo $?\n```\nThen check."),
            "Run this: code Then check."
        );
    }

    /// A fence that opens in one delta and closes in another is still one fence.
    ///
    /// The deltas arrive at whatever size the model emits them, and the three backticks are
    /// routinely split across two. Without the run counter the fence never opens, and the
    /// whole snippet is read out character by character.
    #[test]
    fn a_fence_split_across_deltas_is_still_a_fence() {
        let said = aloud(&[
            (Kind::Assistant, "Run ``"),
            (Kind::Assistant, "`sh\nls -la"),
            (Kind::Assistant, " /tmp\n``"),
            (Kind::Assistant, "` and look."),
        ]);
        assert_eq!(said, "Run code and look.");
    }

    /// Inline backticks are markers, not a fence: the word inside them is still spoken.
    #[test]
    fn inline_code_keeps_its_word() {
        assert_eq!(assistant("The `cargo` command works."), "The cargo command works.");
    }

    /// **Rule 3.** An aside is skipped, and the words either side do not fuse.
    #[test]
    fn an_aside_is_skipped() {
        assert_eq!(
            assistant("The build (about ninety seconds) finished."),
            "The build finished."
        );
        assert_eq!(assistant("Nested (a (b) c) here."), "Nested here.");
    }

    /// An aside that never closes is **spoken**, not left to silence the turn.
    ///
    /// The bound is what stops one stray `(` from muting everything after it. A wrong aside
    /// costs a few words that should have been skipped; a wrong silence costs the answer, and
    /// nothing on the screen says why.
    #[test]
    fn an_unclosed_aside_is_released_rather_than_silencing_the_rest() {
        let long: String = std::iter::repeat_n("word ", 60).collect();
        let said = assistant(&format!("Start (an aside {long} and the end."));
        assert!(said.starts_with("Start"), "{said}");
        assert!(said.contains("the end."), "everything after the bracket is still spoken: {said}");

        // And at the end of a turn, an aside shorter than the bound is released too, because
        // the bracket that would have closed it is never coming.
        assert_eq!(assistant("Done (almost"), "Done almost");
    }

    /// **And the bound releases mid-turn, not when the turn ends.**
    ///
    /// [`Filter::finish`] releases an open aside too, so the test above passes whether or not
    /// [`MAX_ASIDE`] exists — it survived a mutation that deleted the bound outright. The half
    /// that matters is this one: a turn runs for a minute, and a stray `(` in its first
    /// sentence must not take the rest of it.
    #[test]
    fn the_aside_bound_does_not_wait_for_the_end_of_the_turn() {
        let mut filter = Filter::new();
        let long: String = std::iter::repeat_n("word ", 60).collect();
        let mut said = String::new();
        said.push_str(filter.read(Kind::Assistant, &format!("Start (an aside {long}")).aloud.text());
        said.push_str(filter.read(Kind::Assistant, "and the end. ").aloud.text());
        assert!(said.contains("the end."), "the turn is not over and it is still held: {said:?}");
    }

    /// A word flushed by something that is not a space still gets a separator **after the delta
    /// it was in has already gone out**.
    ///
    /// The bracket ends the token, so `Done` is emitted with nothing after it and the delta
    /// ends there; the aside comes out of the next call. Nothing in that second string knows a
    /// word came before it, so the filter has to. Without it the two fuse into `Donealmost`,
    /// which is a word the voice will happily say.
    #[test]
    fn a_word_and_what_follows_it_do_not_fuse_across_a_delta() {
        let mut filter = Filter::new();
        let first = filter.read(Kind::Assistant, "Done(almost").aloud;
        assert_eq!(first.text(), "Done", "the bracket flushed the word and nothing followed it");
        let rest = filter.finish();
        assert_eq!(format!("{}{}", first.text(), rest.text()), "Done almost");
    }

    /// **Rule 4.** A URL is one word and the sentence around it survives.
    #[test]
    fn a_url_is_one_word() {
        assert_eq!(
            assistant("See https://example.com/a/b?c=d for the rest."),
            "See link for the rest."
        );
        assert_eq!(assistant("It is at www.example.com."), "It is at link.");
    }

    /// The full stop after a URL is not part of the URL.
    ///
    /// Without splitting the tail off the token, `https://example.com/a.` classifies whole and
    /// the sentence loses the character the splitter cuts on — so two sentences become one
    /// fragment and the pause between them disappears.
    #[test]
    fn the_stop_after_a_url_survives_it() {
        let said = assistant("Go to https://example.com/a. Then wait.");
        assert_eq!(said, "Go to link. Then wait.");
    }

    /// A path is one word, and three things that look like one are not.
    #[test]
    fn a_path_is_one_word_and_an_ordinary_word_is_not() {
        assert_eq!(assistant("It is in /usr/local/share now."), "It is in path now.");
        assert_eq!(assistant("Open crates/zyris-voice/src/speak.rs here."), "Open path here.");
        assert_eq!(assistant("Use C:\\Users\\me\\zyris now."), "Use path now.");

        // The half of the rule that decides it. A slash alone is not a path.
        assert_eq!(assistant("read/write and/or 24/7 access."), "read/write and/or 24/7 access.");
    }

    /// Markdown structure is dropped and the words it decorated are kept.
    #[test]
    fn markdown_punctuation_is_dropped_and_the_words_are_not() {
        assert_eq!(assistant("**Done.** It *worked* well."), "Done. It worked well.");
        assert_eq!(assistant("# Heading\n- one\n- two\n"), "Heading one two");
        assert_eq!(assistant("> quoted\n"), "quoted");
        // A hyphen inside a line is a hyphen, not a bullet.
        assert_eq!(assistant("A well-known case."), "A well-known case.");
    }

    /// A markdown link falls out of rules 3 and 4 together: the label is spoken and the target
    /// is an aside. Nothing in this module is written for it specially, which is why there is
    /// a test — it is the arrangement that would break silently.
    #[test]
    fn a_markdown_link_reads_as_its_label() {
        assert_eq!(
            assistant("See [the manual](https://example.com/manual) for more."),
            "See the manual for more."
        );
    }

    /// A word split across two deltas is one word, and is classified whole.
    ///
    /// Deltas break wherever the model's tokeniser did. Classifying the halves separately reads
    /// out `https` and then `example.com/manual`, which is worse than either answer.
    #[test]
    fn a_url_split_across_deltas_is_still_one_url() {
        let said = aloud(&[
            (Kind::Assistant, "Go to https://exa"),
            (Kind::Assistant, "mple.com/x now."),
        ]);
        assert_eq!(said, "Go to link now.");
    }

    /// The last word of a turn arrives with no space after it, so something has to release it.
    #[test]
    fn finish_releases_the_last_word() {
        let mut filter = Filter::new();
        let mid = filter.read(Kind::Assistant, "All done");
        assert_eq!(mid.aloud.text().trim(), "All", "the last token is still being read");
        assert_eq!(filter.finish().text().trim(), "done");
    }

    /// A delta with nothing speakable in it is empty, and that is not a failure.
    #[test]
    fn a_delta_of_nothing_but_a_fence_says_nothing() {
        let mut filter = Filter::new();
        filter.read(Kind::Assistant, "```rust\n");
        let inside = filter.read(Kind::Assistant, "fn main() {}\n");
        assert!(inside.aloud.is_empty(), "{:?}", inside.aloud);
        assert!(!inside.shown.is_empty(), "the screen still has it");
    }

    /// `Aloud` cannot be built from a `String`, and that is the point of it.
    ///
    /// This test is the compile-time property written down: the only constructors are
    /// [`Filter::read`] and [`Filter::finish`], so text that has not been through the filter
    /// has no way into [`crate::split::Splitter`]. If a `From<String>` is ever added, this
    /// comment is what says why it must not be.
    #[test]
    fn the_only_way_to_make_something_to_say_is_to_filter_it() {
        let mut filter = Filter::new();
        let aloud = filter.read(Kind::Assistant, "hello").aloud;
        assert_eq!(std::mem::size_of_val(&aloud), std::mem::size_of::<String>());
        assert_eq!(Aloud::default().text(), "");
    }
}
