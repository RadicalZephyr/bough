//! Building the oracle with GHC, and the pool of processes that answer.
//!
//! The oracle is `haskell/Oracle.hs`, built once per process into a
//! directory the caller gives, under a lock file so that two test binaries
//! do not build at once. GHC's own recompilation check makes a later build
//! fast. Each process reads one program per line and answers one line per
//! program; the pool keeps idle processes behind a mutex, so parallel test
//! threads never share one, and replaces a process that died.

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use crate::answer::{Answer, MalformedAnswer};
use crate::program::Program;

/// What to install, printed when GHC is missing.
pub const INSTALL_HINT: &str = "install GHC and HUnit with `apt-get install ghc libghc-hunit-dev`, \
     or GHC through ghcup (https://www.haskell.org/ghcup/) and then `cabal install --lib HUnit`";

/// The name of the oracle binary in the build directory.
const ORACLE: &str = "bough-oracle";

/// When the build lock counts as abandoned, and how long a build waits for
/// it. A build of the oracle takes seconds; the lock is touched throughout.
const BUILD_LOCK: LockTiming = LockTiming {
    heartbeat: Duration::from_secs(2),
    stale: Duration::from_secs(30),
    patience: Duration::from_secs(10 * 60),
    poll: Duration::from_millis(100),
};

/// How long a pool waits for an answer before it kills the process. The
/// process answers `TIMEOUT` after five seconds on its own; this catches one
/// that stopped answering altogether.
const DEFAULT_WATCHDOG: Duration = Duration::from_secs(30);

/// How much of a process's standard error an error keeps.
const STANDARD_ERROR_TAIL: usize = 8 * 1024;

/// The directory of the Haskell sources: the vendored semantics, `Oracle.hs`
/// and the modules it imports, `OracleTests.hs`.
pub fn haskell_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("haskell")
}

/// The GHC to run: the `BOUGH_GHC` environment variable when it is set,
/// otherwise `ghc` on the `PATH`.
pub fn ghc_command() -> OsString {
    env::var_os("BOUGH_GHC")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsString::from("ghc"))
}

/// Something that went wrong building or running the oracle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// GHC could not be run: it is not installed, or `BOUGH_GHC` names
    /// nothing.
    GhcMissing(String),
    /// The build failed: GHC's output, or why GHC could not start.
    Build(String),
    /// An oracle process could not be started or spoken to.
    Process(String),
    /// A program's text holds a line break, which the protocol cannot carry.
    NotOneLine,
    /// The process exited while it was answering the program. The pool
    /// starts a new process for the next program.
    Died {
        /// The program the process was answering.
        program: String,
        /// How the process exited.
        status: String,
        /// The end of the process's standard error.
        standard_error: String,
        /// What the process wrote of its answer, when it exited in the middle
        /// of the line; empty otherwise.
        unfinished_answer: String,
    },
    /// No answer came within the watchdog's limit, so the process was
    /// killed. The pool starts a new process for the next program.
    NoAnswer {
        /// The program the process was answering.
        program: String,
        /// How long the pool waited.
        waited: Duration,
        /// The end of the process's standard error.
        standard_error: String,
    },
    /// The answer did not follow the protocol. The process is stopped, since
    /// it may be out of step, and the pool starts a new one for the next
    /// program.
    Malformed {
        /// The program the process was answering.
        program: String,
        /// The answer, and what is wrong with it.
        answer: MalformedAnswer,
        /// The end of the process's standard error.
        standard_error: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::GhcMissing(reason) => write!(formatter, "{reason}; {INSTALL_HINT}"),
            Error::Build(message) => write!(formatter, "building with GHC failed: {message}"),
            Error::Process(message) => formatter.write_str(message),
            Error::NotOneLine => formatter.write_str("a program must print as one line"),
            Error::Died {
                program,
                status,
                standard_error,
                unfinished_answer,
            } => {
                write!(
                    formatter,
                    "the oracle process exited ({status}) while answering this program, \
                     and a new process will answer the next one\nprogram: {}\nstandard error: {}",
                    shorten(program),
                    standard_error.trim_end()
                )?;
                if unfinished_answer.is_empty() {
                    Ok(())
                } else {
                    write!(
                        formatter,
                        "\nunfinished answer: {}",
                        shorten(unfinished_answer)
                    )
                }
            }
            Error::NoAnswer {
                program,
                waited,
                standard_error,
            } => write!(
                formatter,
                "the oracle process gave no answer in {waited:?} and was killed, \
                 and a new process will answer the next one\nprogram: {}\nstandard error: {}",
                shorten(program),
                standard_error.trim_end()
            ),
            Error::Malformed {
                program,
                answer,
                standard_error,
            } => write!(
                formatter,
                "{answer}; the process was stopped, and a new process will answer the next one\n\
                 program: {}\nstandard error: {}",
                shorten(program),
                standard_error.trim_end()
            ),
        }
    }
}

