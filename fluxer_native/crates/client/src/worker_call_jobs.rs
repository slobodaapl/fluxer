use super::*;
use tokio::sync::{Mutex as AsyncMutex, oneshot};

/// Serializes requests per account and channel, including cleanup after a
/// cancelled in-flight ring. Tasks retain their original authenticated client.
pub(super) struct CallJobs {
    locks: BTreeMap<(Snowflake, Snowflake), Arc<AsyncMutex<()>>>,
    pub(super) tx: async_mpsc::UnboundedSender<(u64, String)>,
    pub(super) rx: async_mpsc::UnboundedReceiver<(u64, String)>,
    pub(super) epoch: u64,
    pub(super) stop: Option<oneshot::Sender<()>>,
}
impl CallJobs {
    pub(super) fn new() -> Self {
        let (tx, rx) = async_mpsc::unbounded_channel();
        Self {
            locks: BTreeMap::new(),
            tx,
            rx,
            epoch: 0,
            stop: None,
        }
    }
    fn lock(&mut self, account: Snowflake, channel: Snowflake) -> Arc<AsyncMutex<()>> {
        self.locks.entry((account, channel)).or_default().clone()
    }
    pub(super) fn ring(
        &mut self,
        client: Client,
        account: Snowflake,
        channel: Snowflake,
        recipients: Vec<String>,
    ) {
        let lock = self.lock(account, channel);
        let (stop, mut stopped) = oneshot::channel();
        self.stop = Some(stop);
        let tx = self.tx.clone();
        let epoch = self.epoch;
        tokio::spawn(async move {
            let _guard = tokio::select! {
                biased;
                _ = &mut stopped => return,
                guard = lock.lock() => guard,
            };
            // Once sent, always follow the bounded ring attempt with cleanup,
            // even if cancellation arrives before its response.
            let result = tokio::time::timeout(
                Duration::from_secs(10),
                client.call_ringing(&channel.to_string(), &recipients, false),
            )
            .await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let _ = tx.send((epoch, format!("Could not ring call: {e}")));
                }
                Err(_) => {
                    let _ = tx.send((epoch, "Call ring request timed out".into()));
                }
            }
            let _ = stopped.await;
            match tokio::time::timeout(
                Duration::from_secs(10),
                client.call_ringing(&channel.to_string(), &recipients, true),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let _ = tx.send((epoch, format!("Could not stop ringing: {e}")));
                }
                Err(_) => {
                    let _ = tx.send((epoch, "Stop ringing request timed out".into()));
                }
            }
        });
    }
    pub(super) fn cancel(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
    pub(super) fn decline(&mut self, client: Client, account: Snowflake, channel: Snowflake) {
        let lock = self.lock(account, channel);
        let tx = self.tx.clone();
        let epoch = self.epoch;
        tokio::spawn(async move {
            let _guard = lock.lock().await;
            match tokio::time::timeout(
                Duration::from_secs(10),
                client.reject_call(&channel.to_string()),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let _ = tx.send((epoch, format!("Could not decline call: {e}")));
                }
                Err(_) => {
                    let _ = tx.send((epoch, "Decline call request timed out".into()));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[tokio::test]
    async fn cancellation_waits_for_ring_before_cleanup_without_blocking_caller() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (observed_tx, mut observed_rx) = async_mpsc::unbounded_channel();
        let (release_tx, release_rx) = oneshot::channel();
        tokio::spawn(async move {
            let mut release = Some(release_rx);
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = vec![0; 4096];
                let n = socket.read(&mut bytes).await.unwrap();
                let request = String::from_utf8_lossy(&bytes[..n]).into_owned();
                observed_tx.send(request).unwrap();
                if let Some(release) = release.take() {
                    let _ = release.await;
                }
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnull",
                    )
                    .await
                    .unwrap();
            }
        });
        let instance =
            Instance::from_endpoints(&format!("http://{address}/api"), "ws://127.0.0.1/gateway")
                .unwrap();
        let client = Client::new(instance).unwrap();
        let mut jobs = CallJobs::new();
        jobs.ring(client, 1.into(), 20.into(), vec!["2".into()]);
        let first = tokio::time::timeout(Duration::from_secs(2), observed_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(first.starts_with("POST /api/v1/channels/20/call/ring "));
        jobs.cancel();
        assert!(observed_rx.try_recv().is_err());
        release_tx.send(()).unwrap();
        let second = tokio::time::timeout(Duration::from_secs(2), observed_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(second.starts_with("POST /api/v1/channels/20/call/stop-ringing "));
    }
}
