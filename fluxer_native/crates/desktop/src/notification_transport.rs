// SPDX-License-Identifier: AGPL-3.0-or-later
//! Owned, bounded freedesktop transport. IDs belong to a unique bus owner.
use super::{Alert, Feedback, Target};
use fluxer_core::Snowflake;
use futures::{
    FutureExt, StreamExt,
    channel::{mpsc, oneshot},
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
        mpsc as sync,
    },
    thread,
    time::Duration,
};
const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const CAPACITY: usize = 256;
enum Command {
    Show(Alert),
    Clear {
        account: Snowflake,
        channel: Option<Snowflake>,
    },
    ClearAll,
    #[cfg(all(test, target_os = "linux"))]
    Inspect(sync::Sender<usize>),
}
pub(super) struct Transport {
    tx: mpsc::Sender<Command>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<thread::JoinHandle<()>>,
    feedback: sync::SyncSender<Feedback>,
    address: Option<String>,
    streaming: Arc<AtomicU8>,
}
impl Transport {
    pub fn new(feedback: sync::SyncSender<Feedback>, streaming: Arc<AtomicU8>) -> Self {
        Self::start(feedback, None, streaming)
    }
    #[cfg(all(test, target_os = "linux"))]
    fn at(feedback: sync::SyncSender<Feedback>, address: Option<String>) -> Self {
        Self::start(feedback, address, Arc::default())
    }
    fn start(
        feedback: sync::SyncSender<Feedback>,
        address: Option<String>,
        streaming: Arc<AtomicU8>,
    ) -> Self {
        let (tx, rx) = mpsc::channel(32);
        let (stop, stopped) = oneshot::channel();
        let events = feedback.clone();
        let worker_address = address.clone();
        let worker_streaming = streaming.clone();
        let task = thread::spawn(move || {
            futures::executor::block_on(async move {
                let work = run(rx, events, worker_address, worker_streaming).fuse();
                let stopped = stopped.fuse();
                futures::pin_mut!(work, stopped);
                futures::select_biased! { _ = stopped => {}, _ = work => {} }
            })
        });
        Self {
            tx,
            stop: Some(stop),
            task: Some(task),
            feedback,
            address,
            streaming,
        }
    }
    fn send(&mut self, command: Command) {
        if let Err(error) = self.tx.try_send(command) {
            if matches!(
                error.into_inner(),
                Command::Clear { .. } | Command::ClearAll
            ) {
                // A flooded/stalled daemon must not retain actionable old IDs.
                // Replacing the worker cancels in-flight calls and drops its map.
                let replacement = Self::start(
                    self.feedback.clone(),
                    self.address.clone(),
                    self.streaming.clone(),
                );
                *self = replacement;
                failure(&self.feedback, "Desktop notification dismissal queue is unavailable; old callbacks were discarded, but the daemon may still display old notifications".into());
            } else {
                failure(&self.feedback,"Desktop notification queue is full or unavailable; the notification was not delivered".into());
            }
        }
    }
    pub fn show(&mut self, alert: Alert) {
        self.send(Command::Show(alert));
    }
    pub fn clear(&mut self, account: Snowflake, channel: Option<Snowflake>) {
        self.send(Command::Clear { account, channel });
    }
    pub fn clear_all(&mut self) {
        self.send(Command::ClearAll);
    }
}
impl Drop for Transport {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}
fn failure(feedback: &sync::SyncSender<Feedback>, error: String) {
    let _ = feedback.try_send(Feedback::Failure(error));
}
struct Session {
    connection: zbus::Connection,
    signals: zbus::MessageStream,
    owners: zbus::MessageStream,
    owner: Option<String>,
    targets: BTreeMap<u32, Target>,
    order: VecDeque<u32>,
}
impl Session {
    async fn connect(address: Option<&str>) -> Result<Self, String> {
        let builder = match address {
            Some(address) => zbus::connection::Builder::address(address),
            None => zbus::connection::Builder::session(),
        }
        .map_err(|e| e.to_string())?;
        let connection = builder
            .method_timeout(Duration::from_secs(2))
            .build()
            .await
            .map_err(|e| e.to_string())?;
        // Await both subscriptions before the first Notify. Listen only to
        // notification signals and ownership changes for this one bus name.
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface(NAME)
            .unwrap()
            .path(PATH)
            .unwrap()
            .build();
        let signals = zbus::MessageStream::for_match_rule(rule, &connection, Some(256))
            .await
            .map_err(|e| e.to_string())?;
        let owner_rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender("org.freedesktop.DBus")
            .unwrap()
            .interface("org.freedesktop.DBus")
            .unwrap()
            .member("NameOwnerChanged")
            .unwrap()
            .arg(0, NAME)
            .unwrap()
            .build();
        let owners = zbus::MessageStream::for_match_rule(owner_rule, &connection, Some(8))
            .await
            .map_err(|e| e.to_string())?;
        let mut session = Self {
            connection,
            signals,
            owners,
            owner: None,
            targets: BTreeMap::new(),
            order: VecDeque::new(),
        };
        session.refresh_owner().await?;
        Ok(session)
    }
    fn forget(&mut self, id: u32) -> Option<Target> {
        self.order.retain(|current| *current != id);
        self.targets.remove(&id)
    }
    fn owner_changed(&mut self, owner: Option<String>) {
        if self.owner != owner {
            self.targets.clear();
            self.order.clear();
            self.owner = owner;
        }
    }
    async fn current_owner(&self) -> Result<Option<String>, String> {
        let proxy = zbus::fdo::DBusProxy::new(&self.connection)
            .await
            .map_err(|e| e.to_string())?;
        match proxy.get_name_owner(NAME.try_into().unwrap()).await {
            Ok(name) => Ok(Some(name.to_string())),
            Err(zbus::fdo::Error::NameHasNoOwner(_)) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }
    async fn refresh_owner(&mut self) -> Result<(), String> {
        let owner = self.current_owner().await?;
        self.owner_changed(owner);
        Ok(())
    }
    async fn close(&self, owner: &str, id: u32) -> Result<(), String> {
        let proxy = zbus::Proxy::new(&self.connection, owner, PATH, NAME)
            .await
            .map_err(|e| e.to_string())?;
        proxy
            .call::<_, _, ()>("CloseNotification", &(id,))
            .await
            .map_err(|e| e.to_string())
    }
    async fn clear(
        &mut self,
        account: Option<Snowflake>,
        channel: Option<Snowflake>,
        feedback: &sync::SyncSender<Feedback>,
    ) {
        // Forget before contacting a daemon: a disconnected service must never
        // leave a live callback target behind.
        let ids: Vec<_> = self
            .targets
            .iter()
            .filter(|(_, target)| {
                account.is_none_or(|a| a == target.account)
                    && channel.is_none_or(|c| c == target.channel)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in &ids {
            self.forget(*id);
        }
        if let Some(owner) = self.owner.clone() {
            // Closing all IDs concurrently bounds one clear to a method timeout.
            let results =
                futures::future::join_all(ids.into_iter().map(|id| self.close(&owner, id))).await;
            if let Some(error) = results.into_iter().find_map(Result::err) {
                failure(
                    feedback,
                    format!("Could not dismiss desktop notification: {error}"),
                );
            }
        }
    }
    async fn show(
        &mut self,
        alert: Alert,
        feedback: &sync::SyncSender<Feedback>,
        streaming: &AtomicU8,
    ) -> Result<(), String> {
        self.refresh_owner().await?;
        if self.owner.is_none() {
            let bus = zbus::fdo::DBusProxy::new(&self.connection)
                .await
                .map_err(|e| e.to_string())?;
            bus.start_service_by_name(NAME.try_into().unwrap(), 0)
                .await
                .map_err(|e| e.to_string())?;
            self.refresh_owner().await?;
        }
        let owner = self
            .owner
            .clone()
            .ok_or("No freedesktop notification service is running")?;
        let proxy = zbus::Proxy::new(&self.connection, owner.as_str(), PATH, NAME)
            .await
            .map_err(|e| e.to_string())?;
        let hints = std::collections::HashMap::from([
            ("suppress-sound", zbus::zvariant::Value::from(true)),
            (
                "desktop-entry",
                zbus::zvariant::Value::from("app.fluxer.Native"),
            ),
        ]);
        if !alert.allowed(streaming.load(Ordering::Acquire)) {
            return Ok(());
        }
        let id: u32 = proxy
            .call(
                "Notify",
                &(
                    "Fluxer",
                    0u32,
                    "fluxer",
                    &alert.title,
                    &alert.body,
                    vec!["default", "Open message"],
                    hints,
                    -1i32,
                ),
            )
            .await
            .map_err(|e| e.to_string())?;
        if !alert.allowed(streaming.load(Ordering::Acquire)) {
            let _ = self.close(&owner, id).await;
            return Ok(());
        }
        self.refresh_owner().await?;
        if self.owner.as_deref() != Some(owner.as_str()) {
            let _ = self.close(&owner, id).await;
            return Err(
                "Notification daemon changed while delivering; stale callback discarded".into(),
            );
        }
        // NotificationTracker.ts retains at most five notifications per channel.
        let same_channel: Vec<_> = self
            .order
            .iter()
            .copied()
            .filter(|id| {
                self.targets.get(id).is_some_and(|target| {
                    target.account == alert.target.account
                        && target.epoch == alert.target.epoch
                        && target.channel == alert.target.channel
                })
            })
            .collect();
        if same_channel.len() >= 5
            && let Some(oldest) = same_channel.first().copied()
        {
            self.forget(oldest);
            if let Err(error) = self.close(&owner, oldest).await {
                failure(
                    feedback,
                    format!("Could not dismiss old desktop notification: {error}"),
                );
            }
        }
        if self.targets.len() >= CAPACITY
            && let Some(oldest) = self.order.front().copied()
        {
            self.forget(oldest);
            if let Err(error) = self.close(&owner, oldest).await {
                failure(
                    feedback,
                    format!("Could not dismiss old desktop notification: {error}"),
                );
            }
        }
        self.forget(id);
        self.order.push_back(id);
        self.targets.insert(id, alert.target);
        Ok(())
    }
    async fn signal(
        &mut self,
        message: zbus::Message,
        feedback: &sync::SyncSender<Feedback>,
    ) -> Result<(), String> {
        let header = message.header();
        let interface = header.interface().map(|v| v.as_str());
        let member = header.member().map(|v| v.as_str());
        if interface == Some("org.freedesktop.DBus")
            && member == Some("NameOwnerChanged")
            && header
                .sender()
                .is_some_and(|s| s.as_str() == "org.freedesktop.DBus")
        {
            if let Ok((name, _, _)) = message.body().deserialize::<(String, String, String)>()
                && name == NAME
            {
                // A queued owner change may predate the last Notify. Query the
                // current owner instead of replaying an obsolete generation.
                self.refresh_owner().await?;
            }
            return Ok(());
        }
        if interface != Some(NAME) || header.path().is_none_or(|p| p.as_str() != PATH) {
            return Ok(());
        }
        let sender = header.sender().map(|s| s.as_str());
        if sender != self.owner.as_deref() {
            return Ok(());
        }
        // Also fence a queued action if its owner has since lost the name.
        self.refresh_owner().await?;
        if sender != self.owner.as_deref() {
            return Ok(());
        }
        match member {
            Some("ActionInvoked") => {
                if let Ok((id, action)) = message.body().deserialize::<(u32, String)>()
                    && action == "default"
                    && let Some(target) = self.forget(id)
                {
                    let _ = feedback.try_send(Feedback::Click(target));
                }
            }
            Some("NotificationClosed") => {
                if let Ok((id, _)) = message.body().deserialize::<(u32, u32)>() {
                    self.forget(id);
                }
            }
            _ => {}
        }
        Ok(())
    }
}
async fn run(
    mut commands: mpsc::Receiver<Command>,
    feedback: sync::SyncSender<Feedback>,
    address: Option<String>,
    streaming: Arc<AtomicU8>,
) {
    let mut session: Option<Session> = None;
    loop {
        enum Input {
            Command(Option<Command>),
            Signal(Option<zbus::Result<zbus::Message>>),
        }
        let input = if let Some(session) = &mut session {
            let command = commands.next().fuse();
            let signal = session.signals.next().fuse();
            let owner = session.owners.next().fuse();
            futures::pin_mut!(command, signal, owner);
            futures::select! {command=command=>Input::Command(command),signal=signal=>Input::Signal(signal),owner=owner=>Input::Signal(owner)}
        } else {
            Input::Command(commands.next().await)
        };
        match input {
            Input::Command(None) => break,
            Input::Command(Some(Command::Show(alert))) => {
                if !alert.allowed(streaming.load(Ordering::Acquire)) {
                    continue;
                }
                if session.is_none() {
                    match Session::connect(address.as_deref()).await {
                        Ok(connected) => session = Some(connected),
                        Err(error) => {
                            failure(
                                &feedback,
                                format!("Desktop notifications unavailable: {error}"),
                            );
                            continue;
                        }
                    }
                }
                if let Some(session) = &mut session
                    && let Err(error) = session.show(alert, &feedback, &streaming).await
                {
                    failure(
                        &feedback,
                        format!("Desktop notification delivery failed: {error}"),
                    );
                }
            }
            Input::Command(Some(Command::Clear { account, channel })) => {
                if let Some(session) = &mut session {
                    session.clear(Some(account), channel, &feedback).await;
                }
            }
            Input::Command(Some(Command::ClearAll)) => {
                if let Some(session) = &mut session {
                    session.clear(None, None, &feedback).await;
                }
            }
            #[cfg(all(test, target_os = "linux"))]
            Input::Command(Some(Command::Inspect(reply))) => {
                let _ = reply.send(session.as_ref().map_or(0, |s| s.targets.len()));
            }
            Input::Signal(Some(Ok(message))) => {
                if let Some(session) = &mut session
                    && let Err(error) = session.signal(message, &feedback).await
                {
                    failure(
                        &feedback,
                        format!("Desktop notification listener failed: {error}"),
                    );
                }
            }
            Input::Signal(Some(Err(error))) => {
                session = None;
                failure(
                    &feedback,
                    format!("Desktop notification connection lost: {error}"),
                );
            }
            Input::Signal(None) => {
                session = None;
                failure(&feedback, "Desktop notification connection closed".into());
            }
        }
    }
}
#[cfg(all(test, target_os = "linux"))]
#[path = "notification_transport_tests.rs"]
mod tests;
