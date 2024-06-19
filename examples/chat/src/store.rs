use futures_util::stream::Stream;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::RwLock;

use crate::types;
#[derive(Default, Clone)]
pub struct Store {
    inner: Arc<RwLock<InnerStore>>,
}

#[derive(Default)]
struct InnerStore {
    messages: HashMap<types::ApplicationId, Vec<types::ChatMessage>>,
}

impl Store {
    pub async fn add_message(
        &mut self,
        application_id: types::ApplicationId,
        message: types::ChatMessage,
    ) {
        self.inner
            .write()
            .await
            .messages
            .entry(application_id)
            .or_default()
            .push(message)
    }

    pub fn batch_stream(
        &self,
        application_id: types::ApplicationId,
        batch_size: usize,
    ) -> MessageStream {
        MessageStream::new(&self, application_id, batch_size)
    }
}

pub struct MessageStream<'a> {
    store: &'a Store,
    application_id: types::ApplicationId,
    batch_size: usize,
    index: usize,
}

impl<'a> MessageStream<'a> {
    pub fn new(store: &'a Store, application_id: types::ApplicationId, batch_size: usize) -> Self {
        Self {
            store,
            application_id,
            batch_size,
            index: 0,
        }
    }

    pub async fn next(&mut self) -> Option<Vec<types::ChatMessage>> {
        let lock = self.store.inner.read().await;
        let messages = lock.messages.get(&self.application_id)?;
        if self.index == messages.len() {
            return None;
        }

        let end_index = (self.index + self.batch_size).min(messages.len());
        let batch = messages[self.index..end_index].to_vec();
        self.index = end_index;
        Some(batch)
    }
}

impl<'a> Stream for MessageStream<'a> {
    type Item = Vec<types::ChatMessage>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(self.next()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_message_iterator() {
        let mut store = Store::default();
        let application_id = types::ApplicationId("app1".into());

        for _ in 0..7 {
            store
                .add_message(
                    application_id.clone(),
                    types::ChatMessage::new(None, vec![1, 2, 3]),
                )
                .await;
        }
        let mut message_iterator = store.batch_stream(application_id, 3);

        for _ in 0..2 {
            let batch = message_iterator.next().await.unwrap();
            assert_eq!(batch.len(), 3);
        }
        let batch = message_iterator.next().await.unwrap();
        assert_eq!(batch.len(), 1);

        let batch = message_iterator.next().await;
        assert_eq!(batch.is_none(), true);
    }
}
