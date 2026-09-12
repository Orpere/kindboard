//! Async subprocess runner (ADR-0009).
//!
//! All subprocess invocation goes through [`Cmd`]. Rules enforced here:
//!
//! 1. `tokio::process::Command`, args arrays only — never `sh -c`, never
//!    string-built command lines (no shell interpolation; user input is inert).
//! 2. On Unix the child runs in its **own process group**
//!    (`process_group(0)`, the safe std API equivalent to `setpgid(0, 0)`),
//!    so cancellation/timeout can reap the whole tree with `killpg`
//!    (TERM → 5 s grace → KILL), no zombie leaks.
//! 3. stdout is streamed as lines; stderr keeps a bounded tail (2 KiB) for
//!    error messages.
//! 4. Every call has a deadline; exit code 0 is the only success signal.
//! 5. Cancellation is cooperative via
//!    `tokio_util::sync::CancellationToken`. A spawn-time watcher delivers
//!    the TERM → grace → KILL ladder as soon as the token fires, so
//!    cancellation engages even while a caller is still draining the
//!    streams; `wait` then reports `ExecError::Cancelled` regardless of the
//!    reaped exit code (a group TERM can make a shell exit 0).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::error::ExecError;

/// Default per-call timeout (override per command; ADR-0009: 30 s probes,
/// 2 min helm/cilium, 5 min `kind create`).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// Grace period between SIGTERM and SIGKILL in the kill ladder.
pub const KILL_GRACE: Duration = Duration::from_secs(5);
/// Maximum stderr kept for error messages (bytes, "last 2 KiB").
pub const STDERR_TAIL_BYTES: usize = 2048;

/// A description of one subprocess invocation (builder + runner).
#[derive(Debug, Clone)]
pub struct Cmd {
    program: String,
    args: Vec<String>,
    timeout: Duration,
    env: BTreeMap<String, String>,
    cancel: Option<CancellationToken>,
}

/// The result of a completed [`Cmd`] run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdOutput {
    /// Exit status; `None` when killed by a signal.
    pub exit_code: Option<i32>,
    /// Full stdout, split into lines (a trailing newline does not produce an
    /// empty final line).
    pub stdout_lines: Vec<String>,
    /// Tail of stderr (last [`STDERR_TAIL_BYTES`] bytes, lossy UTF-8).
    pub stderr_tail: String,
}

impl CmdOutput {
    /// Whether the process exited with code 0.
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// stdout joined with newlines.
    pub fn stdout(&self) -> String {
        self.stdout_lines.join("\n")
    }
}

/// A spawned-but-not-yet-waited process, exposing line streams.
pub struct ProcessHandle {
    child: Child,
    pid: Option<u32>,
    program: String,
    timeout: Duration,
    cancel: Option<CancellationToken>,
    /// Set once the child has been reaped (by `wait` or the kill ladder),
    /// so the cancel watcher never signals a stale/possibly-reused pgid.
    reaped: std::sync::Arc<AtomicBool>,
    stdout_rx: mpsc::Receiver<String>,
    stderr_rx: mpsc::Receiver<String>,
    stderr_shared: std::sync::Arc<std::sync::Mutex<String>>,
    started: Instant,
}

impl Cmd {
    /// Create a command for `program` (looked up on PATH by the OS).
    pub fn new(program: impl Into<String>) -> Self {
        Cmd {
            program: program.into(),
            args: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            env: BTreeMap::new(),
            cancel: None,
        }
    }

