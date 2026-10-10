use std::{
    ffi::c_void,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
    thread::{ThreadId, current},
    time::Duration,
};

use anyhow::Context;
use gpui_util::ResultExt;
use windows::Win32::{
    Foundation::{FILETIME, LPARAM, WPARAM},
    Media::{timeBeginPeriod, timeEndPeriod},
    System::Threading::{
        CloseThreadpoolTimer, CreateThreadpoolTimer, GetCurrentThread, PTP_CALLBACK_INSTANCE,
        PTP_TIMER, SetThreadPriority, SetThreadpoolTimer, THREAD_PRIORITY_TIME_CRITICAL,
        TP_CALLBACK_ENVIRON_V3, TP_CALLBACK_PRIORITY, TP_CALLBACK_PRIORITY_HIGH,
        TP_CALLBACK_PRIORITY_LOW, TP_CALLBACK_PRIORITY_NORMAL, TrySubmitThreadpoolCallback,
    },
    UI::WindowsAndMessaging::PostMessageW,
};

use crate::{HWND, SafeHwnd, WM_GPUI_TASK_DISPATCHED_ON_MAIN_THREAD};
use gpui::{
    PlatformDispatcher, Priority, PriorityQueueSender, RunnableVariant, TimerResolutionGuard,
};

pub(crate) struct WindowsDispatcher {
    pub(crate) wake_posted: AtomicBool,
    main_sender: PriorityQueueSender<RunnableVariant>,
    main_thread_id: ThreadId,
    pub(crate) platform_window_handle: SafeHwnd,
    validation_number: usize,
}

impl WindowsDispatcher {
    pub(crate) fn new(
        main_sender: PriorityQueueSender<RunnableVariant>,
        platform_window_handle: HWND,
        validation_number: usize,
    ) -> Self {
        let main_thread_id = current().id();
        let platform_window_handle = platform_window_handle.into();

        WindowsDispatcher {
            main_sender,
            main_thread_id,
            platform_window_handle,
            validation_number,
            wake_posted: AtomicBool::new(false),
        }
    }

    fn dispatch_on_threadpool(&self, priority: TP_CALLBACK_PRIORITY, runnable: RunnableVariant) {
        let environ = TP_CALLBACK_ENVIRON_V3 {
            Version: 3,
            CallbackPriority: priority,
            Size: size_of::<TP_CALLBACK_ENVIRON_V3>() as u32,
            ..Default::default()
        };

        // If the thread pool never runs our callback, the matching `from_raw` is never called, which leaks the runnable.
        // Dropping the scheduled runnable would cancel its task and make the next poll of any awaiter panic. Since we expect
        // the scenario to usually happen during shutdown, this leak is acceptable.
        let context = runnable.into_raw().as_ptr() as *mut c_void;

        unsafe {
            TrySubmitThreadpoolCallback(Some(run_work_callback), Some(context), Some(&environ))
                .log_err();
        }
    }

    fn dispatch_on_threadpool_after(&self, runnable: RunnableVariant, duration: Duration) {
        let context = runnable.into_raw().as_ptr() as *mut c_void;

        unsafe {
            if let Ok(timer) = CreateThreadpoolTimer(Some(run_timer_callback), Some(context), None)
            {
                // Negative FILETIME expresses a relative delay in 100ns ticks
                let ticks = (duration.as_nanos() / 100).min(i64::MAX as u128) as i64;
                let due = (-ticks) as u64;
                let due_time = FILETIME {
                    dwLowDateTime: due as u32,
                    dwHighDateTime: (due >> 32) as u32,
                };
                SetThreadpoolTimer(timer, Some(&due_time), 0, None);
            }
        }
    }

    #[inline(always)]
    pub(crate) fn execute_runnable(runnable: RunnableVariant) {
        let location = runnable.metadata().location;
        let spawned = runnable.metadata().spawned;
        gpui::profiler::update_running_task(spawned, location);
        runnable.run();
        gpui::profiler::save_task_timing();
    }

    // Mirrors the `wake_posted` handling in `WindowsPlatformState::run_foreground_task`
    // (platform.rs). `try_post` only runs when we are the thread that flips the flag
    // from false to true, so if it fails we must roll the flag back; otherwise the
    // runnable just enqueued in `main_sender` is left with no one posting the wake
    // message, since every later dispatch would see `wake_posted == true` and skip it.
    fn notify_main_thread(wake_posted: &AtomicBool, try_post: impl FnOnce() -> bool) {
        if !wake_posted.swap(true, Ordering::AcqRel) && !try_post() {
            wake_posted.store(false, Ordering::Release);
        }
    }
}