impl std::error::Error for Error {}

/// The start of a long program, for messages.
fn shorten(program: &str) -> String {
    const LIMIT: usize = 2000;
    if program.len() <= LIMIT {
        program.to_owned()
    } else {
        let cut = (0..=LIMIT)
            .rev()
            .find(|index| program.is_char_boundary(*index))
            .unwrap_or(0);
        format!("{}… ({} bytes in all)", &program[..cut], program.len())
    }
}

// ----- the decision a test makes -----

/// What a test that needs GHC does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Build the oracle and run.
    Run,
    /// Say that the test skipped, and return.
    Skip,
    /// Panic with this message.
    Fail(String),
}

/// Decides what a test that needs GHC does, from the value of the
/// `BOUGH_ORACLE` environment variable and, unless it says to skip, from
/// `probe`, which runs GHC and returns its version or why it could not.
///
/// `BOUGH_ORACLE=skip` skips; any other value is a mistake, not a way to run.
/// Without the variable a test runs when GHC answers, and fails with the
/// install hint when it does not.
pub fn plan(setting: Option<&str>, probe: impl FnOnce() -> Result<String, String>) -> Plan {
    match setting {
        Some("skip") => Plan::Skip,
        Some(other) if !other.is_empty() => Plan::Fail(format!(
            "BOUGH_ORACLE is `{other}`, and the one value it takes is `skip`"
        )),
        _ => match probe() {
            Ok(_version) => Plan::Run,
            Err(reason) => Plan::Fail(format!(
                "this test needs GHC, and {reason}. To run it, {INSTALL_HINT}. \
                 To skip every test that needs GHC, set BOUGH_ORACLE=skip."
            )),
        },
    }
}

/// Runs `ghc --numeric-version`, once per process.
fn probe_ghc() -> Result<String, String> {
    static PROBE: OnceLock<Result<String, String>> = OnceLock::new();
    PROBE
        .get_or_init(|| {
            let ghc = ghc_command();
            let shown = ghc.to_string_lossy().into_owned();
            match Command::new(&ghc).arg("--numeric-version").output() {
                Ok(output) if output.status.success() => {
                    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
                }
                Ok(output) => Err(format!(
                    "`{shown} --numeric-version` failed ({})",
                    output.status
                )),
                Err(error) => Err(format!("`{shown}` could not be run ({error})")),
            }
        })
        .clone()
}

/// The oracle every test in a test binary shares, built into `directory`
/// on first use (later calls share it, whatever directory they name);
/// `None` when the test should skip, after saying so on the standard error
/// with the name of the test.
///
/// Panics with the install hint when GHC is missing and `BOUGH_ORACLE=skip`
/// is not set, and with GHC's output when the build fails. A test that
/// needs GHC starts with `let Some(oracle) = for_tests(directory) else {
/// return };`, with `directory` under `CARGO_TARGET_TMPDIR`.
#[track_caller]
pub fn for_tests(directory: impl AsRef<Path>) -> Option<&'static Oracle> {
    static SHARED: OnceLock<Result<Oracle, Error>> = OnceLock::new();
    match plan(env::var("BOUGH_ORACLE").ok().as_deref(), probe_ghc) {
        Plan::Skip => {
            // Written to the standard error itself: the test harness keeps
            // what `eprintln!` prints and shows it only for a test that
            // fails, so a skipped test would read as one that ran. The line
            // goes in one write, so that the harness's own output, on the
            // same terminal, cannot cut it. The harness names each test's
            // thread after the test.
            let test = thread::current()
                .name()
                .map_or_else(String::new, |name| format!(" {name}"));
            let line = format!(
                "skipped{test}: BOUGH_ORACLE=skip, so this test, which needs GHC, did not run\n"
            );
            let _ = io::stderr().write_all(line.as_bytes());
            None
        }
        Plan::Fail(message) => panic!("{message}"),
        Plan::Run => match SHARED.get_or_init(|| Oracle::build(directory)) {
            Ok(oracle) => Some(oracle),
            Err(error) => panic!("{error}"),
        },
    }
}

