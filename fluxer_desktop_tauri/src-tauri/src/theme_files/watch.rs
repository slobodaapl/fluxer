// SPDX-License-Identifier: AGPL-3.0-or-later

//! Linked CSS files watched for edits: `createThemeLinkedFileWatchSet` of
//! ThemeLinkedFileWatch.ts and the per-webview subscriptions of ThemeLocalFiles.ts.
//!
//! Electron used `fs.watchFile` with a 250 ms interval, which polls `stat` and fires when the
//! result changes, including between existing and missing; two failed polls in a row do not
//! fire. Polling is kept rather than inotify: it sees files that do not exist yet, editors that
//! save by renaming over the file, and network file systems, as `fs.watchFile` did. Every new
//! path is read at once, so a missing file reports `missing` straight away, and each poll that
//! fires reads the file again. A read only reports if its path is still watched and no later
//! read of it has started.
//!
//! A subscription belongs to the document that asked for it. Electron dropped it when the
//! webview navigated or was destroyed; here every report is withheld once the document's token
//! is gone, and the next poll drops the subscription.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use super::ThemeLinkedFileChange;
use super::css_file::read_theme_css_file;

const POLL_INTERVAL: Duration = Duration::from_millis(250);

pub(super) type Emit = Arc<dyn Fn(ThemeLinkedFileChange) + Send + Sync>;

/// The `stat` fields libuv compares between polls.
#[cfg(unix)]
type Snapshot = (i64, i64, i64, i64, u64, u32, u32, u32, u64, u64);
#[cfg(windows)]
type Snapshot = (
    Option<std::time::SystemTime>,
    Option<std::time::SystemTime>,
    u64,
    u32,
);

#[cfg(unix)]
fn snapshot(metadata: &std::fs::Metadata) -> Snapshot {
    use std::os::unix::fs::MetadataExt;
    (
        metadata.ctime(),
        metadata.ctime_nsec(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.size(),
        metadata.mode(),
        metadata.uid(),
        metadata.gid(),
        metadata.ino(),
        metadata.dev(),
    )
}

#[cfg(windows)]
fn snapshot(metadata: &std::fs::Metadata) -> Snapshot {
    use std::os::windows::fs::MetadataExt;
    (
        metadata.modified().ok(),
        metadata.created().ok(),
        metadata.len(),
        metadata.file_attributes(),
    )
}

/// `stat` of `path`, following symlinks as `fs.watchFile` does; `None` when it fails.
fn poll_stat(path: &str) -> Option<Snapshot> {
    std::fs::metadata(path)
        .ok()
        .map(|metadata| snapshot(&metadata))
}

struct Entry {
    /// Tells a re-added path from the entry it replaced (`current !== entry`).
    id: u64,
    generation: u64,
    /// The last poll's result; unset until the first one.
    last: Option<Option<Snapshot>>,
}

/// The watched paths of one subscription.
pub(super) struct WatchSet {
    entries: Mutex<HashMap<String, Entry>>,
    next_id: AtomicU64,
    emit: Emit,
}

impl WatchSet {
    fn new(emit: Emit) -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            emit,
        })
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        self.entries.lock().expect("theme linked file watch lock")
    }

    pub fn emit(&self, change: ThemeLinkedFileChange) {
        (self.emit)(change);
    }

    /// `replace`: stops watching paths not in `paths`, starts on the new ones.
    pub fn replace(self: &Arc<Self>, paths: &[String]) {
        let mut added = Vec::new();
        {
            let mut entries = self.lock();
            entries.retain(|path, _| paths.contains(path));
            for path in paths {
                if entries.contains_key(path) {
                    continue;
                }
                let entry = Entry {
                    id: self.next_id.fetch_add(1, Ordering::Relaxed),
                    generation: 0,
                    last: None,
                };
                entries.insert(path.clone(), entry);
                added.push(path.clone());
            }
        }
        for path in added {
            self.read(path, true);
        }
    }

    /// `read`. `baseline` takes the first poll's `stat` before reading, so an edit that lands
    /// after the read is seen by the next poll.
    fn read(self: &Arc<Self>, path: String, baseline: bool) {
        let ticket = {
            let mut entries = self.lock();
            let Some(entry) = entries.get_mut(&path) else {
                return;
            };
            entry.generation += 1;
            (entry.id, entry.generation)
        };
        let set = Arc::clone(self);
        tauri::async_runtime::spawn_blocking(move || {
            if baseline {
                let polled = poll_stat(&path);
                if let Some(entry) = set.lock().get_mut(&path)
                    && entry.id == ticket.0
                    && entry.last.is_none()
                {
                    entry.last = Some(polled);
                }
            }
            let change = read_theme_css_file(&path);
            let entries = set.lock();
            if entries
                .get(&path)
                .is_some_and(|entry| (entry.id, entry.generation) == ticket)
            {
                (set.emit)(change);
            }
        });
    }

    /// One `fs.watchFile` poll of every path; the paths whose `stat` changed.
    fn poll(&self) -> Vec<String> {
        let watched: Vec<(String, u64)> = self
            .lock()
            .iter()
            .map(|(path, entry)| (path.clone(), entry.id))
            .collect();
        let mut changed = Vec::new();
        for (path, id) in watched {
            let current = poll_stat(&path);
            let mut entries = self.lock();
            let Some(entry) = entries.get_mut(&path).filter(|entry| entry.id == id) else {
                continue;
            };
            let fired = match (&entry.last, &current) {
                (None, _) | (Some(None), None) => false,
                (Some(Some(previous)), Some(current)) => previous != current,
                (Some(_), _) => true,
            };
            entry.last = Some(current);
            if fired {
                changed.push(path);
            }
        }
        changed
    }

    /// `dispose`: in-flight reads find nothing to report to.
    fn dispose(&self) {
        self.lock().clear();
    }
}