    /// Append one argument.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Append many arguments.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for arg in args {
            self.args.push(arg.into());
        }
        self
    }

    /// Set the per-call deadline.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Inject one environment variable (inherited env is kept).
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Inject several environment variables (inherited env is kept).
    pub fn envs(mut self, map: impl IntoIterator<Item = (String, String)>) -> Self {
        self.env.extend(map);
        self
    }

    /// Attach a cancellation token. When it fires (or the timeout expires),
    /// the process group gets TERM, then KILL after [`KILL_GRACE`].
    pub fn cancel(mut self, token: CancellationToken) -> Self {
        self.cancel = Some(token);
        self
    }

    /// Program name (argv[0]).
    pub fn program(&self) -> &str {
        &self.program
    }

    /// The full argv as strings (for display, planning, and tests).
    pub fn argv(&self) -> Vec<String> {
        std::iter::once(self.program.clone())
            .chain(self.args.iter().cloned())
            .collect()
    }

    /// The configured deadline.
    pub fn deadline(&self) -> Duration {
        self.timeout
    }

    /// Run to completion and capture output (bounded stderr tail).
    ///
    /// # Errors
    /// - [`ExecError::Spawn`] when the program cannot be spawned
    /// - [`ExecError::Command`] on a non-zero exit or signal death
    /// - [`ExecError::Timeout`] when the deadline fires
    /// - [`ExecError::Cancelled`] when the attached token fires
    pub async fn run(&self) -> Result<CmdOutput, ExecError> {
        let handle = self.spawn().await?;
        handle.wait().await
    }

    /// Spawn the process, returning a [`ProcessHandle`] with streaming
    /// stdout/stderr receivers. The caller should drain the receivers while
    /// the process runs, then call [`ProcessHandle::wait`].
    pub async fn spawn(&self) -> Result<ProcessHandle, ExecError> {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|source| ExecError::Spawn {
            prog: self.program.clone(),
            source,
        })?;
        let pid = child.id();

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (stdout_tx, stdout_rx) = mpsc::channel::<String>(64);
        let (stderr_tx, stderr_rx) = mpsc::channel::<String>(64);
        let stderr_shared = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let prog = self.program.clone();

        // Cancellation must engage even while a caller drains the streams
        // (the kill ladder inside `wait` cannot run until `wait` is called).
        // This watcher owns the signal delivery: on token fire it runs the
        // TERM → grace → KILL ladder against the child's process group,
        // independently of reaping. `reaped` guards against signalling a
        // process group that no longer belongs to this child.
        let reaped = std::sync::Arc::new(AtomicBool::new(false));
        let watcher_token = self.cancel.clone();
        let watcher_reaped = reaped.clone();
        tokio::spawn(async move {
            let Some(token) = watcher_token else {
                return;
            };
            token.cancelled().await;
            if !watcher_reaped.load(Ordering::SeqCst) {
                signal_group(pid, nix::sys::signal::Signal::SIGTERM);
                tokio::time::sleep(KILL_GRACE).await;
                if !watcher_reaped.load(Ordering::SeqCst) {
                    signal_group(pid, nix::sys::signal::Signal::SIGKILL);
                }
            }
        });

        tokio::spawn(async move {
            if let Some(stdout) = stdout {
                forward_lines(stdout, stdout_tx).await;
            }
        });
        let stderr_for_task = stderr_shared.clone();
        tokio::spawn(async move {
            if let Some(stderr) = stderr {
                forward_tail(stderr, stderr_tx, stderr_for_task).await;
            }
        });

        Ok(ProcessHandle {
            child,
            pid,
            program: prog,
            timeout: self.timeout,
            cancel: self.cancel.clone(),
            reaped,
            stdout_rx,
            stderr_rx,
            stderr_shared,
            started: Instant::now(),
        })
    }
}