// ----- building -----

/// Builds the oracle into `directory` and returns the binary's path:
/// `ghc -O1 -threaded -rtsopts -with-rtsopts=-M1g`, so the time limit
/// interrupts pure code, a heap overflow answers `ERR heap` instead of
/// taking the machine, and a test may lower the limit.
///
/// It builds once per process and directory, under a lock file in the
/// directory.
pub fn build(directory: &Path) -> Result<PathBuf, Error> {
    compile_haskell(
        directory,
        "Oracle.hs",
        ORACLE,
        &["-O1", "-threaded", "-rtsopts", "-with-rtsopts=-M1g"],
    )
}

/// Compiles the Haskell program `source`, a file in [`haskell_directory`],
/// into the binary `directory/name`, with its objects in
/// `directory/name-objects`, and returns the binary's path.
///
/// It compiles once per process for each directory and name. A lock file,
/// `directory/build.lock`, keeps two processes from compiling into the
/// directory at once; GHC's recompilation check makes the second quick.
pub fn compile_haskell(
    directory: &Path,
    source: &str,
    name: &str,
    options: &[&str],
) -> Result<PathBuf, Error> {
    static COMPILED: Mutex<Vec<Compiled>> = Mutex::new(Vec::new());
    let mut compiled = lock(&COMPILED);
    let key = (directory.to_path_buf(), name.to_owned());
    if let Some((_, result)) = compiled.iter().find(|(done, _)| *done == key) {
        return result.clone();
    }
    let result = compile_now(directory, source, name, options);
    compiled.push((key, result.clone()));
    result
}

/// A finished compile: the directory and binary name, and its result.
type Compiled = ((PathBuf, String), Result<PathBuf, Error>);

fn compile_now(
    directory: &Path,
    source: &str,
    name: &str,
    options: &[&str],
) -> Result<PathBuf, Error> {
    fs::create_dir_all(directory)
        .map_err(|error| Error::Build(format!("cannot create {}: {error}", directory.display())))?;
    let _lock = BuildLock::acquire(directory.join("build.lock"))?;
    let haskell = haskell_directory();
    let binary = directory.join(name);
    let mut include = OsString::from("-i");
    include.push(&haskell);
    let ghc = ghc_command();
    let mut command = Command::new(&ghc);
    command
        .args(options)
        .arg(include)
        .arg("-outputdir")
        .arg(directory.join(format!("{name}-objects")))
        .arg("-o")
        .arg(&binary)
        .arg(haskell.join(source));
    let output = command.output().map_err(|error| {
        Error::GhcMissing(format!(
            "`{}` could not be run ({error})",
            ghc.to_string_lossy()
        ))
    })?;
    if output.status.success() {
        return Ok(binary);
    }
    let mut message = format!(
        "{command:?} exited with {}\n{}{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if message.contains("Test.HUnit") {
        message.push_str("\nThe tests need the HUnit package: `apt-get install libghc-hunit-dev`, or `cabal install --lib HUnit`.");
    }
    Err(Error::Build(message))
}

/// When a lock file counts as abandoned, and how waiting for one goes.
#[derive(Clone, Copy, Debug)]
struct LockTiming {
    /// How often the holder touches the lock file.
    heartbeat: Duration,
    /// A lock file nobody has touched for this long was left by a process
    /// that died holding it, for example a test run interrupted by Ctrl-C.
    stale: Duration,
    /// How long to wait for another holder before giving up.
    patience: Duration,
    /// How long a waiter sleeps between looks.
    poll: Duration,
}

/// A lock file, held while it exists and holds this holder's token: created
/// exclusively, touched while held, and removed on drop if it is still this
/// holder's.
struct BuildLock {
    path: PathBuf,
    token: String,
    heartbeat: Option<(mpsc::Sender<()>, JoinHandle<()>)>,
}

impl BuildLock {
    fn acquire(path: PathBuf) -> Result<BuildLock, Error> {
        BuildLock::acquire_with(path, BUILD_LOCK)
    }

