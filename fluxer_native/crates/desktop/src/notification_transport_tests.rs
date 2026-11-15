// SPDX-License-Identifier: AGPL-3.0-or-later
//! All traffic stays on an owned private bus; never use the user's session bus.
use super::{Alert, Command, Feedback, NAME, PATH, Target, Transport};
use fluxer_core::Snowflake;
use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    process::{Child, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
struct Bus {
    child: Child,
    address: String,
}
impl Bus {
    fn new() -> Self {
        let mut child = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("private dbus-daemon");
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        Self {
            child,
            address: address.trim().into(),
        }
    }
}
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
#[derive(Debug)]
struct Payload {
    app: String,
    replaces: u32,
    icon: String,
    title: String,
    body: String,
    actions: Vec<String>,
    hints: HashMap<String, zbus::zvariant::OwnedValue>,
    expires: i32,
}
struct Daemon {
    shown: mpsc::Sender<Payload>,
    closed: mpsc::Sender<u32>,
    next: Arc<Mutex<u32>>,
    immediate_click: bool,
    hang: bool,
}
#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Daemon {
    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        app: String,
        replaces: u32,
        icon: String,
        title: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, zbus::zvariant::OwnedValue>,
        expires: i32,
        #[zbus(signal_emitter)] emitter: zbus::object_server::SignalEmitter<'_>,
    ) -> u32 {
        let id = {
            let mut next = self.next.lock().unwrap();
            let id = *next;
            *next += 1;
            id
        };
        self.shown
            .send(Payload {
                app,
                replaces,
                icon,
                title,
                body,
                actions,
                hints,
                expires,
            })
            .unwrap();
        if self.hang {
            std::future::pending::<()>().await;
        }
        if self.immediate_click {
            emitter
                .emit(NAME, "ActionInvoked", &(id, "default"))
                .await
                .unwrap();
        }
        id
    }
    fn close_notification(&self, id: u32) {
        self.closed.send(id).unwrap();
    }
}
struct Service {
    connection: zbus::blocking::Connection,
    shown: mpsc::Receiver<Payload>,
    closed: mpsc::Receiver<u32>,
    next: Arc<Mutex<u32>>,
}
impl Service {
    fn new(bus: &Bus, immediate_click: bool) -> Self {
        Self::with_mode(bus, immediate_click, false)
    }
    fn with_mode(bus: &Bus, immediate_click: bool, hang: bool) -> Self {
        let (shown, shown_rx) = mpsc::channel();
        let (closed, closed_rx) = mpsc::channel();
        let next = Arc::new(Mutex::new(1));
        let connection = zbus::blocking::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name(NAME)
            .unwrap()
            .serve_at(
                PATH,
                Daemon {
                    shown,
                    closed,
                    next: next.clone(),
                    immediate_click,
                    hang,
                },
            )
            .unwrap()
            .build()
            .unwrap();
        Self {
            connection,
            shown: shown_rx,
            closed: closed_rx,
            next,
        }
    }
    fn signal(&self, member: &str, id: u32) {
        match member {
            "ActionInvoked" => self
                .connection
                .emit_signal(None::<&str>, PATH, NAME, member, &(id, "default"))
                .unwrap(),
            _ => self
                .connection
                .emit_signal(None::<&str>, PATH, NAME, member, &(id, 2u32))
                .unwrap(),
        }
    }
    fn shown(&self) -> Payload {
        self.shown
            .recv_timeout(Duration::from_secs(5))
            .expect("Notify payload")
    }
}
fn target(message: u64) -> Target {
    Target {
        account: Snowflake(1),
        epoch: 7,
        channel_epoch: 0,
        channel: Snowflake(2),
        message: Snowflake(message),
    }
}
fn alert(message: u64) -> Alert {
    Alert {
        target: target(message),
        title: "Private fixture title".into(),
        body: "Safe &amp; body".into(),
    }
}
fn count(transport: &mut Transport) -> usize {
    let (tx, rx) = mpsc::channel();
    transport.send(Command::Inspect(tx));
    rx.recv_timeout(Duration::from_secs(5)).unwrap()
}
fn wait_count(transport: &mut Transport, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if count(transport) == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "notification count did not reach {expected}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn click(events: &mpsc::Receiver<Feedback>) -> Target {
    match events.recv_timeout(Duration::from_secs(5)).unwrap() {
        Feedback::Click(target) => target,
        Feedback::Failure(error) => panic!("{error}"),
        _ => panic!("unexpected feedback"),
    }
}
#[test]
fn private_bus_notify_click_closed_clear_owner_replacement_and_teardown() {
    let bus = Bus::new();
    let first = Service::new(&bus, false);
    let (tx, events) = mpsc::sync_channel(256);
    let mut transport = Transport::at(tx, Some(bus.address.clone()));
    transport.show(alert(10));
    let payload = first.shown();
    assert_eq!(payload.app, "Fluxer");
    assert_eq!(payload.replaces, 0);
    assert_eq!(payload.icon, "fluxer");
    assert_eq!(payload.title, "Private fixture title");
    assert_eq!(payload.body, "Safe &amp; body");
    assert_eq!(payload.actions, ["default", "Open message"]);
    assert_eq!(payload.expires, -1);
    assert!(bool::try_from(payload.hints.get("suppress-sound").unwrap()).unwrap());
    assert_eq!(
        <&str>::try_from(payload.hints.get("desktop-entry").unwrap()).unwrap(),
        "app.fluxer.Native"
    );
    wait_count(&mut transport, 1);
    first.signal("ActionInvoked", 1);
    assert_eq!(click(&events), target(10));
    wait_count(&mut transport, 0);
    transport.show(alert(11));
    first.shown();
    wait_count(&mut transport, 1);
    first.signal("NotificationClosed", 2);
    wait_count(&mut transport, 0);
    first.signal("ActionInvoked", 2);
    transport.show(alert(12));
    first.shown();
    wait_count(&mut transport, 1);
    transport.clear(Snowflake(1), Some(Snowflake(2)));
    assert_eq!(
        first.closed.recv_timeout(Duration::from_secs(5)).unwrap(),
        3
    );
    wait_count(&mut transport, 0);
    first.signal("ActionInvoked", 3);
    transport.show(alert(13));
    first.shown();
    wait_count(&mut transport, 1);
    first.connection.release_name(NAME).unwrap();
    let second = Service::new(&bus, false);
    // Both daemons issue ID 1, but old-owner callbacks must not use the new map.
    *second.next.lock().unwrap() = 1;
    transport.show(alert(14));
    second.shown();
    wait_count(&mut transport, 1);
    first.signal("ActionInvoked", 4);
    first.signal("ActionInvoked", 1);
    second.signal("NotificationClosed", 999);
    second.signal("ActionInvoked", 1);
    assert_eq!(click(&events), target(14));
    wait_count(&mut transport, 0);
    assert!(events.try_recv().is_err());
    transport.show(alert(15));
    second.shown();
    wait_count(&mut transport, 1);
    second.connection.release_name(NAME).unwrap();
    transport.clear(Snowflake(1), None);
    wait_count(&mut transport, 0);
    let start = Instant::now();
    drop(transport);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "transport teardown blocked"
    );
}
#[test]
fn signal_subscription_is_ready_before_notify_and_drop_cancels_idle_listener() {
    let bus = Bus::new();
    let service = Service::new(&bus, true);
    let (tx, events) = mpsc::sync_channel(256);
    let mut transport = Transport::at(tx, Some(bus.address.clone()));
    transport.show(alert(99));
    service.shown();
    assert_eq!(click(&events), target(99));
    let start = Instant::now();
    drop(transport);
    assert!(start.elapsed() < Duration::from_secs(2));
    // The worker's connection has been dropped; no detached signal loop remains.
    assert!(matches!(
        events.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
}

#[test]
fn channel_capacity_is_five_and_clear_all_closes_every_remaining_id() {
    let bus = Bus::new();
    let service = Service::new(&bus, false);
    let (tx, events) = mpsc::sync_channel(256);
    let mut transport = Transport::at(tx, Some(bus.address.clone()));
    for message in 1..=6 {
        transport.show(alert(message));
        service.shown();
    }
    wait_count(&mut transport, 5);
    assert_eq!(
        service.closed.recv_timeout(Duration::from_secs(5)).unwrap(),
        1
    );
    transport.clear_all();
    wait_count(&mut transport, 0);
    let mut closed: Vec<_> = (0..5)
        .map(|_| service.closed.recv_timeout(Duration::from_secs(5)).unwrap())
        .collect();
    closed.sort();
    assert_eq!(closed, vec![2, 3, 4, 5, 6]);
    assert!(events.try_recv().is_err());
}
#[test]
fn dropping_transport_cancels_a_stalled_notify_call() {
    let bus = Bus::new();
    let service = Service::with_mode(&bus, false, true);
    let (tx, events) = mpsc::sync_channel(256);
    let mut transport = Transport::at(tx, Some(bus.address.clone()));
    let names = || {
        zbus::blocking::fdo::DBusProxy::new(&service.connection)
            .unwrap()
            .list_names()
            .unwrap()
            .into_iter()
            .filter(|name| name.as_str().starts_with(':'))
            .count()
    };
    let before = names();
    transport.show(alert(1));
    service.shown();
    assert_eq!(names(), before + 1);
    let start = Instant::now();
    drop(transport);
    assert!(start.elapsed() < Duration::from_secs(2));
    let deadline = Instant::now() + Duration::from_secs(2);
    while names() != before {
        assert!(
            Instant::now() < deadline,
            "transport connection survived shutdown"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(matches!(
        events.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
}

#[test]
fn a_full_queue_cannot_keep_old_callbacks_alive_after_clear() {
    let bus = Bus::new();
    let service = Service::with_mode(&bus, false, true);
    let (tx, events) = mpsc::sync_channel(256);
    let mut transport = Transport::at(tx, Some(bus.address.clone()));
    transport.show(alert(1));
    service.shown();
    for message in 2..=40 {
        transport.show(alert(message));
    }
    let start = Instant::now();
    transport.clear_all();
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(count(&mut transport), 0);
    assert!(events.try_iter().any(|event|matches!(event,Feedback::Failure(error)if error.contains("old callbacks were discarded"))));
}
