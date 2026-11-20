// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::{CommerceData, CommerceView};
use fluxer_protocol::premium_features_visible;

impl Worker {
    pub(super) fn cancel_commerce(&mut self) {
        if let Some(task) = self.commerce_task.take() {
            task.abort();
        }
    }

    pub(super) fn fetch_commerce(&mut self, request_id: u64, view: CommerceView) {
        self.cancel_commerce();
        let client = self.client().and_then(|client| {
            if self.demo
                || !self
                    .state
                    .current_user
                    .as_ref()
                    .is_some_and(|user| !user.bot)
            {
                return Err("Sign in to a user account to view billing settings".into());
            }
            if !premium_features_visible(&client.instance().features) {
                return Err("Plutonium is unavailable on this instance".into());
            }
            Ok(client)
        });
        let client = match client {
            Ok(client) => client,
            Err(message) => {
                self.emit(Event::Commerce {
                    request_id,
                    result: Err(message),
                });
                return;
            }
        };
        let events = self.events.clone();
        self.commerce_task = Some(tokio::spawn(async move {
            let request = async {
                match view {
                    CommerceView::Plutonium => {
                        client.premium_status().await.map(CommerceData::Plutonium)
                    }
                    CommerceView::Gifts => client.gift_inventory().await.map(CommerceData::Gifts),
                }
            };
            let result = tokio::time::timeout(Duration::from_secs(15), request)
                .await
                .map_err(|_| "Loading billing settings timed out".to_string())
                .and_then(|result| result.map_err(error));
            let _ = events.send(Event::Commerce { request_id, result });
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commerce_requires_a_user_account_and_premium_tiering() {
        let (events, incoming) = mpsc::channel();
        let mut worker = Worker::new(events, Arc::new(Mutex::new(BTreeMap::new())));
        let mut instance =
            Instance::from_endpoints("http://127.0.0.1:1/api", "ws://127.0.0.1:1/gateway").unwrap();
        let token = SessionToken::new("mock-session").unwrap();
        for (request_id, user, features) in [
            (1, None, json!({"self_hosted": false})),
            (
                2,
                Some(User {
                    bot: true,
                    ..User::default()
                }),
                json!({"self_hosted": false}),
            ),
            (
                3,
                Some(User::default()),
                json!({"self_hosted": true, "stripe_enabled": true}),
            ),
        ] {
            worker.state.current_user = user;
            instance.features = features;
            worker.client = Some(
                Client::new(instance.clone())
                    .unwrap()
                    .with_token(token.clone()),
            );
            worker.fetch_commerce(request_id, CommerceView::Gifts);
            assert!(matches!(
                incoming.try_recv().unwrap(),
                Event::Commerce { request_id: received, result: Err(_) } if received == request_id
            ));
            assert!(worker.commerce_task.is_none());
        }
    }
}
