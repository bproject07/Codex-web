use std::{
    io::{self, Read},
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

#[cfg(windows)]
use std::os::windows::{io::AsRawHandle, process::CommandExt};

use anyhow::{Context, Result, bail};

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        Threading::{CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
    },
};

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy)]
pub struct BoundedProcessOptions {
    pub timeout: Duration,
    pub stdout_limit: usize,
    pub stderr_limit: usize,
}

#[derive(Debug)]
pub struct BoundedProcessOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

pub struct ManagedProcess {
    child: Child,
    owner: ProcessTreeOwner,
}

impl ManagedProcess {
    pub fn spawn(command: &mut Command) -> Result<Self> {
        configure_private_process_tree(command);
        let owner = ProcessTreeOwner::new().context("failed to create a process tree")?;
        let mut child = command.spawn().context("failed to start managed process")?;
        if let Err(error) = owner.attach_and_start(&child) {
            owner.terminate(&mut child);
            let _ = child.wait();
            return Err(error).context("failed to contain managed process");
        }
        Ok(Self { child, owner })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub fn interrupt(&mut self) {
        #[cfg(unix)]
        if let Ok(process_group) = i32::try_from(self.child.id()) {
            // SAFETY: the managed child was placed in this exact process group
            // before it could execute.
            let _ = unsafe { libc::kill(-process_group, libc::SIGINT) };
        }
    }

    pub fn terminate_and_wait(&mut self) {
        self.owner.terminate(&mut self.child);
        let _ = self.child.wait();
    }

    pub fn terminate_descendants(&mut self) {
        self.owner.terminate(&mut self.child);
    }
}

impl Drop for ManagedProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            self.owner.terminate(&mut self.child);
            let _ = self.child.wait();
        } else {
            // A direct child can exit while descendants retain its inherited
            // resources. Always close/terminate the owned tree before release.
            self.owner.terminate(&mut self.child);
        }
    }
}

#[derive(Debug)]
struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

/// Runs a command in a private process tree and captures both output streams.
///
/// The output readers keep draining their pipes after reaching their retention
/// limits so a noisy child cannot deadlock. The process tree is terminated on
/// timeout, if inherited pipe handles outlive the direct child, and after a
/// successful run to remove any descendant the command left behind.
pub fn run_bounded(
    command: &mut Command,
    options: BoundedProcessOptions,
) -> Result<BoundedProcessOutput> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    configure_private_process_tree(command);

    let process_owner = ProcessTreeOwner::new().context("failed to create a process tree")?;
    let mut child = command.spawn().context("failed to start bounded process")?;
    if let Err(error) = process_owner.attach_and_start(&child) {
        process_owner.terminate(&mut child);
        let _ = child.wait();
        return Err(error).context("failed to contain bounded process");
    }

    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        process_owner.terminate(&mut child);
        let _ = child.wait();
        bail!("failed to capture bounded process output");
    };
    let stdout_receiver = spawn_bounded_output_reader(stdout, options.stdout_limit);
    let stderr_receiver = spawn_bounded_output_reader(stderr, options.stderr_limit);

    let deadline = Instant::now() + options.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(PROCESS_POLL_INTERVAL);
            }
            Ok(None) => {
                process_owner.terminate(&mut child);
                let _ = child.wait();
                bail!("bounded process timed out after {:?}", options.timeout);
            }
            Err(error) => {
                process_owner.terminate(&mut child);
                let _ = child.wait();
                return Err(error).context("failed to wait for bounded process");
            }
        }
    };

    let captured = (|| {
        let stdout = receive_bounded_output(&stdout_receiver, deadline, "stdout")?;
        let stderr = receive_bounded_output(&stderr_receiver, deadline, "stderr")?;
        Ok::<_, anyhow::Error>((stdout, stderr))
    })();

    // This is intentional even after a successful direct-child exit: commands
    // used for probes must not leave detached descendants running.
    process_owner.terminate(&mut child);

    let (stdout, stderr) = captured?;
    Ok(BoundedProcessOutput {
        status,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
        stdout_truncated: stdout.truncated,
        stderr_truncated: stderr.truncated,
    })
}

fn configure_private_process_tree(command: &mut Command) {
    #[cfg(windows)]
    command.creation_flags(CREATE_SUSPENDED);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;

        command.process_group(0);
    }

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt as _;

        let expected_parent = libc::pid_t::try_from(std::process::id())
            .expect("the current Linux process ID fits pid_t");
        // SAFETY: this hook calls only async-signal-safe libc functions between
        // fork and exec. Capturing the expected parent before spawning closes
        // the race where the supervisor exits before PR_SET_PDEATHSIG is set.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != expected_parent {
                    return Err(io::Error::from_raw_os_error(libc::ESRCH));
                }
                Ok(())
            });
        }
    }
}

