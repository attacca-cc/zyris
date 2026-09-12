//! Autostart on Windows: a Task Scheduler entry.
//!
//! One task named `Zyris`, registered from an XML document through `schtasks`. A value under
//! `SOFTWARE\Microsoft\Windows\CurrentVersion\Run` — what `tauri-plugin-autostart` writes —
//! would have been one registry call, and it starts the program the instant the shell comes up,
//! with no second chance if that fails. The whole reason the spec picked Task Scheduler is the
//! two elements a Run key has no room for: a thirty-second delay before the first try, and three
//! retries a minute apart after a failure.
//!
//! Nothing here remembers anything. Every question is answered by asking Task Scheduler again,
//! because a person can delete or switch off the task in the Task Scheduler window without
//! telling Zyris.
//!
//! **Nothing here reads what `schtasks` says.** Its messages are localized — on the Korean
//! Windows 11 machine this was written against, the text after `ERROR:` came back in Korean and
//! `/query /fo list` printed Korean field names. Exit codes are the same everywhere, and the one
//! place a decision turns on output is the task document read back by `/query /xml`, whose
//! element names come from a schema rather than from a translation.

use std::ffi::OsStr;
use std::io::ErrorKind;
use std::os::windows::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Output};

use anyhow::Context as _;

use crate::{Backend, State};

/// The task's name, which is also what a person reads in the Task Scheduler window.
///
/// Worth keeping recognisable. Somebody scrolling a list of scheduled tasks decides what to
/// delete from the name alone, and a GUID is an invitation to delete it.
const TASK: &str = "Zyris";

/// Said the same way wherever it comes up, because it is the one case a person can act on.
const NO_SCHTASKS: &str =
    "this machine has no `schtasks`, so there is no Task Scheduler to start Zyris from";

/// `CREATE_NO_WINDOW`.
///
/// Without it every call here flashes a console window in the middle of the screen, because
/// `schtasks` is a console program and Zyris is not. The switch in the Settings window makes
/// three of these calls; three black rectangles is how a working feature looks broken.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Autostart through a Task Scheduler entry.
pub(crate) struct TaskScheduler;

impl Backend for TaskScheduler {
    fn state(&self) -> anyhow::Result<State> {
        let output = match run("schtasks", &["/query", "/tn", TASK]) {
            Ok(output) => output,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(State::Unsupported(NO_SCHTASKS.to_owned()));
            }
            Err(error) => return Err(error).context("could not run `schtasks /query`"),
        };

        // **The exit code, never the word.** Measured on Windows 11: `/query /tn <name>` exits 0
        // when the task is registered and 1 when it is not. A non-zero exit for some third
        // reason would read as "not there" here, and there is no way around that — the reason is
        // only ever in the localized line we are refusing to parse, and inventing an error out
        // of an exit code we cannot name would turn a working switch into a broken one on a
        // machine we have not seen.
        if !output.status.success() {
            return Ok(State::Disabled);
        }