    fn acquire_with(path: PathBuf, timing: LockTiming) -> Result<BuildLock, Error> {
        let started = Instant::now();
        let token = lock_token();
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    if let Err(error) = file.write_all(token.as_bytes()) {
                        let _ = fs::remove_file(&path);
                        return Err(Error::Build(format!(
                            "cannot write {}: {error}",
                            path.display()
                        )));
                    }
                    let (stop, stopped) = mpsc::channel::<()>();
                    let heartbeat = thread::Builder::new()
                        .name("bough-oracle build lock".to_owned())
                        .spawn(move || {
                            while let Err(RecvTimeoutError::Timeout) =
                                stopped.recv_timeout(timing.heartbeat)
                            {
                                let _ = file.set_modified(SystemTime::now());
                            }
                        });
                    return match heartbeat {
                        Ok(heartbeat) => Ok(BuildLock {
                            path,
                            token,
                            heartbeat: Some((stop, heartbeat)),
                        }),
                        Err(error) => {
                            let _ = fs::remove_file(&path);
                            Err(Error::Build(format!("cannot start a thread: {error}")))
                        }
                    };
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let abandoned = age(&path).is_some_and(|age| age > timing.stale);
                    if abandoned && break_abandoned(&path, timing.stale) {
                        continue;
                    }
                    if started.elapsed() > timing.patience {
                        return Err(Error::Build(format!(
                            "another build has held {} for {:?}",
                            path.display(),
                            timing.patience
                        )));
                    }
                    thread::sleep(timing.poll);
                }
                Err(error) => {
                    return Err(Error::Build(format!(
                        "cannot create {}: {error}",
                        path.display()
                    )));
                }
            }
        }
    }
}

impl Drop for BuildLock {
    fn drop(&mut self) {
        if let Some((stop, heartbeat)) = self.heartbeat.take() {
            drop(stop);
            let _ = heartbeat.join();
        }
        // A lock is taken over only once nobody has touched it for the stale
        // time, which a live holder's heartbeat prevents. Should it happen
        // all the same, the file is another holder's now, and stays.
        if fs::read_to_string(&self.path).is_ok_and(|text| text == self.token) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// A token no other holder of a lock has: the process id, which is also for
/// a person looking at a lock left behind, a count within the process, and
/// the time.
fn lock_token() -> String {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!(
        "{} {} {time}\n",
        std::process::id(),
        COUNT.fetch_add(1, Ordering::SeqCst)
    )
}

/// How long ago a file was last modified, when it exists and says.
fn age(path: &Path) -> Option<Duration> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
}

/// Removes an abandoned lock file, one waiter at a time, and says whether it
/// did. A waiter creates `<lock>.break` exclusively, looks at the lock's age
/// again, and removes the lock only if it is still abandoned. So of two
/// waiters that found it abandoned at once, the second finds the lock the
/// first took, fresh, and leaves it. A `.break` file left by a waiter that
/// died in the moment it holds one is abandoned in turn after the stale
/// time.
fn break_abandoned(path: &Path, stale: Duration) -> bool {
    let mut breaking = path.as_os_str().to_owned();
    breaking.push(".break");
    let breaking = PathBuf::from(breaking);
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&breaking)
    {
        Ok(_) => {
            let removed = age(path).is_some_and(|age| age > stale) && fs::remove_file(path).is_ok();
            let _ = fs::remove_file(&breaking);
            removed
        }
        Err(_) => {
            if age(&breaking).is_some_and(|age| age > stale) {
                let _ = fs::remove_file(&breaking);
            }
            false
        }
    }
}

/// Locks a mutex whose data a panic cannot leave inconsistent: a list of
/// idle processes or of finished builds.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ----- the pool -----

/// The oracle: a pool of GHC processes that answer programs.
///
/// Each thread that asks takes an idle process, or starts one, and puts it
/// back after the answer, so parallel threads never share a process. A
/// process that dies is replaced; the error names the program it was
/// answering.
pub struct Oracle {
    binary: PathBuf,
    arguments: Vec<OsString>,
    watchdog: Duration,
    idle: Mutex<Vec<Process>>,
    started: AtomicUsize,
}