/// Forward a piped stream as UTF-8 lines to `tx`.
///
/// Reads raw chunks and splits on `\n`, buffering the unterminated tail
/// between chunks. Invalid UTF-8 is converted lossy **per line**, so a
/// binary byte in the middle of the stream degrades to U+FFFD in that line
/// only — every subsequent line still arrives (tokio's
/// `AsyncBufReadExt::lines` would silently end the whole stream at the
/// first invalid byte). Valid multi-byte characters split across chunk
/// boundaries survive intact because the line is accumulated before
/// conversion. `\r\n` is stripped like `lines()` does, and a final line
/// without a trailing newline is forwarded on EOF. A single line may grow
/// without bound until its terminator arrives — the same contract as
/// `read_line`-based `lines()`, and bounded in practice by the ring/pipe
/// consumers downstream.
async fn forward_lines<R>(stream: R, tx: mpsc::Sender<String>)
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(stream);
    let mut carry: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                carry.extend_from_slice(&buf[..n]);
                while let Some(nl) = carry.iter().position(|&byte| byte == b'\n') {
                    let line_bytes: Vec<u8> = carry.drain(..=nl).collect();
                    let mut end = line_bytes.len() - 1; // index of the '\n'
                    if end > 0 && line_bytes[end - 1] == b'\r' {
                        end -= 1;
                    }
                    let line = String::from_utf8_lossy(&line_bytes[..end]).into_owned();
                    if tx.send(line).await.is_err() {
                        return; // receiver dropped
                    }
                }
            }
        }
    }
    if !carry.is_empty() {
        let _ = tx.send(String::from_utf8_lossy(&carry).into_owned()).await;
    }
}

/// Forward a piped stream as UTF-8 chunks to `tx`, also appending every
/// chunk to a shared bounded tail (newest last, capped at
/// [`STDERR_TAIL_BYTES`] bytes, lossy conversion for non-UTF-8 bytes).
async fn forward_tail<R>(
    mut stream: R,
    tx: mpsc::Sender<String>,
    tail: std::sync::Arc<std::sync::Mutex<String>>,
) where
    R: AsyncRead + Unpin,
{
    let mut buf = vec![0u8; 8192];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                if tx.send(chunk.clone()).await.is_err() {
                    break;
                }
                append_tail(&tail, &chunk);
            }
        }
    }
}

fn append_tail(tail: &std::sync::Mutex<String>, chunk: &str) {
    let Ok(mut guard) = tail.lock() else {
        return; // poisoned lock: drop the chunk rather than panic
    };
    guard.push_str(chunk);
    if guard.len() > STDERR_TAIL_BYTES {
        let start = guard.len() - STDERR_TAIL_BYTES;
        let boundary = guard
            .char_indices()
            .find(|(i, _)| *i >= start)
            .map(|(i, _)| i)
            .unwrap_or(guard.len());
        let trimmed = guard[boundary..].to_string();
        *guard = trimmed;
    }
}

impl ProcessHandle {
    /// Stream of stdout lines (drain while the process runs).
    pub fn stdout_lines(&mut self) -> &mut mpsc::Receiver<String> {
        &mut self.stdout_rx
    }

    /// Stream of stderr chunks (drain while the process runs).
    pub fn stderr_lines(&mut self) -> &mut mpsc::Receiver<String> {
        &mut self.stderr_rx
    }

    /// Program name.
    pub fn program(&self) -> &str {
        &self.program
    }

    /// Process id of the direct child (== process group id on Unix).
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Send SIGTERM to the child's process group.
    pub fn terminate_group(&self) {
        signal_group(self.pid, nix::sys::signal::Signal::SIGTERM);
    }

    /// Send SIGKILL to the child's process group.
    pub fn kill_group(&self) {
        signal_group(self.pid, nix::sys::signal::Signal::SIGKILL);
    }

    /// Wait for the process and return its output.
    ///
    /// Enforces the timeout/cancellation kill ladder. stdout is captured in
    /// full; stderr is the bounded tail accumulated so far.
    pub async fn wait(mut self) -> Result<CmdOutput, ExecError> {
        // Drain both pipes concurrently with waiting so a chatty child can
        // never stall on a full channel.
        let mut stdout_rx = std::mem::replace(&mut self.stdout_rx, mpsc::channel(1).1);
        let mut stderr_rx = std::mem::replace(&mut self.stderr_rx, mpsc::channel(1).1);
        let drain_stdout = tokio::spawn(async move {
            let mut lines = Vec::new();
            while let Some(line) = stdout_rx.recv().await {
                lines.push(line);
            }
            lines
        });
        let drain_stderr = tokio::spawn(async move { while stderr_rx.recv().await.is_some() {} });

        let outcome = self.await_outcome().await;
        let stdout_lines = drain_stdout.await.unwrap_or_default();
        let _ = drain_stderr.await;
        let stderr_tail = self
            .stderr_shared
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();

        match outcome {
            Ok(Some(status)) => {
                let exit_code = status.code();
                if exit_code == Some(0) {
                    Ok(CmdOutput {
                        exit_code,
                        stdout_lines,
                        stderr_tail,
                    })
                } else {
                    Err(ExecError::Command {
                        prog: self.program.clone(),
                        code: exit_code,
                        stderr_tail: tail_for_error(&stderr_tail),
                    })
                }
            }
            Ok(None) => Err(ExecError::Command {
                prog: self.program.clone(),
                code: None,
                stderr_tail: tail_for_error(&stderr_tail),
            }),
            Err(OutcomeError::Timeout { elapsed }) => Err(ExecError::Timeout {
                prog: self.program.clone(),
                elapsed,
            }),
            Err(OutcomeError::Cancelled) => Err(ExecError::Cancelled {
                prog: self.program.clone(),
            }),
        }
    }