        // The task exists, which is not the same as it being on. Someone can right-click it in
        // the Task Scheduler window and choose Disable, and after that `/query` still exits 0
        // while nothing starts at logon — measured. A switch that reads on while the machine
        // does nothing is the one failure this crate exists to avoid, so ask the document.
        //
        // A read that fails leaves the task exactly as present as the exit code already said, so
        // the answer falls back to what `/query` alone knew rather than to an error.
        match read_back() {
            Ok(document) if turned_off(&document) => Ok(State::Disabled),
            Ok(_) => Ok(State::Enabled),
            Err(error) => {
                tracing::debug!(%error, "could not read the {TASK} task back; assuming it is on");
                Ok(State::Enabled)
            }
        }
    }

    fn enable(&self, exe: &Path) -> anyhow::Result<()> {
        // A temporary directory rather than a temporary file: `schtasks` opens the document by
        // path in another process, and a directory that deletes itself leaves nothing behind
        // either way. A `zyris-task.xml` sitting in somebody's home folder afterwards is litter
        // that looks like a file they are supposed to keep.
        let scratch = tempfile::Builder::new()
            .prefix("zyris-autostart-")
            .tempdir()
            .context("could not make a temporary directory for the task document")?;
        let document = scratch.path().join("zyris-task.xml");
        std::fs::write(&document, encode_task(&render_task(exe)))
            .with_context(|| format!("could not write {}", document.display()))?;

        // `/f` overwrites an entry that is already there, which is what makes turning the switch
        // on twice — or on after somebody edited the task by hand — end in the document this
        // crate wrote rather than in an error or in whatever was there before.
        let output = run(
            "schtasks",
            &[
                OsStr::new("/create"),
                OsStr::new("/tn"),
                OsStr::new(TASK),
                OsStr::new("/xml"),
                document.as_os_str(),
                OsStr::new("/f"),
            ],
        );
        let output = match output {
            Ok(output) => output,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                anyhow::bail!("cannot turn autostart on: {NO_SCHTASKS}");
            }
            Err(error) => return Err(error).context("could not run `schtasks /create`"),
        };

        if !output.status.success() {
            anyhow::bail!("could not register the {TASK} task: {}", complaint(&output));
        }

        tracing::info!(
            task = TASK,
            executable = %exe.display(),
            "registered the Task Scheduler entry",
        );

        Ok(())
    }

    fn disable(&self) -> anyhow::Result<()> {
        let present = match run("schtasks", &["/query", "/tn", TASK]) {
            Ok(output) => output.status.success(),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                anyhow::bail!("cannot turn autostart off: {NO_SCHTASKS}");
            }
            Err(error) => return Err(error).context("could not run `schtasks /query`"),
        };

        // Nothing to remove is the job already done. `schtasks /delete` on a task that is not
        // there exits non-zero, and turning off something that is already off must not fail.
        if !present {
            return Ok(());
        }

        let output = run("schtasks", &["/delete", "/tn", TASK, "/f"])
            .context("could not run `schtasks /delete`")?;
        if !output.status.success() {
            anyhow::bail!("could not remove the {TASK} task: {}", complaint(&output));
        }

        tracing::info!(task = TASK, "removed the Task Scheduler entry");

        Ok(())
    }

    fn mechanism(&self) -> Option<String> {
        // Built from `TASK` rather than written out, so the name a person is told to look for
        // in the Task Scheduler window cannot drift away from the one registered here.
        Some(format!("a Task Scheduler entry named {TASK}"))
    }

    // `caveats` is deliberately the default. Linux has one because a systemd unit can be enabled
    // while the user does not linger, which leaves autostart on and useless after logout. A
    // registered task has no second switch like that: it either exists and is on — and `state`
    // above checks both — or it does not. The battery settings that could have quietly stopped
    // it are written `false` in the document, so they are not a caveat either.
}

/// The task document, as a string.
///
/// Pure, so what goes into it can be checked without a Task Scheduler to check it against —
/// which matters more here than on Linux, because this file is not even compiled on the machine
/// it is written on.
///
/// This exact shape was registered on a real Windows 11 machine and read back with
/// `/query /xml ONE`: the `Delay` and the `RestartOnFailure` both survive, which is the whole
/// reason the spec picked Task Scheduler over a Run key. `LeastPrivilege` and the two `Enabled`
/// elements are defaults and do not come back in the read-back; that is them being defaults, not
/// them being ignored.
fn render_task(exe: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Keeps this computer connected to Attacca.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <Delay>PT30S</Delay>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>--minimized</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        command = escape(&exe.display().to_string()),
    )
}

/// The document as the bytes `schtasks` will accept: UTF-16, little-endian, **with a BOM**.
///
/// The BOM is not decoration. Measured: a UTF-16LE document without one is answered with
/// `ERROR: The task XML is malformed.`, and on a localized Windows the line after that is
/// localized too, so the message a person reports says nothing about encodings. It is the
/// mistake to expect, because the file looks right in every editor.
fn encode_task(xml: &str) -> Vec<u8> {
    const BOM: u16 = 0xFEFF;

    let mut bytes = Vec::with_capacity((xml.len() + 1) * 2);
    for unit in std::iter::once(BOM).chain(xml.encode_utf16()) {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }

    bytes
}

/// XML text content, with the three characters that would otherwise end the element escaped.
///
/// `C:\Users\R&D\zyris.exe` is a legal Windows path, and pasted into an element unescaped it is
/// a malformed document — answered with the same localized "malformed" line as a missing BOM,
/// which says nothing about paths. A directory called `R&D` is not a strange thing for somebody
/// to have.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());

    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(character),
        }
    }

    escaped
}

/// The registered task as Task Scheduler hands it back.
fn read_back() -> anyhow::Result<String> {
    let output = run("schtasks", &["/query", "/tn", TASK, "/xml", "ONE"])
        .context("could not run `schtasks /query /xml`")?;

    if !output.status.success() {
        anyhow::bail!("`schtasks /query /xml` failed: {}", complaint(&output));
    }

    Ok(decode(&output.stdout))
}