impl PlatformDispatcher for WindowsDispatcher {
    fn is_main_thread(&self) -> bool {
        current().id() == self.main_thread_id
    }

    fn dispatch(&self, runnable: RunnableVariant, priority: Priority) {
        let priority = match priority {
            Priority::RealtimeAudio => {
                panic!("RealtimeAudio priority should use spawn_realtime, not dispatch")
            }
            Priority::High => TP_CALLBACK_PRIORITY_HIGH,
            Priority::Medium => TP_CALLBACK_PRIORITY_NORMAL,
            Priority::Low => TP_CALLBACK_PRIORITY_LOW,
        };
        self.dispatch_on_threadpool(priority, runnable);
    }

    fn dispatch_on_main_thread(&self, runnable: RunnableVariant, priority: Priority) {
        match self.main_sender.send(priority, runnable) {
            Ok(_) => {
                Self::notify_main_thread(&self.wake_posted, || unsafe {
                    PostMessageW(
                        Some(self.platform_window_handle.as_raw()),
                        WM_GPUI_TASK_DISPATCHED_ON_MAIN_THREAD,
                        WPARAM(self.validation_number),
                        LPARAM(0),
                    )
                    .log_err()
                    .is_some()
                });
            }
            Err(runnable) => {
                // NOTE: Runnable may wrap a Future that is !Send.
                //
                // This is usually safe because we only poll it on the main thread.
                // However if the send fails, we know that:
                // 1. main_receiver has been dropped (which implies the app is shutting down)
                // 2. we are on a background thread.
                // It is not safe to drop something !Send on the wrong thread, and
                // the app will exit soon anyway, so we must forget the runnable.
                std::mem::forget(runnable);
            }
        }
    }

    fn dispatch_after(&self, duration: Duration, runnable: RunnableVariant) {
        self.dispatch_on_threadpool_after(runnable, duration);
    }

    fn spawn_realtime(&self, f: Box<dyn FnOnce() + Send>) {
        std::thread::spawn(move || {
            // SAFETY: always safe to call
            let thread_handle = unsafe { GetCurrentThread() };

            // SAFETY: thread_handle is a valid handle to the current thread
            unsafe { SetThreadPriority(thread_handle, THREAD_PRIORITY_TIME_CRITICAL) }
                .context("thread priority")
                .log_err();

            f();
        });
    }

    fn increase_timer_resolution(&self) -> TimerResolutionGuard {
        unsafe {
            timeBeginPeriod(1);
        }
        gpui_util::defer(Box::new(|| unsafe {
            timeEndPeriod(1);
        }))
    }
}

unsafe extern "system" fn run_work_callback(
    _instance: PTP_CALLBACK_INSTANCE,
    context: *mut c_void,
) {
    let runnable = unsafe { RunnableVariant::from_raw(NonNull::new_unchecked(context as *mut ())) };
    WindowsDispatcher::execute_runnable(runnable);
}

unsafe extern "system" fn run_timer_callback(
    _instance: PTP_CALLBACK_INSTANCE,
    context: *mut c_void,
    timer: PTP_TIMER,
) {
    let runnable = unsafe { RunnableVariant::from_raw(NonNull::new_unchecked(context as *mut ())) };
    WindowsDispatcher::execute_runnable(runnable);
    unsafe { CloseThreadpoolTimer(timer) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hane_main_thread_wake_retry_resets_flag_when_post_fails() {
        let wake_posted = AtomicBool::new(false);

        WindowsDispatcher::notify_main_thread(&wake_posted, || false);

        assert!(
            !wake_posted.load(Ordering::Acquire),
            "a failed notification must not leave wake_posted stuck true, \
             otherwise a later dispatch_on_main_thread call would skip posting \
             and the queued runnable would never be woken up"
        );
    }

    #[test]
    fn hane_main_thread_wake_retry_retries_after_a_previous_failure() {
        let wake_posted = AtomicBool::new(false);

        WindowsDispatcher::notify_main_thread(&wake_posted, || false);
        assert!(!wake_posted.load(Ordering::Acquire));

        let mut posted = false;
        WindowsDispatcher::notify_main_thread(&wake_posted, || {
            posted = true;
            true
        });

        assert!(posted, "the next dispatch must retry posting the wake message");
        assert!(wake_posted.load(Ordering::Acquire));
    }

    #[test]
    fn hane_main_thread_wake_retry_skips_post_when_already_pending() {
        let wake_posted = AtomicBool::new(true);

        WindowsDispatcher::notify_main_thread(&wake_posted, || {
            panic!("must not post again while a wake message is already pending")
        });

        assert!(wake_posted.load(Ordering::Acquire));
    }
}
