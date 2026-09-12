//! Log watching (ADR-0012): bounded ring buffers + follow-mode streaming
//! from `docker logs -f` / `kubectl logs -f` through the exec runner.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use crate::error::Result;
use crate::kindctl::KindCommand;

/// Default ring capacity (lines).
pub const DEFAULT_LOG_RING_CAP: usize = 2000;

/// A bounded ring buffer of log lines; oldest lines are evicted first.
#[derive(Debug, Clone)]
pub struct LogRing {
    lines: VecDeque<String>,
    cap: usize,
}

impl LogRing {
    /// Create a ring with the given capacity (clamped to ≥ 1).
    pub fn new(cap: usize) -> Self {
        LogRing {
            lines: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    /// Current capacity.
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Number of buffered lines.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether the ring is empty.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Append a line, evicting the oldest when full.
    pub fn push(&mut self, line: impl Into<String>) {
        self.lines.push_back(line.into());
        while self.lines.len() > self.cap {
            self.lines.pop_front();
        }
    }

    /// Append multiple lines in order.
    pub fn extend<I, S>(&mut self, lines: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for line in lines {
            self.push(line);
        }
    }

    /// A snapshot of the buffered lines, oldest first.
    pub fn snapshot(&self) -> Vec<String> {
        self.lines.iter().cloned().collect()
    }

    /// Clear the buffer.
    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

impl Default for LogRing {
    /// A ring with the default capacity ([`DEFAULT_LOG_RING_CAP`]); a
    /// derived default would leave `cap = 0` and evict every pushed line.
    fn default() -> Self {
        LogRing::new(DEFAULT_LOG_RING_CAP)
    }
}

/// Which log backend to read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogSource {
    /// Node-level logs from a kind node container
    /// (`docker logs [--follow] <container>`).
    Docker {
        /// Container name (e.g. `demo-control-plane`).
        container: String,
    },
    /// Workload logs via kubectl.
    Kubectl {
        /// Kubeconfig context.
        context: String,
        /// Pod name.
        pod: String,
        /// Namespace.
        ns: String,
        /// Container within the pod.
        container: Option<String>,
    },
}

impl LogSource {
    /// The [`KindCommand`] for a follow-mode stream.
    pub fn follow_command(&self) -> KindCommand {
        match self {
            LogSource::Docker { container } => KindCommand::DockerLogs {
                container: container.clone(),
                follow: true,
                tail: None,
            },
            LogSource::Kubectl {
                context,
                pod,
                ns,
                container,
            } => KindCommand::KubectlLogs {
                context: context.clone(),
                pod: pod.clone(),
                ns: ns.clone(),
                container: container.clone(),
                follow: true,
                tail: None,
            },
        }
    }

    /// The [`KindCommand`] for a one-shot read of the last `tail` lines.
    pub fn tail_command(&self, tail: Option<u32>) -> KindCommand {
        match self {
            LogSource::Docker { container } => KindCommand::DockerLogs {
                container: container.clone(),
                follow: false,
                tail,
            },
            LogSource::Kubectl {
                context,
                pod,
                ns,
                container,
            } => KindCommand::KubectlLogs {
                context: context.clone(),
                pod: pod.clone(),
                ns: ns.clone(),
                container: container.clone(),
                follow: false,
                tail,
            },
        }
    }
}

/// Stream logs in follow mode into a shared ring until the stream ends or
/// the token fires (cancellation is a *normal* end — it returns `Ok`).
///
/// The ring is bounded by its own capacity, so a runaway stream can never
/// grow memory (ADR-0012).
pub async fn watch_logs(
    source: &LogSource,
    ring: Arc<Mutex<LogRing>>,
    cancel: CancellationToken,
) -> Result<()> {
    let command = source.follow_command();
    let mut cmd = command.to_cmd().cancel(cancel.clone());
    // Streams have no per-call deadline; the cancellation token is the
    // bound (plus the exec kill ladder if the token fires).
    cmd = cmd.timeout(std::time::Duration::from_secs(3600 * 24));
    let mut handle = cmd.spawn().await?;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                handle.terminate_group();
                let _ = handle.wait().await; // Cancelled is a normal end.
                return Ok(());
            }
            line = handle.stdout_lines().recv() => {
                match line {
                    Some(line) => {
                        if let Ok(mut ring) = ring.lock() {
                            ring.push(line);
                        }
                    }
                    None => break,
                }
            }
        }
    }
    handle.wait().await?;
    Ok(())
}

/// Default line cap for a one-shot log read when the caller passes no tail
/// (bounds `docker logs`/`kubectl logs` output collected into memory).
pub const DEFAULT_ONE_SHOT_TAIL: u32 = 2000;

