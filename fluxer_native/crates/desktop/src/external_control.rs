// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local session-bus call controls. No keyboard injection, account API or capture.
use fluxer_client::{CallAction, CallControlHandle, CallControlSnapshot, VoiceStatus};
use futures::{
    FutureExt,
    future::{BoxFuture, Either, select},
};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

pub(super) const BUS_NAME: &str = "app.fluxer.Native";
const PATH: &str = "/app/fluxer/Native/Call";
const INTERFACE: &str = "app.fluxer.Native.Call1";
const ACTION_TIMEOUT: Duration = Duration::from_secs(3);

trait Backend: Send + Sync {
    fn snapshot(&self) -> Option<CallControlSnapshot>;
    fn control(
        &self,
        expected: CallControlSnapshot,
        action: CallAction,
        deadline: Instant,
    ) -> BoxFuture<'_, Result<Option<CallControlSnapshot>, String>>;
}
impl Backend for CallControlHandle {
    fn snapshot(&self) -> Option<CallControlSnapshot> {
        CallControlHandle::snapshot(self)
    }
    fn control(
        &self,
        expected: CallControlSnapshot,
        action: CallAction,
        deadline: Instant,
    ) -> BoxFuture<'_, Result<Option<CallControlSnapshot>, String>> {
        self.control(expected.account_id, expected.generation, action, deadline)
            .boxed()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct View {
    status: String,
    generation: u64,
    muted: bool,
    deafened: bool,
    effective_muted: bool,
    effective_deafened: bool,
    sharing: bool,
}
impl View {
    fn read(backend: &dyn Backend) -> Self {
        let Some(call) = backend.snapshot() else {
            return Self {
                status: "disconnected".into(),
                ..Default::default()
            };
        };
        Self {
            status: match call.status {
                VoiceStatus::Disconnected => "disconnected",
                VoiceStatus::Connecting { .. } => "connecting",
                VoiceStatus::Connected { .. } => "connected",
                VoiceStatus::Reconnecting => "reconnecting",
            }
            .into(),
            generation: call.generation,
            muted: call.muted,
            deafened: call.deafened,
            effective_muted: call.muted
                || call.server_muted
                || call.deafened
                || call.server_deafened,
            effective_deafened: call.deafened || call.server_deafened,
            sharing: call.screen_sharing,
        }
    }
    fn in_call(&self) -> bool {
        self.status != "disconnected"
    }
    fn properties(&self) -> HashMap<&str, zbus::zvariant::Value<'_>> {
        use zbus::zvariant::Value;
        HashMap::from([
            ("InCall", Value::from(self.in_call())),
            ("Status", Value::from(self.status.as_str())),
            ("CallGeneration", Value::from(self.generation)),
            ("Muted", Value::from(self.muted)),
            ("Deafened", Value::from(self.deafened)),
            ("EffectiveMuted", Value::from(self.effective_muted)),
            ("EffectiveDeafened", Value::from(self.effective_deafened)),
            ("ScreenSharing", Value::from(self.sharing)),
        ])
    }
}

struct CallInterface {
    backend: Arc<dyn Backend>,
    pending: AtomicUsize,
}
struct Pending<'a>(&'a AtomicUsize);
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
impl CallInterface {
    fn new(backend: Arc<dyn Backend>) -> Self {
        Self {
            backend,
            pending: AtomicUsize::new(0),
        }
    }
    async fn act(&self, action: CallAction) -> zbus::fdo::Result<()> {
        if self.pending.fetch_add(1, Ordering::AcqRel) >= 16 {
            self.pending.fetch_sub(1, Ordering::AcqRel);
            return Err(zbus::fdo::Error::LimitsExceeded(
                "Too many pending call actions".into(),
            ));
        }
        let _pending = Pending(&self.pending);
        let expected = self
            .backend
            .snapshot()
            .ok_or_else(|| zbus::fdo::Error::Failed("No active call".into()))?;
        let deadline = Instant::now() + ACTION_TIMEOUT;
        match select(
            self.backend.control(expected, action, deadline),
            gpui::Timer::after(ACTION_TIMEOUT).boxed(),
        )
        .await
        {
            Either::Left((result, _)) => result.map(|_| ()).map_err(zbus::fdo::Error::Failed),
            // Dropping the reply future cancels an action still waiting in the
            // worker queue. The actor also validates the monotonic deadline.
            Either::Right(_) => Err(zbus::fdo::Error::TimedOut("Call control timed out".into())),
        }
    }
}

