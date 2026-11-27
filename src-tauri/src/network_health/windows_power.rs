use std::cell::RefCell;
use std::ffi::c_void;
use std::io;
use std::sync::{Arc, Mutex};

use agent_core::util::LockExt;
use windows_sys::Win32::System::Power::{
    DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, HPOWERNOTIFY, PowerRegisterSuspendResumeNotification,
    PowerUnregisterSuspendResumeNotification,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND,
};

type Callback = Arc<dyn Fn(bool) + Send + Sync>;
static CALLBACK: Mutex<Option<Callback>> = Mutex::new(None);

struct PowerWatch(HPOWERNOTIFY);

impl Drop for PowerWatch {
    fn drop(&mut self) {
        // SAFETY: This token was returned by our successful registration.
        unsafe { PowerUnregisterSuspendResumeNotification(self.0) };
        CALLBACK.locked().take();
    }
}

thread_local! {
    static WATCH: RefCell<Option<PowerWatch>> = const { RefCell::new(None) };
}

unsafe extern "system" fn on_power(_: *const c_void, event: u32, _: *const c_void) -> u32 {
    let sleeping = match event {
        PBT_APMSUSPEND => true,
        // Windows also emits RESUMESUSPEND when a user is present.
        PBT_APMRESUMEAUTOMATIC => false,
        _ => return 0,
    };
    let callback = CALLBACK.locked().clone();
    if let Some(callback) = callback {
        callback(sleeping);
    }
    0
}

pub fn install_power_watch(on_change: impl Fn(bool) + Send + Sync + 'static) -> io::Result<()> {
    WATCH.set(None);
    *CALLBACK.locked() = Some(Arc::new(on_change));
    let mut params = DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
        Callback: Some(on_power),
        Context: std::ptr::null_mut(),
    };
    let mut registration = std::ptr::null_mut();
    // SAFETY: Windows copies the callback parameters during this call. The
    // callback refers only to static state, including during unregistration.
    let code = unsafe {
        PowerRegisterSuspendResumeNotification(
            DEVICE_NOTIFY_CALLBACK,
            (&raw mut params).cast(),
            &mut registration,
        )
    };
    if code != 0 {
        CALLBACK.locked().take();
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    WATCH.set(Some(PowerWatch(registration as HPOWERNOTIFY)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::PBT_APMRESUMESUSPEND;

    #[test]
    fn power_watch_accepts_one_resume_notification_and_stops_on_drop() {
        let (send, events) = std::sync::mpsc::channel();
        install_power_watch(move |sleeping| {
            let _ = send.send(sleeping);
        })
        .unwrap();
        let notify = |event| {
            // SAFETY: The registered callback uses only the event code and static state.
            unsafe { on_power(std::ptr::null(), event, std::ptr::null()) };
        };
        notify(PBT_APMSUSPEND);
        notify(PBT_APMRESUMEAUTOMATIC);
        notify(PBT_APMRESUMESUSPEND);
        assert_eq!(events.try_iter().collect::<Vec<_>>(), [true, false]);
        WATCH.set(None);
        notify(PBT_APMSUSPEND);
        assert!(events.try_recv().is_err());
    }
}