/// Whether a read-back document describes a task that has been switched off.
///
/// `Enabled` is `true` by default and Task Scheduler omits it from the read-back when it is, so
/// the presence of a `false` one is the whole question — at the task level from the Disable item
/// in the right-click menu, or on the trigger from the same menu one level down. Both mean
/// nothing starts at logon, which is the only thing the switch is claiming.
///
/// Whitespace is dropped first so the answer does not depend on how the document was laid out.
fn turned_off(document: &str) -> bool {
    let compact: String = document.chars().filter(|c| !c.is_whitespace()).collect();

    compact.contains("<Enabled>false</Enabled>")
}

/// Text out of a child process, whichever of the two encodings it used.
///
/// `schtasks` is captured through a pipe here, and through a pipe it writes the active code page
/// — measured, and the reason this is not simply `String::from_utf16`. The BOM branch is for the
/// other half: the document Zyris hands it is UTF-16 with a BOM, `schtasks` is a program that
/// deals in UTF-16, and a build of Windows that hands one back would otherwise decode to
/// nothing recognisable without saying so. Everything looked for here is ASCII, which survives
/// either.
fn decode(bytes: &[u8]) -> String {
    if let [0xFF, 0xFE, rest @ ..] = bytes {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();

        return String::from_utf16_lossy(&units);
    }

    String::from_utf8_lossy(bytes).into_owned()
}

/// Run a console program without putting a console on the screen.
///
/// `Command::output` also gives the child no stdin, so nothing here can stop on a prompt nobody
/// is there to answer.
fn run<S: AsRef<OsStr>>(program: &str, args: &[S]) -> std::io::Result<Output> {
    Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
}