impl fmt::Debug for Oracle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Oracle")
            .field("binary", &self.binary)
            .field("arguments", &self.arguments)
            .field("watchdog", &self.watchdog)
            .field("started", &self.processes_started())
            .finish_non_exhaustive()
    }
}

impl Oracle {
    /// Builds the oracle into `directory`, once per process, and returns a
    /// pool that has started no process yet.
    pub fn build(directory: impl AsRef<Path>) -> Result<Oracle, Error> {
        build(directory.as_ref()).map(Oracle::with_binary)
    }

    /// A pool over an oracle binary built earlier.
    pub fn with_binary(binary: PathBuf) -> Oracle {
        Oracle {
            binary,
            arguments: Vec::new(),
            watchdog: DEFAULT_WATCHDOG,
            idle: Mutex::new(Vec::new()),
            started: AtomicUsize::new(0),
        }
    }

    /// Sets how long to wait for an answer before the process is killed and
    /// the program reported: 30 seconds unless set. A process answers
    /// `TIMEOUT` after five seconds on its own.
    pub fn with_watchdog(mut self, watchdog: Duration) -> Oracle {
        self.watchdog = watchdog;
        self
    }

    /// Sets the heap limit of each process, in megabytes: 1024 unless set. A
    /// program that needs more answers `ERR heap`.
    pub fn with_heap_limit(mut self, megabytes: u32) -> Oracle {
        self.arguments = vec![
            OsString::from("+RTS"),
            OsString::from(format!("-M{megabytes}m")),
            OsString::from("-RTS"),
        ];
        self
    }

    /// Answers a program.
    pub fn answer(&self, program: &Program) -> Result<Answer, Error> {
        self.answer_line(&program.to_string())
    }

    /// Answers one line of the protocol as it is, whether or not it is a
    /// program.
    pub fn answer_line(&self, line: &str) -> Result<Answer, Error> {
        if line.contains(['\n', '\r']) {
            return Err(Error::NotOneLine);
        }
        let mut process = self.take()?;
        if process.send(line).is_err() {
            // The process exited before or while it read the program.
            return Err(process.died(line, String::new()));
        }
        match process.output.recv_timeout(self.watchdog) {
            Ok(Output::Line(text)) => match Answer::parse(&text) {
                Ok(answer) => {
                    lock(&self.idle).push(process);
                    Ok(answer)
                }
                Err(answer) => {
                    process.stop();
                    Err(Error::Malformed {
                        program: line.to_owned(),
                        answer,
                        standard_error: process.standard_error(),
                    })
                }
            },
            // The output ended in the middle of the line: the process exited
            // while it wrote the answer.
            Ok(Output::Unfinished(text)) => Err(process.died(line, text)),
            Err(RecvTimeoutError::Timeout) => {
                process.stop();
                Err(Error::NoAnswer {
                    program: line.to_owned(),
                    waited: self.watchdog,
                    standard_error: process.standard_error(),
                })
            }
            Err(RecvTimeoutError::Disconnected) => Err(process.died(line, String::new())),
        }
    }

    /// How many processes this pool has started.
    pub fn processes_started(&self) -> usize {
        self.started.load(Ordering::SeqCst)
    }

    /// The operating-system ids of the idle processes.
    pub fn idle_process_ids(&self) -> Vec<u32> {
        lock(&self.idle)
            .iter()
            .map(|process| process.child.id())
            .collect()
    }

    /// An idle process that is still running, or a new one. A process that
    /// exited while idle took no program with it, so it is dropped quietly.
    fn take(&self) -> Result<Process, Error> {
        loop {
            let Some(mut process) = lock(&self.idle).pop() else {
                break;
            };
            if let Ok(None) = process.child.try_wait() {
                return Ok(process);
            }
        }
        let process = Process::start(&self.binary, &self.arguments)?;
        self.started.fetch_add(1, Ordering::SeqCst);
        Ok(process)
    }
}

/// One oracle process, and the threads that read its output.
struct Process {
    child: Child,
    input: Option<ChildStdin>,
    output: Receiver<Output>,
    standard_error: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>,
}

/// What a process wrote on its standard output, as the reader passes it on.
enum Output {
    /// A line, without its line ending: an answer.
    Line(String),
    /// Text after the last line ending, where the output ended: an answer
    /// the process was writing when it exited.
    Unfinished(String),
}