/// One-shot read of the current log tail (no follow).
///
/// `tail == None` defaults to [`DEFAULT_ONE_SHOT_TAIL`] so an unbounded
/// one-shot read can never materialize the full container log in memory.
pub async fn read_logs(source: &LogSource, tail: Option<u32>) -> Result<Vec<String>> {
    let output = source
        .tail_command(Some(tail.unwrap_or(DEFAULT_ONE_SHOT_TAIL)))
        .to_cmd()
        .run()
        .await?;
    Ok(output.stdout_lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::Cmd;

    #[test]
    fn ring_evicts_oldest_first() {
        let mut ring = LogRing::new(3);
        for i in 0..5 {
            ring.push(format!("line {i}"));
        }
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.snapshot(), vec!["line 2", "line 3", "line 4"]);
    }

    #[test]
    fn ring_capacity_invariant() {
        let mut ring = LogRing::new(10);
        for i in 0..1000 {
            ring.push(i.to_string());
        }
        assert!(ring.len() <= ring.capacity());
        assert_eq!(ring.len(), 10);
        assert_eq!(ring.snapshot().first().unwrap(), "990");
    }

    #[test]
    fn ring_capacity_clamped_to_one() {
        let mut ring = LogRing::new(0);
        ring.push("a");
        ring.push("b");
        assert_eq!(ring.capacity(), 1);
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.snapshot(), vec!["b"]);
    }

    #[test]
    fn ring_extend_and_clear() {
        let mut ring = LogRing::new(5);
        ring.extend(vec!["a", "b", "c"]);
        assert_eq!(ring.len(), 3);
        ring.clear();
        assert!(ring.is_empty());
    }

    #[test]
    fn ring_default_has_capacity_and_buffers() {
        let mut ring = LogRing::default();
        assert_eq!(ring.capacity(), DEFAULT_LOG_RING_CAP);
        for i in 0..5000 {
            ring.push(i.to_string());
        }
        assert_eq!(ring.len(), DEFAULT_LOG_RING_CAP);
        assert_eq!(ring.snapshot().first().unwrap(), "3000");
        assert_eq!(ring.snapshot().last().unwrap(), "4999");
        assert!(!ring.is_empty());
    }

    #[test]
    fn docker_source_commands() {
        let source = LogSource::Docker {
            container: "demo-control-plane".into(),
        };
        let (prog, args) = source.follow_command().to_program_and_args();
        assert_eq!(prog, "docker");
        assert_eq!(args, vec!["logs", "--follow", "demo-control-plane"]);
        let (_, args) = source.tail_command(Some(100)).to_program_and_args();
        assert_eq!(args, vec!["logs", "--tail", "100", "demo-control-plane"]);
    }

    #[test]
    fn kubectl_source_commands() {
        let source = LogSource::Kubectl {
            context: "kind-demo".into(),
            pod: "web-0".into(),
            ns: "default".into(),
            container: None,
        };
        let (prog, args) = source.follow_command().to_program_and_args();
        assert_eq!(prog, "kubectl");
        assert_eq!(
            args,
            vec![
                "--context",
                "kind-demo",
                "logs",
                "web-0",
                "-n",
                "default",
                "-f"
            ]
        );
        let (_, args) = source.tail_command(Some(50)).to_program_and_args();
        assert_eq!(
            args,
            vec![
                "--context",
                "kind-demo",
                "logs",
                "web-0",
                "-n",
                "default",
                "--tail",
                "50"
            ]
        );
    }

    #[tokio::test]
    async fn watch_logs_streams_lines_into_ring() {
        let ring = Arc::new(Mutex::new(LogRing::new(2000)));
        let cancel = CancellationToken::new();
        let _source = LogSource::Docker {
            container: "x".into(),
        };
        // watch_logs uses docker — not present in tests. Instead exercise
        // the same plumbing via the exec runner with a real command: build
        // a Cmd directly and push lines the way watch_logs does.
        let mut cmd = Cmd::new("sh")
            .arg("-c")
            .arg("printf 'a\\nb\\nc\\n'")
            .cancel(cancel.clone());
        cmd = cmd.timeout(std::time::Duration::from_secs(10));
        let mut handle = cmd.spawn().await.unwrap();
        while let Some(line) = handle.stdout_lines().recv().await {
            if let Ok(mut ring) = ring.lock() {
                ring.push(line);
            }
        }
        handle.wait().await.unwrap();
        {
            let ring = ring.lock().unwrap();
            assert_eq!(ring.snapshot(), vec!["a", "b", "c"]);
        }

        // And cancellation of a running stream kills the group.
        let ring2 = Arc::new(Mutex::new(LogRing::new(2000)));
        let cancel2 = CancellationToken::new();
        let cmd2 = Cmd::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .cancel(cancel2.clone())
            .timeout(std::time::Duration::from_secs(30));
        let task = tokio::spawn(async move {
            let handle = cmd2.spawn().await.unwrap();
            handle.wait().await
        });
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        cancel2.cancel();
        let result = task.await.unwrap();
        assert!(result.is_err(), "cancelled run must error: {result:?}");
        let _ = ring2;
    }
}