    async fn await_outcome(
        &mut self,
    ) -> std::result::Result<Option<std::process::ExitStatus>, OutcomeError> {
        let deadline = tokio::time::sleep(self.timeout);
        tokio::pin!(deadline);

        let cancel = self.cancel.clone();
        let cancel_fut = async {
            match &cancel {
                Some(token) => token.cancelled().await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(cancel_fut);

        loop {
            // try_wait keeps the mutable borrow short-lived so the select
            // arms below can freely touch `self`.
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.reaped.store(true, Ordering::SeqCst);
                    // A group TERM can make a shell exit 0 (bash defers the
                    // signal while waiting on its foreground child and then
                    // exits successfully). A fired cancel token must be
                    // reported as Cancelled even when the child was reaped
                    // with exit code 0.
                    if self.cancelled() {
                        return Err(OutcomeError::Cancelled);
                    }
                    return Ok(Some(status));
                }
                Ok(None) => {}
                Err(_) => {
                    // try_wait errors only when the child was already
                    // reaped; mark it so the Drop guard never signals a
                    // possibly-reused pgid.
                    self.reaped.store(true, Ordering::SeqCst);
                    return Ok(None);
                }
            }

            tokio::select! {
                _ = &mut deadline => {
                    let elapsed = self.started.elapsed();
                    self.terminate_group();
                    self.grace_then_kill().await;
                    return Err(OutcomeError::Timeout { elapsed });
                }
                _ = &mut cancel_fut => {
                    self.terminate_group();
                    self.grace_then_kill().await;
                    return Err(OutcomeError::Cancelled);
                }
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
            }
        }
    }

    /// Whether an attached cancellation token has fired.
    fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
    }

    /// The TERM → (grace) → KILL ladder, waiting for the child to exit.
    async fn grace_then_kill(&mut self) {
        let grace = tokio::time::sleep(KILL_GRACE);
        tokio::pin!(grace);
        tokio::select! {
            _ = self.child.wait() => {}
            _ = &mut grace => {
                self.kill_group();
                let _ = self.child.wait().await;
            }
        }
        self.reaped.store(true, Ordering::SeqCst);
    }
}

impl Drop for ProcessHandle {
    /// A handle dropped without `wait` must not leak the child (worker
    /// shutdown/abort drops tasks mid-run): kill the whole process group
    /// immediately. Drop cannot await the TERM grace, so SIGKILL is the
    /// only ladder step that fits. `reaped` guards against signalling a
    /// group that no longer belongs to this child (set by every `wait`
    /// terminal path before the handle is dropped).
    fn drop(&mut self) {
        if !self.reaped.load(Ordering::SeqCst) {
            self.kill_group();
            self.reaped.store(true, Ordering::SeqCst);
        }
    }
}

#[derive(Debug)]
enum OutcomeError {
    Timeout { elapsed: Duration },
    Cancelled,
}

/// Poll interval between `try_wait` checks (processes exit quickly; this is
/// the worst-case latency added to detecting a normal exit).
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Signal the child's process group (pgid == child pid via
/// `process_group(0)`). ESRCH (already dead) is acceptable.
fn signal_group(pid: Option<u32>, signal: nix::sys::signal::Signal) {
    if let Some(pid) = pid {
        let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid as i32), signal);
    }
}