struct Subscription {
    id: u64,
    token: String,
    set: Arc<WatchSet>,
    stop: CancellationToken,
}

impl Subscription {
    fn close(self) {
        self.stop.cancel();
        self.set.dispose();
    }
}

/// `linkedFileWatchSets`, keyed by webview label instead of WebContents id.
#[derive(Default)]
pub struct ThemeLinkedFileWatches {
    subscriptions: Mutex<HashMap<String, Subscription>>,
    next_id: AtomicU64,
}

impl ThemeLinkedFileWatches {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Subscription>> {
        self.subscriptions
            .lock()
            .expect("theme linked file subscriptions lock")
    }

    /// `getLinkedFileWatchSubscription`: the watch set of the document `token` names in
    /// `label`, made on first use. `is_current` tells when that document is gone; `emit` must
    /// already refuse to report to any other.
    pub(super) fn subscription(
        self: &Arc<Self>,
        label: &str,
        token: &str,
        emit: Emit,
        is_current: impl Fn() -> bool + Send + 'static,
    ) -> Arc<WatchSet> {
        let (set, replaced) = {
            let mut subscriptions = self.lock();
            if let Some(existing) = subscriptions.get(label)
                && existing.token == token
            {
                return Arc::clone(&existing.set);
            }
            let subscription = Subscription {
                id: self.next_id.fetch_add(1, Ordering::Relaxed),
                token: token.to_owned(),
                set: WatchSet::new(emit),
                stop: CancellationToken::new(),
            };
            self.spawn_polling(label, &subscription, is_current);
            let set = Arc::clone(&subscription.set);
            (set, subscriptions.insert(label.to_owned(), subscription))
        };
        if let Some(replaced) = replaced {
            replaced.close();
        }
        set
    }

    /// `subscription.dispose()`.
    pub(super) fn dispose(&self, label: &str) {
        let removed = self.lock().remove(label);
        if let Some(subscription) = removed {
            subscription.close();
        }
    }