impl Process {
    fn start(binary: &Path, arguments: &[OsString]) -> Result<Process, Error> {
        // The binary accepts RTS options, so a GHCRTS meant for other
        // programs would reach it too.
        let mut child = Command::new(binary)
            .args(arguments)
            .env_remove("GHCRTS")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                Error::Process(format!("cannot start {}: {error}", binary.display()))
            })?;
        let (Some(input), Some(output), Some(mut errors)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Process("the oracle process has no pipes".to_owned()));
        };
        let (sender, received) = mpsc::channel();
        let answers = thread::Builder::new()
            .name("bough-oracle answers".to_owned())
            .spawn(move || {
                let mut reader = BufReader::new(output);
                loop {
                    let mut text = String::new();
                    // The end of the output, or output that is not UTF-8, ends
                    // the reader; the pool finds the channel closed.
                    let Ok(1..) = reader.read_line(&mut text) else {
                        break;
                    };
                    let piece = if text.ends_with('\n') {
                        text.pop();
                        if text.ends_with('\r') {
                            text.pop();
                        }
                        Output::Line(text)
                    } else {
                        Output::Unfinished(text)
                    };
                    if sender.send(piece).is_err() {
                        break;
                    }
                }
            });
        let standard_error = Arc::new(Mutex::new(String::new()));
        let kept = Arc::clone(&standard_error);
        let errors = thread::Builder::new()
            .name("bough-oracle errors".to_owned())
            .spawn(move || {
                let mut buffer = [0; 4096];
                while let Ok(count @ 1..) = errors.read(&mut buffer) {
                    let mut text = lock(&kept);
                    text.push_str(&String::from_utf8_lossy(&buffer[..count]));
                    if text.len() > STANDARD_ERROR_TAIL {
                        let excess = text.len() - STANDARD_ERROR_TAIL;
                        let cut = (excess..text.len())
                            .find(|index| text.is_char_boundary(*index))
                            .unwrap_or(0);
                        text.drain(..cut);
                    }
                }
            });
        match (answers, errors) {
            (Ok(answers), Ok(errors)) => Ok(Process {
                child,
                input: Some(input),
                output: received,
                standard_error,
                readers: vec![answers, errors],
            }),
            (Err(error), _) | (_, Err(error)) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(Error::Process(format!(
                    "cannot start a reader thread: {error}"
                )))
            }
        }
    }

    /// Writes one program and its line ending.
    fn send(&mut self, line: &str) -> io::Result<()> {
        let input = self.input.as_mut().ok_or(io::ErrorKind::BrokenPipe)?;
        input.write_all(line.as_bytes())?;
        input.write_all(b"\n")?;
        input.flush()
    }

    /// Kills the process if it still runs, and waits for it and its readers.
    /// Returns how it exited.
    fn stop(&mut self) -> String {
        self.input = None;
        let _ = self.child.kill();
        let status = match self.child.wait() {
            Ok(status) => status.to_string(),
            Err(error) => format!("unknown: {error}"),
        };
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
        status
    }

    /// The error for a process that exited while answering `program`, having
    /// written `unfinished_answer` of its answer.
    fn died(mut self, program: &str, unfinished_answer: String) -> Error {
        let status = self.stop();
        Error::Died {
            program: program.to_owned(),
            status,
            standard_error: self.standard_error(),
            unfinished_answer,
        }
    }

    fn standard_error(&self) -> String {
        lock(&self.standard_error).clone()
    }
}