/// What a failed command said, for an error message a person reads.
///
/// Quoted, never parsed — most of this is translated on a localized machine, so no decision may
/// turn on its wording. Showing it is still worth more than hiding it: somebody can search for
/// the sentence in their own language.
fn complaint(output: &Output) -> String {
    let stderr = decode(&output.stderr);
    let stderr = stderr.trim();

    if !stderr.is_empty() {
        return stderr.to_owned();
    }

    // `schtasks` puts some of its failures on stdout rather than stderr.
    let stdout = decode(&output.stdout);
    let stdout = stdout.trim();

    if !stdout.is_empty() {
        return stdout.to_owned();
    }

    match output.status.code() {
        Some(code) => format!("it exited {code} and said nothing"),
        None => "it was killed by a signal".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_task_starts_a_zyris_somebody_can_still_open() {
        // `--minimized`, never `--headless`. A headless process takes the instance lock and
        // has no tray and no single-instance plugin, so on a machine with autostart on there
        // would be no window, no tray icon, and no way to get either: launching Zyris would
        // find the lock held and exit without a word. `--minimized` is a full GUI that happens
        // not to be on the screen, and a second launch reaches it.
        let xml = render_task(Path::new(r"C:\Program Files\Zyris\zyris.exe"));

        assert!(xml.contains(r"<Command>C:\Program Files\Zyris\zyris.exe</Command>"));
        assert!(xml.contains("<Arguments>--minimized</Arguments>"));
    }

    #[test]
    fn the_task_waits_before_starting_and_retries_after_failing() {
        // Why Task Scheduler rather than a Run key, in two elements.
        let xml = render_task(Path::new(r"C:\zyris.exe"));

        assert!(xml.contains("<Delay>PT30S</Delay>"));
        assert!(xml.contains("<Count>3</Count>"));
    }

    #[test]
    fn the_task_has_no_execution_time_limit() {
        // The default stops it after three days, and this is a daemon.
        assert!(
            render_task(Path::new(r"C:\zyris.exe"))
                .contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>")
        );
    }

    #[test]
    fn the_mechanism_names_the_entry_somebody_would_go_looking_for() {
        // The Settings screen prints this sentence and nothing else about how autostart works.
        // A person who wants to remove it by hand has to be able to find the task from it.
        let mechanism = TaskScheduler.mechanism().unwrap();

        assert!(mechanism.contains(TASK), "{mechanism}");
    }

    #[test]
    fn the_document_is_utf16_with_a_bom() {
        // Without the BOM `schtasks` answers "The task XML is malformed", and on a localized
        // Windows the detail after that is localized too.
        let bytes = encode_task(&render_task(Path::new(r"C:\zyris.exe")));

        assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
    }

    #[test]
    fn the_document_is_two_bytes_a_character_after_the_bom() {
        // A BOM in front of UTF-8 is the near miss worth failing on: the first two bytes are
        // right and every byte after them is wrong.
        let bytes = encode_task("<Task/>");

        assert_eq!(
            bytes,
            [0xFF, 0xFE, b'<', 0, b'T', 0, b'a', 0, b's', 0, b'k', 0, b'/', 0, b'>', 0],
        );
    }

    #[test]
    fn a_path_with_an_ampersand_does_not_break_the_document() {
        // `C:\Users\R&D\...` is a legal Windows path and a broken XML document.
        let xml = render_task(Path::new(r"C:\Users\R&D\zyris.exe"));

        assert!(xml.contains("R&amp;D"), "the path was not escaped: {xml}");
        assert!(!xml.contains("R&D"), "the raw ampersand survived: {xml}");
    }

    #[test]
    fn a_path_with_a_bracket_does_not_break_the_document() {
        // Rarer than the ampersand and exactly as fatal.
        let xml = render_task(Path::new(r"C:\<odd>\zyris.exe"));

        assert!(xml.contains(r"<Command>C:\&lt;odd&gt;\zyris.exe</Command>"), "{xml}");
    }

    #[test]
    fn a_task_somebody_switched_off_does_not_read_as_on() {
        // Trimmed from a real `/query /xml ONE` read-back of a task disabled from the Task
        // Scheduler window. `/query /tn` still exits 0 for this one, so the exit code alone
        // would have shown the switch as on while nothing started at logon.
        let read_back = r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Settings>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <Enabled>false</Enabled>
  </Settings>
</Task>"#;

        assert!(turned_off(read_back));
    }

    #[test]
    fn a_task_nobody_touched_reads_as_on() {
        // `Enabled` is `true` by default, so the read-back of a healthy task does not mention
        // it at all. Reading that absence as "off" would leave the switch stuck on the wrong
        // side for everybody.
        let read_back = r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure>
  </Settings>
</Task>"#;

        assert!(!turned_off(read_back));
        assert!(!turned_off(&render_task(Path::new(r"C:\zyris.exe"))));
    }

    #[test]
    fn the_read_back_is_understood_in_either_encoding() {
        // A console gives UTF-16 with a BOM and a pipe gives the code page. The same document
        // has to come out of both.
        let document = "<Task><Enabled>false</Enabled></Task>";

        assert!(turned_off(&decode(&encode_task(document))));
        assert!(turned_off(&decode(document.as_bytes())));
    }

    /// The whole round trip against this machine's real Task Scheduler.
    ///
    /// Ignored because it registers a task under the person's own account. Run it deliberately:
    ///
    /// ```text
    /// cargo test -p zyris-autostart -- --ignored --nocapture --exact windows::tests::the_round_trip
    /// ```
    ///
    /// It puts the machine back the way it found it, and refuses to start at all if a `Zyris`
    /// task is already registered rather than writing over somebody else's.
    ///
    /// Everything above this test checks a string. This is the only thing that can tell us
    /// `schtasks` accepts the document at all — the encoding, the schema and the elements the
    /// spec chose Task Scheduler for all fail here or nowhere.
    #[test]
    #[ignore = "registers a real Task Scheduler entry under this account"]
    fn the_round_trip() {
        let backend = TaskScheduler;

        assert_eq!(
            backend.state().unwrap(),
            State::Disabled,
            "a {TASK} task is already registered; this test will not write over it",
        );
        assert_eq!(query_exit_code(), Some(1), "`schtasks /query /tn {TASK}` before");

        // This machine's own test binary: a path that really exists, rather than an invented
        // one Task Scheduler would accept just as happily.
        let exe = std::env::current_exe().unwrap();
        backend.enable(&exe).unwrap();

        assert_eq!(query_exit_code(), Some(0), "`schtasks /query /tn {TASK}` while installed");
        assert_eq!(backend.state().unwrap(), State::Enabled);

        let registered = read_back().unwrap();
        println!("--- as Task Scheduler stored it ---\n{registered}\n--- end ---");
        assert!(
            registered.contains("<Delay>PT30S</Delay>"),
            "the delay did not survive registration: {registered}",
        );
        assert!(
            registered.contains("<Count>3</Count>"),
            "the retry did not survive registration: {registered}",
        );
        assert!(
            registered.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"),
            "the task would be stopped after three days: {registered}",
        );
        assert!(
            registered.contains(&exe.display().to_string()),
            "the executable did not survive registration: {registered}",
        );

        backend.disable().unwrap();

        assert_eq!(query_exit_code(), Some(1), "`schtasks /query /tn {TASK}` after removal");
        assert_eq!(backend.state().unwrap(), State::Disabled);

        // Turning off what is already off is the job already done, not a failure.
        backend.disable().unwrap();
    }

    /// What `schtasks /query /tn Zyris` exits with, which is the first half of [`state`].
    fn query_exit_code() -> Option<i32> {
        run("schtasks", &["/query", "/tn", TASK]).unwrap().status.code()
    }
}