#[zbus::interface(name = "app.fluxer.Native.Call1")]
impl CallInterface {
    async fn toggle_mute(&self) -> zbus::fdo::Result<()> {
        self.act(CallAction::ToggleMute).await
    }
    async fn set_muted(&self, muted: bool) -> zbus::fdo::Result<()> {
        self.act(CallAction::SetMute(muted)).await
    }
    async fn toggle_deafen(&self) -> zbus::fdo::Result<()> {
        self.act(CallAction::ToggleDeaf).await
    }
    async fn set_deafened(&self, deafened: bool) -> zbus::fdo::Result<()> {
        self.act(CallAction::SetDeaf(deafened)).await
    }
    async fn disconnect(&self) -> zbus::fdo::Result<()> {
        self.act(CallAction::Disconnect).await
    }
    async fn stop_sharing(&self) -> zbus::fdo::Result<()> {
        self.act(CallAction::StopSharing).await
    }
    #[zbus(property(emits_changed_signal = "const"))]
    fn api_version(&self) -> u32 {
        1
    }
    #[zbus(property)]
    fn in_call(&self) -> bool {
        View::read(self.backend.as_ref()).in_call()
    }
    #[zbus(property)]
    fn status(&self) -> String {
        View::read(self.backend.as_ref()).status
    }
    #[zbus(property)]
    fn call_generation(&self) -> u64 {
        View::read(self.backend.as_ref()).generation
    }
    #[zbus(property)]
    fn muted(&self) -> bool {
        View::read(self.backend.as_ref()).muted
    }
    #[zbus(property)]
    fn deafened(&self) -> bool {
        View::read(self.backend.as_ref()).deafened
    }
    #[zbus(property)]
    fn effective_muted(&self) -> bool {
        View::read(self.backend.as_ref()).effective_muted
    }
    #[zbus(property)]
    fn effective_deafened(&self) -> bool {
        View::read(self.backend.as_ref()).effective_deafened
    }
    #[zbus(property)]
    fn screen_sharing(&self) -> bool {
        View::read(self.backend.as_ref()).sharing
    }
}

