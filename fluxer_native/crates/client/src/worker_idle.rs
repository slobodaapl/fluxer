//! Read-only Wayland v2 input idle notifications. No focus heuristic is used.
//! ext-idle-notify-v1.xml: only v2 ignores idle inhibitors; v1 is unavailable.
#[cfg(target_os = "linux")]
mod linux {
    use std::{
        os::fd::{AsFd, AsRawFd},
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicU32, Ordering},
        },
        time::Duration,
    };
    use tokio::sync::mpsc::UnboundedSender;
    use wayland_client::{
        Connection, Dispatch, QueueHandle, delegate_noop,
        protocol::{wl_registry, wl_seat},
    };
    use wayland_protocols::ext::idle_notify::v1::client::{
        ext_idle_notification_v1, ext_idle_notifier_v1,
    };

    #[derive(Clone, Copy)]
    enum Kind {
        Idle,
        Afk(u64),
    }
    struct State {
        notifier: Option<ext_idle_notifier_v1::ExtIdleNotifierV1>,
        seat: Option<wl_seat::WlSeat>,
        notifier_name: Option<u32>,
        seat_name: Option<u32>,
        removed: bool,
        idle: bool,
        afk: bool,
        afk_generation: u64,
        tx: UnboundedSender<Option<(bool, bool)>>,
    }
    impl Dispatch<wl_registry::WlRegistry, ()> for State {
        fn event(
            s: &mut Self,
            r: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            q: &QueueHandle<Self>,
        ) {
            match event {
                wl_registry::Event::Global {
                    name,
                    interface,
                    version,
                } if interface == "ext_idle_notifier_v1"
                    && version >= 2
                    && s.notifier.is_none() =>
                {
                    s.notifier = Some(r.bind(name, 2, q, ()));
                    s.notifier_name = Some(name);
                }
                wl_registry::Event::Global {
                    name,
                    interface,
                    version,
                } if interface == "wl_seat" && s.seat.is_none() => {
                    s.seat = Some(r.bind(name, version.min(1), q, ()));
                    s.seat_name = Some(name);
                }
                wl_registry::Event::GlobalRemove { name }
                    if Some(name) == s.notifier_name || Some(name) == s.seat_name =>
                {
                    s.removed = true
                }
                _ => {}
            }
        }
    }
    delegate_noop!(State: ignore wl_seat::WlSeat);
    delegate_noop!(State: ignore ext_idle_notifier_v1::ExtIdleNotifierV1);
    impl Dispatch<ext_idle_notification_v1::ExtIdleNotificationV1, Kind> for State {
        fn event(
            s: &mut Self,
            _: &ext_idle_notification_v1::ExtIdleNotificationV1,
            e: ext_idle_notification_v1::Event,
            kind: &Kind,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            let active = match e {
                ext_idle_notification_v1::Event::Idled => true,
                ext_idle_notification_v1::Event::Resumed => false,
                _ => return,
            };
            if apply(*kind, active, s.afk_generation, &mut s.idle, &mut s.afk) {
                let _ = s.tx.send(Some((s.idle, s.afk)));
            }
        }
    }
    fn apply(kind: Kind, active: bool, generation: u64, idle: &mut bool, afk: &mut bool) -> bool {
        match kind {
            Kind::Idle => *idle = active,
            Kind::Afk(current) if current == generation => *afk = active,
            Kind::Afk(_) => return false,
        }
        true
    }
    pub struct Watch {
        stop: Arc<AtomicBool>,
        pub timeout: Arc<AtomicU32>,
    }
    impl Drop for Watch {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }
    pub fn start(tx: UnboundedSender<Option<(bool, bool)>>) -> Watch {
        let stop = Arc::new(AtomicBool::new(false));
        let timeout = Arc::new(AtomicU32::new(600_000));
        let (end, setting) = (stop.clone(), timeout.clone());
        std::thread::spawn(move || {
            while !end.load(Ordering::Relaxed) {
                let _ = run(&tx, &end, &setting);
                let _ = tx.send(None);
                for _ in 0..10 {
                    if end.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        });
        Watch { stop, timeout }
    }
    fn run(
        tx: &UnboundedSender<Option<(bool, bool)>>,
        stop: &AtomicBool,
        setting: &AtomicU32,
    ) -> Option<()> {
        let conn = Connection::connect_to_env().ok()?;
        let mut queue = conn.new_event_queue();
        let q = queue.handle();
        conn.display().get_registry(&q, ());
        let mut s = State {
            notifier: None,
            seat: None,
            notifier_name: None,
            seat_name: None,
            removed: false,
            idle: false,
            afk: false,
            afk_generation: 0,
            tx: tx.clone(),
        };
        queue.roundtrip(&mut s).ok()?;
        let notifier = s.notifier.clone()?;
        let seat = s.seat.clone()?;
        let mut timeout = setting.load(Ordering::Relaxed);
        let _idle = notifier.get_input_idle_notification(600_000, &seat, &q, Kind::Idle);
        let mut afk = notifier.get_input_idle_notification(timeout, &seat, &q, Kind::Afk(0));
        conn.flush().ok()?;
        let _ = tx.send(Some((false, false)));
        while !stop.load(Ordering::Relaxed) && !s.removed {
            queue.dispatch_pending(&mut s).ok()?;
            let next = setting.load(Ordering::Relaxed);
            if next != timeout {
                afk.destroy();
                s.afk_generation = s.afk_generation.wrapping_add(1);
                timeout = next;
                s.afk = false;
                let _ = tx.send(Some((s.idle, false)));
                afk = notifier.get_input_idle_notification(
                    timeout,
                    &seat,
                    &q,
                    Kind::Afk(s.afk_generation),
                );
                conn.flush().ok()?;
            }
            let mut fd = libc::pollfd {
                fd: conn.as_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut fd, 1, 1000) };
            if ready < 0 || fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                return None;
            }
            if ready > 0 {
                queue.blocking_dispatch(&mut s).ok()?;
            }
        }
        None
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn idle_resume_and_replaced_afk_notifications_are_fenced() {
            let (mut idle, mut afk) = (false, false);
            assert!(apply(Kind::Idle, true, 1, &mut idle, &mut afk));
            assert!(!apply(Kind::Afk(0), true, 1, &mut idle, &mut afk));
            assert_eq!((idle, afk), (true, false));
            assert!(apply(Kind::Afk(1), true, 1, &mut idle, &mut afk));
            assert!(apply(Kind::Idle, false, 1, &mut idle, &mut afk));
            assert!(apply(Kind::Afk(1), false, 1, &mut idle, &mut afk));
            assert_eq!((idle, afk), (false, false));
        }
    }
}
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(not(target_os = "linux"))]
mod unsupported {
    use std::sync::{Arc, atomic::AtomicU32};
    pub struct Watch {
        pub timeout: Arc<AtomicU32>,
        _tx: tokio::sync::mpsc::UnboundedSender<Option<(bool, bool)>>,
    }
    pub fn start(tx: tokio::sync::mpsc::UnboundedSender<Option<(bool, bool)>>) -> Watch {
        Watch {
            timeout: Arc::new(AtomicU32::new(600_000)),
            _tx: tx,
        }
    }
}
#[cfg(not(target_os = "linux"))]
pub use unsupported::*;