    fn spawn_polling(
        self: &Arc<Self>,
        label: &str,
        subscription: &Subscription,
        is_current: impl Fn() -> bool + Send + 'static,
    ) {
        let watches = Arc::clone(self);
        let label = label.to_owned();
        let id = subscription.id;
        let set = Arc::clone(&subscription.set);
        let stop = subscription.stop.clone();
        tauri::async_runtime::spawn(async move {
            let mut interval =
                tokio::time::interval_at(Instant::now() + POLL_INTERVAL, POLL_INTERVAL);
            interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    () = stop.cancelled() => return,
                    _ = interval.tick() => {}
                }
                if !is_current() {
                    let removed = {
                        let mut subscriptions = watches.lock();
                        match subscriptions.get(&label) {
                            Some(current) if current.id == id => subscriptions.remove(&label),
                            _ => None,
                        }
                    };
                    if let Some(subscription) = removed {
                        subscription.close();
                    }
                    return;
                }
                let polled = Arc::clone(&set);
                let Ok(changed) = tauri::async_runtime::spawn_blocking(move || polled.poll()).await
                else {
                    return;
                };
                for path in changed {
                    set.read(path, false);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme_files::ThemeFileError;
    use std::sync::atomic::AtomicBool;

    type Events = Arc<Mutex<Vec<ThemeLinkedFileChange>>>;

    fn recorder() -> (Events, Emit) {
        let events: Events = Arc::default();
        let sink = Arc::clone(&events);
        (
            events,
            Arc::new(move |change| sink.lock().unwrap().push(change)),
        )
    }

    /// Waits up to five seconds for the recorded events to satisfy `done`.
    fn wait_for(events: &Events, done: impl Fn(&[ThemeLinkedFileChange]) -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !done(&events.lock().unwrap()) {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out; events: {:?}",
                events.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn css(path: &str, css: &str) -> ThemeLinkedFileChange {
        ThemeLinkedFileChange {
            path: path.to_owned(),
            css: Some(css.to_owned()),
            error: None,
        }
    }

    fn path_of(directory: &tempfile::TempDir, name: &str) -> String {
        directory.path().join(name).to_string_lossy().into_owned()
    }

    #[test]
    fn reports_at_once_then_on_every_change_including_missing_transitions() {
        let directory = tempfile::tempdir().unwrap();
        let path = path_of(&directory, "theme.css");
        std::fs::write(&path, "a{}").unwrap();
        let (events, emit) = recorder();
        let watches = Arc::new(ThemeLinkedFileWatches::default());
        let set = watches.subscription("main", "token", emit, || true);
        set.replace(std::slice::from_ref(&path));
        wait_for(&events, |events| events == [css(&path, "a{}")]);

        std::fs::write(&path, "a{color:red}").unwrap();
        wait_for(&events, |events| {
            events.last() == Some(&css(&path, "a{color:red}"))
        });

        std::fs::remove_file(&path).unwrap();
        wait_for(&events, |events| {
            events.last()
                == Some(&ThemeLinkedFileChange::failed(
                    &path,
                    ThemeFileError::Missing,
                ))
        });
        // Missing stays missing: no further reports.
        let count = events.lock().unwrap().len();
        std::thread::sleep(POLL_INTERVAL * 3);
        assert_eq!(events.lock().unwrap().len(), count);

        std::fs::write(&path, "b{}").unwrap();
        wait_for(&events, |events| events.last() == Some(&css(&path, "b{}")));
        watches.dispose("main");
    }

    #[test]
    fn a_path_that_does_not_exist_yet_reports_missing_and_then_its_contents() {
        let directory = tempfile::tempdir().unwrap();
        let path = path_of(&directory, "later.css");
        let (events, emit) = recorder();
        let watches = Arc::new(ThemeLinkedFileWatches::default());
        watches
            .subscription("main", "token", emit, || true)
            .replace(std::slice::from_ref(&path));
        wait_for(&events, |events| {
            events
                == [ThemeLinkedFileChange::failed(
                    &path,
                    ThemeFileError::Missing,
                )]
        });
        std::fs::write(&path, "late{}").unwrap();
        wait_for(&events, |events| {
            events.last() == Some(&css(&path, "late{}"))
        });
        watches.dispose("main");
    }

    #[test]
    fn replaced_and_disposed_paths_stop_reporting() {
        let directory = tempfile::tempdir().unwrap();
        let first = path_of(&directory, "first.css");
        let second = path_of(&directory, "second.css");
        std::fs::write(&first, "1{}").unwrap();
        std::fs::write(&second, "2{}").unwrap();
        let (events, emit) = recorder();
        let watches = Arc::new(ThemeLinkedFileWatches::default());
        let set = watches.subscription("main", "token", emit, || true);
        set.replace(&[first.clone(), second.clone()]);
        wait_for(&events, |events| events.len() == 2);
        set.replace(std::slice::from_ref(&second));
        std::fs::write(&first, "1{changed}").unwrap();
        std::fs::write(&second, "2{changed}").unwrap();
        wait_for(&events, |events| {
            events.last() == Some(&css(&second, "2{changed}"))
        });
        assert!(
            events
                .lock()
                .unwrap()
                .iter()
                .all(|event| event != &css(&first, "1{changed}"))
        );

        watches.dispose("main");
        let count = events.lock().unwrap().len();
        std::fs::write(&second, "2{after dispose}").unwrap();
        std::thread::sleep(POLL_INTERVAL * 3);
        assert_eq!(events.lock().unwrap().len(), count);
    }

    #[test]
    fn a_new_document_replaces_the_subscription_and_a_gone_one_is_dropped() {
        let directory = tempfile::tempdir().unwrap();
        let path = path_of(&directory, "theme.css");
        std::fs::write(&path, "a{}").unwrap();
        let (old_events, old_emit) = recorder();
        let watches = Arc::new(ThemeLinkedFileWatches::default());
        let old = watches.subscription("main", "old", old_emit, || true);
        old.replace(std::slice::from_ref(&path));
        wait_for(&old_events, |events| events.len() == 1);
        // The same document gets its existing set back.
        assert!(Arc::ptr_eq(
            &old,
            &watches.subscription("main", "old", Arc::new(|_| {}), || true)
        ));

        let current = Arc::new(AtomicBool::new(true));
        let alive = Arc::clone(&current);
        let (new_events, new_emit) = recorder();
        let new = watches.subscription("main", "new", new_emit, move || {
            alive.load(Ordering::SeqCst)
        });
        assert!(!Arc::ptr_eq(&old, &new));
        new.replace(std::slice::from_ref(&path));
        wait_for(&new_events, |events| events.len() == 1);
        std::fs::write(&path, "a{changed}").unwrap();
        wait_for(&new_events, |events| events.len() == 2);
        assert_eq!(
            old_events.lock().unwrap().len(),
            1,
            "the replaced set kept reporting"
        );

        current.store(false, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while watches.lock().contains_key("main") {
            assert!(
                std::time::Instant::now() < deadline,
                "the gone document's subscription stayed"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
