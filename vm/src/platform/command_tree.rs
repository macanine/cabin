use std::io;
use std::process::{Child, Command, ExitStatus};

/// A subprocess supervised together with the processes it launches.
pub struct CommandTree {
    child: Child,
    reaped: bool,
    #[cfg(windows)]
    job: std::os::windows::io::OwnedHandle,
}

impl CommandTree {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(command, 0);
        #[cfg(windows)]
        let job = windows::prepare(command)?;
        let child = command.spawn()?;
        let tree = Self {
            child,
            reaped: false,
            #[cfg(windows)]
            job,
        };
        #[cfg(windows)]
        windows::start(&tree.job, &tree.child)?;
        Ok(tree)
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        self.reaped |= status.is_some();
        Ok(status)
    }

    pub fn terminate(&mut self) {
        #[cfg(windows)]
        windows::terminate(&self.job);
        if !self.reaped {
            #[cfg(unix)]
            if let Ok(pid) = libc::pid_t::try_from(self.child.id()) {
                // SAFETY: This unreaped child is the leader of its own group.
                unsafe { libc::kill(-pid, libc::SIGKILL) };
            }
            let _ = self.child.kill();
        }
    }
}

impl Drop for CommandTree {
    fn drop(&mut self) {
        if !self.reaped {
            self.terminate();
            let _ = self.child.wait();
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::os::windows::process::{ChildExt, CommandExt};
    use std::process::{Child, Command};
    use std::ptr::null;

    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED, ResumeThread};

    pub(super) fn prepare(command: &mut Command) -> io::Result<OwnedHandle> {
        // Assign the process before it can run, so an early descendant cannot
        // inherit a pipe and escape the job.
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        // SAFETY: Null arguments select default security and an unnamed job.
        let handle = unsafe { CreateJobObjectW(null(), null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a new owned handle.
        let job = unsafe { OwnedHandle::from_raw_handle(handle) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: The handle and the correctly sized information struct remain live.
        if unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    pub(super) fn start(job: &OwnedHandle, child: &Child) -> io::Result<()> {
        // SAFETY: Both handles are borrowed from their owners for the call.
        if unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) } == 0 {
            return Err(io::Error::other(format!(
                "could not assign process to its job: {}",
                io::Error::last_os_error()
            )));
        }
        // SAFETY: CREATE_SUSPENDED left this borrowed primary thread suspended.
        if unsafe { ResumeThread(child.main_thread_handle().as_raw_handle()) } == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn terminate(job: &OwnedHandle) {
        // SAFETY: The job handle is live for the call. Closing it also kills members.
        unsafe { TerminateJobObject(job.as_raw_handle(), 1) };
    }
}