fn read_bounded_output(mut reader: impl Read, limit: usize) -> io::Result<CapturedOutput> {
    let mut retained = Vec::with_capacity(limit.min(16 * 1024));
    let mut truncated = false;
    let mut buffer = [0_u8; 4096];
    loop {
        let length = reader.read(&mut buffer)?;
        if length == 0 {
            break;
        }

        let remaining = limit.saturating_sub(retained.len());
        let retained_length = length.min(remaining);
        retained.extend_from_slice(&buffer[..retained_length]);
        truncated |= retained_length < length;
    }
    Ok(CapturedOutput {
        bytes: retained,
        truncated,
    })
}

fn spawn_bounded_output_reader(
    reader: impl Read + Send + 'static,
    limit: usize,
) -> Receiver<io::Result<CapturedOutput>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = sender.send(read_bounded_output(reader, limit));
    });
    receiver
}

fn receive_bounded_output(
    receiver: &Receiver<io::Result<CapturedOutput>>,
    deadline: Instant,
    stream_name: &str,
) -> Result<CapturedOutput> {
    match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(output) => {
            output.with_context(|| format!("failed to read bounded process {stream_name}"))
        }
        Err(RecvTimeoutError::Timeout) => {
            bail!("bounded process did not close {stream_name} before its timeout")
        }
        Err(RecvTimeoutError::Disconnected) => {
            bail!("bounded process {stream_name} reader stopped unexpectedly")
        }
    }
}

struct ProcessTreeOwner {
    #[cfg(windows)]
    job: HANDLE,
}

impl ProcessTreeOwner {
    fn new() -> Result<Self> {
        #[cfg(windows)]
        {
            // SAFETY: null security attributes and name request a private job
            // object with default ACLs. The returned handle is owned by Self.
            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if job.is_null() {
                return Err(std::io::Error::last_os_error()).context("CreateJobObjectW failed");
            }

            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `limits` has the exact layout and byte length required by
            // JobObjectExtendedLimitInformation, and `job` is a valid handle.
            let configured = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&limits).cast(),
                    std::mem::size_of_val(&limits) as u32,
                )
            };
            if configured == 0 {
                let error = std::io::Error::last_os_error();
                // SAFETY: this branch still owns the valid job handle.
                unsafe {
                    CloseHandle(job);
                }
                return Err(error).context("SetInformationJobObject failed");
            }

            Ok(Self { job })
        }

        #[cfg(not(windows))]
        {
            Ok(Self {})
        }
    }

    fn attach_and_start(&self, child: &Child) -> Result<()> {
        #[cfg(windows)]
        {
            // SAFETY: both handles are valid for the duration of this call.
            let assigned = unsafe {
                AssignProcessToJobObject(self.job, AsRawHandle::as_raw_handle(child) as HANDLE)
            };
            if assigned == 0 {
                return Err(std::io::Error::last_os_error())
                    .context("AssignProcessToJobObject failed");
            }
            resume_suspended_process(child.id())?;
        }

        #[cfg(not(windows))]
        let _ = child;

        Ok(())
    }

    fn terminate(&self, child: &mut Child) {
        #[cfg(windows)]
        {
            // SAFETY: Self owns `job`. Terminating the job is bounded and
            // includes descendants even after their direct parent exits.
            let _ = unsafe { TerminateJobObject(self.job, 1) };
        }

        #[cfg(unix)]
        {
            // The command starts in its own process group. A negative PID
            // targets that exact group, including descendants.
            if let Ok(process_group) = i32::try_from(child.id()) {
                // SAFETY: kill receives the exact negative process-group
                // identifier assigned immediately before spawn and a constant
                // signal. Errors are handled by the direct child fallback.
                let _ = unsafe { libc::kill(-process_group, libc::SIGKILL) };
            }
        }

        let _ = child.kill();
    }
}

#[cfg(windows)]
fn resume_suspended_process(process_id: u32) -> Result<()> {
    // SAFETY: the snapshot handle is checked before use and closed on every
    // path below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error())
            .context("CreateToolhelp32Snapshot failed while resuming process");
    }

    let result = (|| {
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..THREADENTRY32::default()
        };
        // A CREATE_SUSPENDED process has exactly its primary thread at this
        // point; it cannot execute or create descendants until this succeeds.
        let mut has_entry = unsafe { Thread32First(snapshot, &mut entry) } != 0;
        while has_entry {
            if entry.th32OwnerProcessID == process_id {
                // SAFETY: the enumerated thread ID belongs to the suspended
                // process and the returned handle is checked before use.
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    return Err(std::io::Error::last_os_error())
                        .context("OpenThread failed while resuming process");
                }
                // SAFETY: `thread` grants THREAD_SUSPEND_RESUME and is valid.
                let previous_count = unsafe { ResumeThread(thread) };
                let resume_error = (previous_count == u32::MAX).then(std::io::Error::last_os_error);
                // SAFETY: this scope owns the thread handle.
                unsafe {
                    CloseHandle(thread);
                }
                if let Some(error) = resume_error {
                    return Err(error).context("ResumeThread failed for bounded process");
                }
                return Ok(());
            }
            // SAFETY: snapshot and entry remain valid for enumeration.
            has_entry = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
        }

        bail!("suspended bounded-process primary thread was not found")
    })();

    // SAFETY: this function owns the valid snapshot handle.
    unsafe {
        CloseHandle(snapshot);
    }
    result
}