/// Cut the stored tail down to the amount shown in error messages,
/// always ending on a char boundary.
fn tail_for_error(tail: &str) -> String {
    if tail.len() <= STDERR_TAIL_BYTES {
        return tail.to_string();
    }
    let start = tail.len() - STDERR_TAIL_BYTES;
    match tail.char_indices().find(|(i, _)| *i >= start) {
        Some((i, _)) => tail[i..].to_string(),
        None => tail.to_string(),
    }
}

/// Kubeconfig YAML keys whose values are secrets (or secret-like) and must
/// never reach logs or progress events.
const KUBECONFIG_SECRET_KEYS: &[&str] = &[
    "client-key-data:",
    "client-certificate-data:",
    "token:",
    "password:",
    "client-secret:",
];

/// Scrub a single output line of kubeconfig secrets before it is forwarded
/// to progress events.
///
/// Matches lines of the form `key: value` (the shape kubeconfig YAML uses
/// for data fields) and replaces the value with `<redacted>`. Lines that
/// don't match pass through untouched, so ordinary CLI output is unaffected.
/// This is the runtime safeguard for the event channel: subprocess stdout is
/// the only path kubeconfig bytes (e.g. `kind get kubeconfig`) could take
/// into UI progress events. Parsing consumers of `CmdOutput` (kubeconfig
/// merge) use the unredacted collected output — this function only guards
/// the logging/event boundary.
pub fn redact_kubeconfig_secrets(line: &str) -> String {
    let trimmed = line.trim_start();
    for key in KUBECONFIG_SECRET_KEYS {
        if let Some(rest) = trimmed.strip_prefix(key) {
            let (value_start, _) = match rest.char_indices().find(|(_, c)| !c.is_whitespace()) {
                Some(found) => found,
                None => return line.to_string(),
            };
            let indent_len = line.len() - trimmed.len();
            let mut redacted = String::with_capacity(line.len());
            redacted.push_str(&line[..indent_len]);
            redacted.push_str(key);
            redacted.push_str(&rest[..value_start]);
            redacted.push_str("<redacted>");
            return redacted;
        }
    }
    line.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_scrubs_kubeconfig_secret_values() {
        assert_eq!(
            redact_kubeconfig_secrets(
                "    client-key-data: LS0tLS1CRUdJTiBSU0EgUFJJVkFURSBLRVktLS0tLQ=="
            ),
            "    client-key-data: <redacted>"
        );
        assert_eq!(
            redact_kubeconfig_secrets("    client-certificate-data: abcd1234"),
            "    client-certificate-data: <redacted>"
        );
        assert_eq!(
            redact_kubeconfig_secrets("    token:      secret-token-value"),
            "    token:      <redacted>"
        );
    }

    #[test]
    fn redact_passes_through_benign_lines() {
        let benign = [
            "apiVersion: v1",
            "contexts:",
            "  name: kind-demo",
            "server: https://127.0.0.1:3443",
            "namespace/default created",
            "deployment.apps/ingress-nginx-controller condition met",
        ];
        for line in benign {
            assert_eq!(redact_kubeconfig_secrets(line), line, "line: {line}");
        }
    }

    #[test]
    fn redact_never_redacts_whole_line_when_value_absent() {
        assert_eq!(
            redact_kubeconfig_secrets("token:"),
            "token:",
            "a key with no value must pass through (no whitespace separator)"
        );
        assert_eq!(
            redact_kubeconfig_secrets("tokenized output is fine"),
            "tokenized output is fine"
        );
    }

    #[tokio::test]
    async fn runs_simple_command() {
        let out = Cmd::new("echo").arg("hello world").run().await.unwrap();
        assert!(out.success());
        assert_eq!(out.stdout_lines, vec!["hello world".to_string()]);
        assert_eq!(out.stdout(), "hello world");
        assert!(out.stderr_tail.is_empty());
    }

    #[tokio::test]
    async fn multiple_output_lines() {
        let out = Cmd::new("sh")
            .arg("-c")
            .arg("printf 'a\\nb\\nc\\n'")
            .run()
            .await
            .unwrap();
        assert_eq!(out.stdout_lines, vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn non_zero_exit_is_command_error() {
        let err = Cmd::new("sh")
            .arg("-c")
            .arg("echo boom >&2; exit 3")
            .run()
            .await;
        match err {
            Err(ExecError::Command {
                prog,
                code,
                stderr_tail,
            }) => {
                assert_eq!(prog, "sh");
                assert_eq!(code, Some(3));
                assert!(stderr_tail.contains("boom"), "stderr tail: {stderr_tail}");
            }
            other => panic!("expected Command error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn timeout_kills_process_group() {
        let cmd = Cmd::new("sh")
            .arg("-c")
            .arg("sleep 30 & sleep 30")
            .timeout(Duration::from_millis(300));
        let started = std::time::Instant::now();
        let err = cmd.run().await;
        let elapsed = started.elapsed();
        assert!(matches!(err, Err(ExecError::Timeout { .. })), "got {err:?}");
        // TERM kills promptly; must not take the full 5 s grace.
        assert!(
            elapsed < Duration::from_secs(3),
            "kill ladder took too long: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn cancellation_kills_process_group() {
        let token = CancellationToken::new();
        let cmd = Cmd::new("sh")
            .arg("-c")
            .arg("sleep 30 & sleep 30")
            .cancel(token.clone());
        let task = tokio::spawn(async move { cmd.run().await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        token.cancel();
        let result = task.await.unwrap();
        assert!(
            matches!(result, Err(ExecError::Cancelled { .. })),
            "got {result:?}"
        );
    }

    #[tokio::test]
    async fn spawn_error_is_reported() {
        let err = Cmd::new("definitely-not-a-real-binary-xyz").run().await;
        assert!(matches!(err, Err(ExecError::Spawn { .. })), "got {err:?}");
    }

    #[tokio::test]
    async fn env_injection_reaches_child() {
        let out = Cmd::new("sh")
            .arg("-c")
            .arg("printf '%s' \"$KBTEST_VAR\"")
            .env("KBTEST_VAR", "injected-value")
            .run()
            .await
            .unwrap();
        assert_eq!(out.stdout(), "injected-value");
    }

    #[tokio::test]
    async fn argv_is_verbatim_with_metacharacters() {
        // A space and shell metachars must survive as a single argv entry
        // (no shell interpolation).
        let out = Cmd::new("echo")
            .arg("$(rm -rf /) ; with spaces")
            .run()
            .await
            .unwrap();
        assert_eq!(
            out.stdout_lines,
            vec!["$(rm -rf /) ; with spaces".to_string()]
        );
    }

    #[tokio::test]
    async fn streaming_handle_delivers_lines() {
        let cmd = Cmd::new("sh")
            .arg("-c")
            .arg("printf 'x\\ny\\nz\\n'; echo err >&2");
        let mut handle = cmd.spawn().await.unwrap();
        let mut lines = Vec::new();
        while let Some(line) = handle.stdout_lines().recv().await {
            lines.push(line);
        }
        let out = handle.wait().await.unwrap();
        assert_eq!(lines, vec!["x", "y", "z"]);
        assert!(out.success());
    }

    #[tokio::test]
    async fn stderr_tail_is_bounded() {
        // 100 KiB of stderr; the kept tail must stay bounded.
        let cmd = Cmd::new("sh")
            .arg("-c")
            .arg("yes 'x' | head -c 100000 >&2; exit 1");
        let err = cmd.run().await;
        match err {
            Err(ExecError::Command { stderr_tail, .. }) => {
                assert!(
                    stderr_tail.len() <= STDERR_TAIL_BYTES + 8192,
                    "tail too large: {}",
                    stderr_tail.len()
                );
                assert!(stderr_tail.trim_end().ends_with('x'));
            }
            other => panic!("expected Command error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn killed_by_signal_has_no_exit_code() {
        let cmd = Cmd::new("sh")
            .arg("-c")
            .arg("kill -9 $$")
            .timeout(Duration::from_secs(10));
        let err = cmd.run().await;
        match err {
            Err(ExecError::Command { code, .. }) => assert_eq!(code, None),
            other => panic!("expected Command error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn long_output_does_not_stall() {
        // 1 MiB of stdout must be captured fully (concurrent drain).
        let cmd = Cmd::new("sh")
            .arg("-c")
            .arg("yes '012345678901234567890123456789' | head -c 1048576")
            .timeout(Duration::from_secs(30));
        let out = cmd.run().await.unwrap();
        let total: usize = out.stdout_lines.iter().map(|l| l.len() + 1).sum::<usize>();
        assert!(total >= 1048576 - 1024, "captured only {total} bytes");
    }

    #[tokio::test]
    async fn argv_reflects_all_arguments() {
        let cmd = Cmd::new("kubectl")
            .args(["--context", "my cluster", "get", "nodes"])
            .timeout(Duration::from_secs(30));
        assert_eq!(
            cmd.argv(),
            vec!["kubectl", "--context", "my cluster", "get", "nodes"]
        );
        assert_eq!(cmd.program(), "kubectl");
        assert_eq!(cmd.deadline(), Duration::from_secs(30));
    }

    #[tokio::test]
    async fn output_without_trailing_newline_is_one_line() {
        let out = Cmd::new("sh")
            .arg("-c")
            .arg("printf 'abc'")
            .run()
            .await
            .unwrap();
        assert!(out.success());
        assert_eq!(out.stdout_lines, vec!["abc".to_string()]);
    }

    #[tokio::test]
    async fn invalid_utf8_bytes_do_not_end_the_stdout_stream() {
        // Regression (review fix): `BufReader::lines()` used to end the
        // stream at the first invalid UTF-8 byte, silently losing every
        // line after it (a `docker logs` stream with binary junk). The
        // line is lossy-converted and later lines still arrive.
        let out = Cmd::new("sh")
            .arg("-c")
            .arg("printf 'ok1\\n\\377\\376\\nok2\\nok3\\n'")
            .run()
            .await
            .unwrap();
        assert!(out.success());
        assert_eq!(
            out.stdout_lines,
            vec!["ok1", "\u{fffd}\u{fffd}", "ok2", "ok3"]
        );
    }

    #[tokio::test]
    async fn forward_lines_preserves_multibyte_across_chunk_boundaries() {
        // A valid multi-byte char split across read boundaries must survive
        // intact, and an invalid byte mid-stream must not end the stream.
        let chunks = vec![
            b"h\xC3".to_vec(), // "h" + first byte of é
            b"\xA9llo\nbad\xFF\n".to_vec(),
            b"world\n".to_vec(),
        ];
        let (tx, mut rx) = mpsc::channel::<String>(8);
        forward_lines(ChunkedReader::new(chunks), tx).await;
        let mut lines = Vec::new();
        while let Some(line) = rx.recv().await {
            lines.push(line);
        }
        assert_eq!(lines, vec!["héllo", "bad\u{fffd}", "world"]);
    }

    #[tokio::test]
    async fn forward_lines_strips_crlf_and_flushes_the_final_partial_line() {
        let chunks = vec![b"a\r\nb\n".to_vec(), b"tail".to_vec()];
        let (tx, mut rx) = mpsc::channel::<String>(8);
        forward_lines(ChunkedReader::new(chunks), tx).await;
        let mut lines = Vec::new();
        while let Some(line) = rx.recv().await {
            lines.push(line);
        }
        assert_eq!(lines, vec!["a", "b", "tail"]);
    }

    #[tokio::test]
    async fn cancel_mid_stream_returns_cancelled() {
        let token = CancellationToken::new();
        let cmd = Cmd::new("sh")
            .arg("-c")
            .arg("printf 'first\\n'; sleep 30")
            .cancel(token.clone())
            .timeout(Duration::from_secs(30));
        let handle = cmd.spawn().await.unwrap();
        let task = tokio::spawn(async move {
            let mut handle = handle;
            let mut lines = Vec::new();
            while let Some(line) = handle.stdout_lines().recv().await {
                lines.push(line);
            }
            let result = handle.wait().await;
            (lines, result)
        });
        tokio::time::sleep(Duration::from_millis(400)).await;
        token.cancel();
        let (lines, result) = task.await.unwrap();
        assert_eq!(lines, vec!["first".to_string()]);
        assert!(
            matches!(result, Err(ExecError::Cancelled { .. })),
            "got {result:?}"
        );
    }

    #[tokio::test]
    async fn cancel_reports_cancelled_even_if_shell_exits_zero() {
        // A group TERM makes `sh` exit 0 (the shell defers the signal while
        // waiting on its foreground child). Cancellation must be reported as
        // Cancelled, never as a successful exit.
        let token = CancellationToken::new();
        let cmd = Cmd::new("sh")
            .arg("-c")
            .arg("printf 'x\\n'; sleep 30")
            .cancel(token.clone())
            .timeout(Duration::from_secs(60));
        let task = tokio::spawn(async move { cmd.run().await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        token.cancel();
        let result = task.await.unwrap();
        assert!(
            matches!(result, Err(ExecError::Cancelled { .. })),
            "got {result:?}"
        );
    }

    #[tokio::test]
    async fn one_second_timeout_kills_sleep_30() {
        let cmd = Cmd::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .timeout(Duration::from_secs(1));
        let started = std::time::Instant::now();
        let err = cmd.run().await;
        let elapsed = started.elapsed();
        assert!(matches!(err, Err(ExecError::Timeout { .. })), "got {err:?}");
        assert!(
            elapsed < Duration::from_secs(7),
            "timeout + kill ladder must not exceed the grace bound: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn stderr_tail_survives_non_utf8_bytes() {
        // Non-UTF-8 stderr bytes are replaced lossily, never panicking.
        let err = Cmd::new("sh")
            .arg("-c")
            .arg("printf 'boom \\377 \\376\\n' >&2; exit 2")
            .run()
            .await;
        match err {
            Err(ExecError::Command {
                code, stderr_tail, ..
            }) => {
                assert_eq!(code, Some(2));
                assert!(stderr_tail.contains("boom"), "{stderr_tail}");
            }
            other => panic!("expected Command error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dropping_a_spawned_handle_kills_the_process_group() {
        // A task abort / worker shutdown drops the handle without `wait`;
        // the Drop guard must kill the whole group (no orphan children).
        let handle = Cmd::new("sh")
            .arg("-c")
            .arg("sleep 30 & sleep 30")
            .spawn()
            .await
            .unwrap();
        let pid = handle.pid().unwrap();
        drop(handle);
        let mut dead = false;
        for _ in 0..100 {
            match nix::sys::wait::waitpid(
                nix::unistd::Pid::from_raw(pid as i32),
                Some(nix::sys::wait::WaitPidFlag::WNOHANG),
            ) {
                Ok(nix::sys::wait::WaitStatus::StillAlive) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(_) | Ok(_) => {
                    dead = true;
                    break;
                }
            }
        }
        assert!(
            dead,
            "dropping the handle must kill the child process group"
        );
    }

    /// A byte stream that returns one fixed chunk per `poll_read` call
    /// (exercises `forward_lines` chunk-boundary handling deterministically).
    struct ChunkedReader {
        chunks: std::collections::VecDeque<Vec<u8>>,
    }

    impl ChunkedReader {
        fn new(chunks: Vec<Vec<u8>>) -> Self {
            ChunkedReader {
                chunks: chunks.into(),
            }
        }
    }

    impl tokio::io::AsyncRead for ChunkedReader {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            match self.chunks.pop_front() {
                Some(chunk) => {
                    buf.put_slice(&chunk);
                    std::task::Poll::Ready(Ok(()))
                }
                None => std::task::Poll::Ready(Ok(())),
            }
        }
    }
}
