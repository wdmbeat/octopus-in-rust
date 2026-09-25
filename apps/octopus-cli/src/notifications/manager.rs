use std::collections::HashSet;

use crate::config::NotificationConfig;
use crate::notifications::store::NotificationStore;
use crate::notifications::{
    NotificationDelivery, NotificationDeliveryStatus, NotificationEvent, NotificationSinkState,
    NotificationView,
};

#[derive(Debug, Clone)]
pub struct NotificationManager {
    _config: NotificationConfig,
    store: NotificationStore,
}

impl NotificationManager {
    pub fn new(root: std::path::PathBuf, config: NotificationConfig) -> Self {
        Self {
            _config: config,
            store: NotificationStore::new(root),
        }
    }

    pub fn new_id(&self) -> String {
        format!("n{}", uuid::Uuid::new_v4().simple())
    }

    fn initial_delivery(&self, event: &NotificationEvent) -> NotificationDelivery {
        let mut sinks = std::collections::HashMap::new();
        for sink in &event.targets {
            sinks.insert(sink.clone(), NotificationSinkState::pending());
        }
        NotificationDelivery { sinks }
    }

    pub fn find_by_dedupe_key(&self, dedupe_key: &str) -> Option<NotificationView> {
        self.store
            .list_views()
            .into_iter()
            .find(|view| view.event.dedupe_key.as_deref() == Some(dedupe_key))
    }

    pub fn publish(&self, event: NotificationEvent) -> NotificationView {
        if let Some(ref dedupe_key) = event.dedupe_key
            && let Some(existing) = self.find_by_dedupe_key(dedupe_key)
        {
            return existing;
        }
        let delivery = self.initial_delivery(&event);
        let _ = self.store.create_notification(&event, &delivery);
        NotificationView { event, delivery }
    }

    pub fn recover(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let stale_after = self._config.claim_stale_after_ms as f64 / 1000.0;

        for view in self.store.list_views() {
            let mut updated = false;
            let mut delivery = view.delivery.clone();
            for sink_state in delivery.sinks.values_mut() {
                let claimed_at = match sink_state.status {
                    NotificationDeliveryStatus::Claimed(at) => at,
                    _ => continue,
                };
                if now - claimed_at <= stale_after {
                    continue;
                }
                *sink_state = NotificationSinkState::pending();
                updated = true;
            }
            if updated {
                let _ = self.store.write_delivery(&view.event.id, &delivery);
            }
        }
    }

    pub fn has_pending_for_sink(&self, sink: &str) -> bool {
        for view in self.store.list_views() {
            if let Some(sink_state) = view.delivery.sinks.get(sink)
                && matches!(sink_state.status, NotificationDeliveryStatus::Pending)
            {
                return true;
            }
        }
        false
    }

    pub fn claim_for_sink(&self, sink: &str, limit: usize) -> Vec<NotificationView> {
        self.recover();
        let mut claimed = Vec::new();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        for view in self.store.list_views().into_iter().rev() {
            let Some(sink_state) = view.delivery.sinks.get(sink) else {
                continue;
            };
            if matches!(sink_state.status, NotificationDeliveryStatus::Acked(..)) {
                continue;
            }
            if matches!(sink_state.status, NotificationDeliveryStatus::Claimed(..)) {
                continue;
            }
            let mut delivery = view.delivery.clone();
            if let Some(target_state) = delivery.sinks.get_mut(sink) {
                *target_state = NotificationSinkState::claimed(now);
            }
            let _ = self.store.write_delivery(&view.event.id, &delivery);
            claimed.push(NotificationView {
                event: view.event,
                delivery,
            });
            if claimed.len() >= limit {
                break;
            }
        }
        claimed
    }

    pub fn ack(&self, sink: &str, notification_id: &str) -> Option<NotificationView> {
        let view = self.store.merged_view(notification_id)?;
        let mut delivery = view.delivery.clone();
        let Some(sink_state) = delivery.sinks.get_mut(sink) else {
            return Some(view);
        };
        let acked_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        *sink_state = NotificationSinkState::acked(acked_at);
        let _ = self.store.write_delivery(notification_id, &delivery);
        Some(NotificationView {
            event: view.event,
            delivery,
        })
    }

    pub fn ack_ids(&self, sink: &str, notification_ids: &HashSet<String>) {
        for id in notification_ids {
            let _ = self.ack(sink, id);
        }
    }

    /// Deliver pending notifications for one sink.
    ///
    /// The handler is called for each claimed notification. If it succeeds,
    /// the notification is acked. If it fails, the notification stays claimed
    /// and will be recovered later.
    pub async fn deliver_pending<F, Fut>(
        &self,
        sink: &str,
        limit: usize,
        mut on_notification: F,
    ) -> Vec<NotificationView>
    where
        F: FnMut(&NotificationView) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let mut delivered = Vec::new();
        for view in self.claim_for_sink(sink, limit) {
            on_notification(&view).await;
            if let Some(acked) = self.ack(sink, &view.event.id) {
                delivered.push(acked);
            }
        }
        delivered
    }
}