#[cfg(windows)]
impl Drop for ProcessTreeOwner {
    fn drop(&mut self) {
        // Closing a kill-on-close job is the final fail-safe for all
        // descendants, including processes that retained captured pipe handles.
        // SAFETY: Self owns the handle and closes it exactly once.
        unsafe {
            CloseHandle(self.job);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Write as _, stderr, stdout},
        process::Command,
        time::{Duration, Instant},
    };

    #[cfg(target_os = "linux")]
    use std::{fs, path::PathBuf, thread};

    #[cfg(target_os = "linux")]
    use super::ManagedProcess;
    use super::{BoundedProcessOptions, run_bounded};

    const FIXTURE_ENV: &str = "CWT_PROCESS_TREE_TEST_FIXTURE";
    #[cfg(target_os = "linux")]
    const FIXTURE_PATH_ENV: &str = "CWT_PROCESS_TREE_TEST_PATH";

    fn fixture_command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("current test executable"));
        command
            .args([
                "--ignored",
                "--exact",
                "process_tree::tests::subprocess_fixture",
            ])
            .env(FIXTURE_ENV, mode);
        command
    }

    #[test]
    fn retains_only_bounded_stdout_and_stderr_while_draining_the_process() {
        let mut command = fixture_command("oversized");
        let output = run_bounded(
            &mut command,
            BoundedProcessOptions {
                timeout: Duration::from_secs(5),
                stdout_limit: 257,
                stderr_limit: 129,
            },
        )
        .expect("oversized fixture completes");

        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 257);
        assert_eq!(output.stderr.len(), 129);
        assert!(
            output.stdout.iter().filter(|byte| **byte == b'x').count() > 128,
            "the retained stdout must include the oversized fixture payload"
        );
        assert!(output.stderr.iter().all(|byte| *byte == b'y'));
        assert!(output.stdout_truncated);
        assert!(output.stderr_truncated);
    }

    #[test]
    fn timeout_terminates_the_bounded_process() {
        let mut command = fixture_command("timeout");
        let started = Instant::now();
        let error = run_bounded(
            &mut command,
            BoundedProcessOptions {
                timeout: Duration::from_millis(150),
                stdout_limit: 1024,
                stderr_limit: 1024,
            },
        )
        .expect_err("slow fixture must time out");

        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn managed_process_is_killed_when_its_supervisor_exits() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let marker = temporary.path().join("managed-child.pid");
        let status = fixture_command("pdeath-parent")
            .env(FIXTURE_PATH_ENV, &marker)
            .status()
            .expect("parent fixture starts");
        assert!(status.success(), "parent fixture failed with {status}");

        let child_pid = fs::read_to_string(&marker)
            .expect("child PID marker")
            .trim()
            .parse::<u32>()
            .expect("numeric child PID");
        let deadline = Instant::now() + Duration::from_secs(5);
        while linux_process_is_running(child_pid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }

        if linux_process_is_running(child_pid) {
            if let Ok(process_group) = i32::try_from(child_pid) {
                // SAFETY: this is test cleanup for the exact private process
                // group created by the fixture.
                let _ = unsafe { libc::kill(-process_group, libc::SIGKILL) };
            }
            panic!("managed child {child_pid} survived its supervisor");
        }
    }

    #[cfg(target_os = "linux")]
    fn linux_process_is_running(process_id: u32) -> bool {
        let Ok(stat) = fs::read_to_string(format!("/proc/{process_id}/stat")) else {
            return false;
        };
        let Some((_, fields)) = stat.rsplit_once(") ") else {
            return true;
        };
        !fields.starts_with('Z') && !fields.starts_with('X')
    }

    #[test]
    #[ignore]
    fn subprocess_fixture() {
        match std::env::var(FIXTURE_ENV).as_deref() {
            Ok("oversized") => {
                stdout()
                    .write_all(&vec![b'x'; 16 * 1024])
                    .expect("write fixture stdout");
                stderr()
                    .write_all(&vec![b'y'; 16 * 1024])
                    .expect("write fixture stderr");
            }
            Ok("timeout") => std::thread::sleep(Duration::from_secs(30)),
            #[cfg(target_os = "linux")]
            Ok("pdeath-parent") => {
                let marker =
                    PathBuf::from(std::env::var_os(FIXTURE_PATH_ENV).expect("fixture marker path"));
                let mut command = fixture_command("pdeath-child");
                command.env(FIXTURE_PATH_ENV, &marker);
                let _managed = ManagedProcess::spawn(&mut command).expect("managed child starts");
                let deadline = Instant::now() + Duration::from_secs(5);
                while !marker.exists() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(10));
                }
                assert!(marker.exists(), "managed child did not publish its PID");
                std::process::exit(0);
            }
            #[cfg(target_os = "linux")]
            Ok("pdeath-child") => {
                let marker =
                    PathBuf::from(std::env::var_os(FIXTURE_PATH_ENV).expect("fixture marker path"));
                fs::write(&marker, std::process::id().to_string()).expect("write child PID marker");
                thread::sleep(Duration::from_secs(30));
            }
            _ => {}
        }
    }
}