fn serve(
    backend: Arc<dyn Backend>,
    stop: mpsc::Receiver<()>,
    address: Option<&str>,
) -> zbus::Result<()> {
    let builder = match address {
        Some(address) => zbus::blocking::connection::Builder::address(address)?,
        None => zbus::blocking::connection::Builder::session()?,
    };
    let connection = builder
        .name(BUS_NAME)?
        .allow_name_replacements(false)
        .replace_existing_names(false)
        .serve_at(PATH, CallInterface::new(backend.clone()))?
        .build()?;
    let mut previous = View::read(backend.as_ref());
    loop {
        match stop.recv_timeout(Duration::from_millis(100)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let view = View::read(backend.as_ref());
        if view != previous {
            connection.emit_signal(
                None::<&str>,
                PATH,
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                &(INTERFACE, view.properties(), Vec::<&str>::new()),
            )?;
            previous = view;
        }
    }
    connection.close()
}

pub(super) struct ExternalControl {
    stop: mpsc::SyncSender<()>,
}
impl ExternalControl {
    pub fn start(handle: CallControlHandle) -> Self {
        let (stop, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("fluxer-call-control".into())
            .spawn(move || {
                if let Err(error) = serve(Arc::new(handle), receiver, None) {
                    eprintln!("Local call control unavailable: {error}");
                }
            })
            .expect("start local call control worker");
        Self { stop }
    }
}
impl Drop for ExternalControl {
    fn drop(&mut self) {
        let _ = self.stop.try_send(());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{BUS_NAME, Backend, CallInterface, INTERFACE, PATH, View, serve};
    use fluxer_client::{CallAction, CallControlSnapshot, VoiceStatus};
    use futures::{FutureExt, future::BoxFuture};
    use std::{
        io::{BufRead, BufReader},
        process::{Child, Command, Stdio},
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        time::{Duration, Instant},
    };

    fn call() -> CallControlSnapshot {
        CallControlSnapshot {
            account_id: 1.into(),
            generation: 7,
            channel_id: 10.into(),
            connection_id: None,
            status: VoiceStatus::Connected {
                channel_id: 10.into(),
            },
            muted: false,
            deafened: false,
            server_muted: false,
            server_deafened: false,
            screen_sharing: true,
        }
    }
    struct Fixture {
        state: Mutex<Option<CallControlSnapshot>>,
        calls: Mutex<Vec<CallAction>>,
    }
    impl Backend for Fixture {
        fn snapshot(&self) -> Option<CallControlSnapshot> {
            self.state.lock().unwrap().clone()
        }
        fn control(
            &self,
            expected: CallControlSnapshot,
            action: CallAction,
            deadline: Instant,
        ) -> BoxFuture<'_, Result<Option<CallControlSnapshot>, String>> {
            async move {
                let mut state = self.state.lock().unwrap();
                let current = state.as_mut().ok_or("No active call")?;
                if current.account_id != expected.account_id
                    || current.generation != expected.generation
                    || Instant::now() >= deadline
                {
                    return Err("Stale or expired call".into());
                }
                self.calls.lock().unwrap().push(action);
                match action {
                    CallAction::ToggleMute => current.muted = !current.muted,
                    CallAction::SetMute(value) => current.muted = value,
                    CallAction::ToggleDeaf => current.deafened = !current.deafened,
                    CallAction::SetDeaf(value) => current.deafened = value,
                    CallAction::StopSharing => current.screen_sharing = false,
                    CallAction::Disconnect => *state = None,
                }
                Ok(state.clone())
            }
            .boxed()
        }
    }
    struct PrivateBus {
        process: Child,
        address: String,
    }
    impl PrivateBus {
        fn start() -> Self {
            let mut process = Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("private D-Bus test requires dbus-daemon");
            let mut address = String::new();
            BufReader::new(process.stdout.take().unwrap())
                .read_line(&mut address)
                .unwrap();
            assert!(!address.is_empty());
            Self {
                process,
                address: address.trim().into(),
            }
        }
    }
    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.process.kill();
            let _ = self.process.wait();
        }
    }
    struct Server {
        stop: mpsc::Sender<()>,
        task: Option<std::thread::JoinHandle<zbus::Result<()>>>,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.stop.send(());
            if let Some(task) = self.task.take() {
                task.join()
                    .expect("server thread panicked")
                    .expect("private call service failed");
            }
        }
    }

    #[test]
    fn private_bus_controls_properties_signals_and_ownership() {
        let bus = PrivateBus::start();
        let backend = Arc::new(Fixture {
            state: Mutex::new(Some(call())),
            calls: Mutex::new(Vec::new()),
        });
        let (stop, receiver) = mpsc::channel();
        let address = bus.address.clone();
        let service_backend = backend.clone();
        let server = Server {
            stop,
            task: Some(std::thread::spawn(move || {
                serve(service_backend, receiver, Some(&address))
            })),
        };
        let connection = zbus::blocking::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .method_timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let bus_proxy = zbus::blocking::Proxy::new(
            &connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .unwrap();
        for _ in 0..100 {
            if bus_proxy
                .call::<_, _, bool>("NameHasOwner", &(BUS_NAME,))
                .unwrap()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let proxy: zbus::blocking::Proxy<'_> = zbus::blocking::proxy::Builder::new(&connection)
            .destination(BUS_NAME)
            .unwrap()
            .path(PATH)
            .unwrap()
            .interface(INTERFACE)
            .unwrap()
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .unwrap();
        assert_eq!(proxy.get_property::<u32>("ApiVersion").unwrap(), 1);
        assert!(proxy.get_property::<bool>("InCall").unwrap());
        assert!(!proxy.get_property::<bool>("Muted").unwrap());
        assert_eq!(proxy.get_property::<u64>("CallGeneration").unwrap(), 7);

        let property_proxy = zbus::blocking::Proxy::new(
            &connection,
            BUS_NAME,
            PATH,
            "org.freedesktop.DBus.Properties",
        )
        .unwrap();
        let signals = property_proxy.receive_signal("PropertiesChanged").unwrap();
        let (sent, received) = mpsc::channel();
        let listener = std::thread::spawn(move || {
            for signal in signals {
                let body = signal
                    .body()
                    .deserialize::<(
                        String,
                        std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
                        Vec<String>,
                    )>()
                    .unwrap();
                if body.0 == INTERFACE
                    && body
                        .1
                        .get("Muted")
                        .and_then(|value| bool::try_from(value).ok())
                        == Some(true)
                {
                    let _ = sent.send(());
                    break;
                }
            }
        });
        proxy.call::<_, _, ()>("ToggleMute", &()).unwrap();
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        listener.join().unwrap();
        assert!(proxy.get_property::<bool>("Muted").unwrap());
        proxy.call::<_, _, ()>("SetMuted", &(false,)).unwrap();
        proxy.call::<_, _, ()>("SetDeafened", &(true,)).unwrap();
        assert!(proxy.get_property::<bool>("EffectiveMuted").unwrap());
        proxy.call::<_, _, ()>("ToggleDeafen", &()).unwrap();
        proxy.call::<_, _, ()>("StopSharing", &()).unwrap();
        assert!(!proxy.get_property::<bool>("ScreenSharing").unwrap());
        let duplicate = zbus::blocking::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name(BUS_NAME)
            .unwrap()
            .build();
        assert!(
            duplicate.is_err(),
            "second app cannot take over the current call controller"
        );
        proxy.call::<_, _, ()>("Disconnect", &()).unwrap();
        assert!(!proxy.get_property::<bool>("InCall").unwrap());
        assert!(proxy.call::<_, _, ()>("ToggleMute", &()).is_err());
        assert_eq!(
            *backend.calls.lock().unwrap(),
            [
                CallAction::ToggleMute,
                CallAction::SetMute(false),
                CallAction::SetDeaf(true),
                CallAction::ToggleDeaf,
                CallAction::StopSharing,
                CallAction::Disconnect
            ]
        );
        drop(server);
        assert!(
            !bus_proxy
                .call::<_, _, bool>("NameHasOwner", &(BUS_NAME,))
                .unwrap()
        );
    }

    #[test]
    fn effective_state_never_claims_server_moderation_was_undone() {
        let mut state = call();
        state.server_muted = true;
        let backend = Fixture {
            state: Mutex::new(Some(state)),
            calls: Mutex::new(Vec::new()),
        };
        let view = View::read(&backend);
        assert!(!view.muted && view.effective_muted);
        backend
            .state
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .server_deafened = true;
        assert!(View::read(&backend).effective_deafened);
    }

    #[test]
    fn pending_controls_are_bounded_and_timeouts_drop_the_action_future() {
        struct Never {
            dropped: Arc<AtomicUsize>,
        }
        struct Dropped(Arc<AtomicUsize>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::AcqRel);
            }
        }
        impl Backend for Never {
            fn snapshot(&self) -> Option<CallControlSnapshot> {
                Some(call())
            }
            fn control(
                &self,
                _: CallControlSnapshot,
                _: CallAction,
                _: Instant,
            ) -> BoxFuture<'_, Result<Option<CallControlSnapshot>, String>> {
                async move {
                    let _guard = Dropped(self.dropped.clone());
                    futures::future::pending().await
                }
                .boxed()
            }
        }
        let dropped = Arc::new(AtomicUsize::new(0));
        let interface = CallInterface::new(Arc::new(Never {
            dropped: dropped.clone(),
        }));
        let results = futures::executor::block_on(futures::future::join_all(
            (0..17).map(|_| interface.act(CallAction::ToggleMute)),
        ));
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(zbus::fdo::Error::LimitsExceeded(_))))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(zbus::fdo::Error::TimedOut(_))))
                .count(),
            16
        );
        assert_eq!(dropped.load(Ordering::Acquire), 16);
        assert_eq!(interface.pending.load(Ordering::Acquire), 0);
    }
}