impl Drop for Process {
    /// Closes the process's input, which ends it, and reaps it.
    fn drop(&mut self) {
        self.input = None;
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if let Ok(Some(_)) | Err(_) = self.child.try_wait() {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Barrier;

    #[test]
    fn skip_skips_without_asking_for_ghc() {
        assert_eq!(plan(Some("skip"), || panic!("GHC was probed")), Plan::Skip);
    }

    #[test]
    fn with_ghc_a_test_runs() {
        assert_eq!(plan(None, || Ok("9.4.7".to_owned())), Plan::Run);
        assert_eq!(plan(Some(""), || Ok("9.4.7".to_owned())), Plan::Run);
    }

    #[test]
    fn without_ghc_a_test_fails_with_the_install_hint() {
        let Plan::Fail(message) =
            plan(
                None,
                || Err("`ghc` could not be run (not found)".to_owned()),
            )
        else {
            panic!("expected a failure");
        };
        assert!(
            message.contains("apt-get install ghc libghc-hunit-dev"),
            "{message}"
        );
        assert!(message.contains("ghcup"), "{message}");
        assert!(message.contains("BOUGH_ORACLE=skip"), "{message}");
        assert!(message.contains("not found"), "{message}");
    }

    #[test]
    fn another_value_of_bough_oracle_is_a_mistake() {
        let Plan::Fail(message) = plan(Some("yes"), || Ok("9.4.7".to_owned())) else {
            panic!("expected a failure");
        };
        assert!(message.contains("`yes`"), "{message}");
    }

    #[test]
    fn a_long_program_is_shortened_in_messages() {
        let program = "é".repeat(3000);
        let short = shorten(&program);
        assert!(short.len() < 2100, "{}", short.len());
        assert!(short.ends_with("(6000 bytes in all)"), "{short}");
        assert_eq!(shorten("SInput 0"), "SInput 0");
    }

    #[test]
    fn a_line_break_is_refused_before_any_process_starts() {
        let oracle = Oracle::with_binary(PathBuf::from("/nonexistent/bough-oracle"));
        assert_eq!(
            oracle.answer_line("Program\nEverything"),
            Err(Error::NotOneLine)
        );
        assert_eq!(oracle.processes_started(), 0);
    }

    #[test]
    fn a_missing_binary_is_an_error_not_a_panic() {
        let oracle = Oracle::with_binary(PathBuf::from("/nonexistent/bough-oracle"));
        let Err(Error::Process(message)) = oracle.answer_line("Program Everything [] [] [] []")
        else {
            panic!("expected a process error");
        };
        assert!(message.contains("/nonexistent/bough-oracle"), "{message}");
    }

    /// A directory of its own for one lock test, under the system's
    /// temporary directory.
    fn lock_directory(name: &str) -> PathBuf {
        let directory = env::temp_dir().join(format!("bough-oracle-{name}-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn a_build_lock_is_released_on_drop_and_a_second_holder_waits_for_it() {
        let directory = lock_directory("lock-waits");
        let path = directory.join("build.lock");
        let first = BuildLock::acquire(path.clone()).unwrap();
        assert!(path.exists());
        let (acquired, second_acquired) = mpsc::channel();
        let waiting = {
            let path = path.clone();
            thread::spawn(move || {
                let second = BuildLock::acquire(path).unwrap();
                acquired.send(Instant::now()).unwrap();
                drop(second);
            })
        };
        thread::sleep(Duration::from_millis(300));
        assert!(
            second_acquired.try_recv().is_err(),
            "the second holder did not wait"
        );
        let released = Instant::now();
        drop(first);
        assert!(second_acquired.recv().unwrap() >= released);
        waiting.join().unwrap();
        assert!(!path.exists());
        fs::remove_dir_all(directory).unwrap();
    }

    /// Leaves a lock file that nobody has touched for twice the stale time.
    fn abandon(path: &Path) {
        let abandoned = fs::File::create(path).unwrap();
        abandoned
            .set_modified(SystemTime::now() - 2 * BUILD_LOCK.stale)
            .unwrap();
    }

    #[test]
    fn a_build_lock_nobody_touches_is_taken_over() {
        let directory = lock_directory("lock-stale");
        let path = directory.join("build.lock");
        abandon(&path);
        let started = Instant::now();
        let lock = BuildLock::acquire(path.clone()).unwrap();
        assert!(started.elapsed() < BUILD_LOCK.stale);
        drop(lock);
        assert!(!path.exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_holder_that_outlives_the_stale_time_keeps_its_lock() {
        // Short times, so that the first holder lives three stale times
        // while its heartbeat keeps the lock fresh and a second one waits.
        let timing = LockTiming {
            heartbeat: Duration::from_millis(50),
            stale: Duration::from_millis(600),
            patience: Duration::from_secs(60),
            poll: Duration::from_millis(10),
        };
        let directory = lock_directory("lock-heartbeat");
        let path = directory.join("build.lock");
        let first = BuildLock::acquire_with(path.clone(), timing).unwrap();
        let (acquired, second_acquired) = mpsc::channel();
        let waiting = {
            let path = path.clone();
            thread::spawn(move || {
                let second = BuildLock::acquire_with(path, timing).unwrap();
                acquired.send(Instant::now()).unwrap();
                drop(second);
            })
        };
        thread::sleep(3 * timing.stale);
        assert!(
            second_acquired.try_recv().is_err(),
            "the second holder took the lock of a live one"
        );
        let released = Instant::now();
        drop(first);
        assert!(second_acquired.recv().unwrap() >= released);
        waiting.join().unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_waiter_late_to_break_an_abandoned_lock_leaves_the_new_holders_lock() {
        // Two waiters find the lock abandoned. The first breaks it and takes
        // the lock; the second, a moment behind, must leave the first's lock.
        let directory = lock_directory("lock-late-break");
        let path = directory.join("build.lock");
        abandon(&path);
        let first = BuildLock::acquire(path.clone()).unwrap();
        assert!(!break_abandoned(&path, BUILD_LOCK.stale));
        assert_eq!(fs::read_to_string(&path).unwrap(), first.token);
        drop(first);
        assert!(!path.exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_holder_removes_only_its_own_lock() {
        let directory = lock_directory("lock-own");
        let path = directory.join("build.lock");
        let lock = BuildLock::acquire(path.clone()).unwrap();
        // As if another holder had taken the lock over.
        fs::write(&path, "another holder\n").unwrap();
        drop(lock);
        assert_eq!(fs::read_to_string(&path).unwrap(), "another holder\n");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn waiters_that_race_for_an_abandoned_lock_hold_it_one_at_a_time() {
        let timing = LockTiming {
            poll: Duration::from_millis(1),
            ..BUILD_LOCK
        };
        let directory = lock_directory("lock-race");
        let path = directory.join("build.lock");
        let holders = AtomicUsize::new(0);
        let most = AtomicUsize::new(0);
        for _ in 0..20 {
            abandon(&path);
            let start = Barrier::new(8);
            thread::scope(|scope| {
                for _ in 0..8 {
                    scope.spawn(|| {
                        start.wait();
                        let lock = BuildLock::acquire_with(path.clone(), timing).unwrap();
                        let now = holders.fetch_add(1, Ordering::SeqCst) + 1;
                        most.fetch_max(now, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(2));
                        holders.fetch_sub(1, Ordering::SeqCst);
                        drop(lock);
                    });
                }
            });
        }
        assert_eq!(most.load(Ordering::SeqCst), 1);
        fs::remove_dir_all(directory).unwrap();
    }

    /// A pool whose processes run `sh -c script` in place of the oracle.
    #[cfg(unix)]
    fn pretend_oracle(script: &str) -> Oracle {
        Oracle {
            arguments: vec![OsString::from("-c"), OsString::from(script)],
            ..Oracle::with_binary(PathBuf::from("/bin/sh"))
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_process_that_exits_in_the_middle_of_an_answer_died_and_says_what_it_wrote() {
        let oracle =
            pretend_oracle("read program; echo 'dying on purpose' >&2; printf 'OK [[0,'; exit 4");
        let outcome = oracle.answer_line("please die");
        assert_eq!(
            outcome,
            Err(Error::Died {
                program: "please die".to_owned(),
                status: "exit status: 4".to_owned(),
                standard_error: "dying on purpose\n".to_owned(),
                unfinished_answer: "OK [[0,".to_owned(),
            })
        );
        let message = outcome.unwrap_err().to_string();
        assert!(message.contains("unfinished answer: OK [[0,"), "{message}");
    }

    #[cfg(unix)]
    #[test]
    fn an_answer_off_the_protocol_names_the_program_and_stops_the_process() {
        let oracle =
            pretend_oracle("read program; echo 'out of step' >&2; echo 'NOT AN ANSWER'; read next");
        let outcome = oracle.answer_line("a program");
        let Err(Error::Malformed {
            program,
            answer,
            standard_error,
        }) = &outcome
        else {
            panic!("expected a malformed answer, got {outcome:?}");
        };
        assert_eq!(program, "a program");
        assert_eq!(answer.line, "NOT AN ANSWER");
        assert_eq!(standard_error, "out of step\n");
        assert!(oracle.idle_process_ids().is_empty());
        let message = outcome.unwrap_err().to_string();
        assert!(message.contains("program: a program"), "{message}");
        assert!(message.contains("standard error: out of step"), "{message}");
    }
}
