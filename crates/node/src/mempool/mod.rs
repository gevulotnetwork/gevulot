use async_trait::async_trait;
use eyre::Result;
use std::collections::VecDeque;
use std::sync::Arc;
use uuid::Uuid;

pub trait Identifiable {
    fn id(&self) -> Uuid;
}

#[async_trait]
pub trait Storage<T: Identifiable>: Send + Sync {
    async fn get(&self, id: Uuid) -> Result<Option<T>>;
    async fn set(&self, obj: &T) -> Result<()>;
    async fn fill_deque(&self, deque: &mut VecDeque<T>) -> Result<()>;
}

#[derive(Clone)]
pub struct Mempool<T: Identifiable> {
    storage: Arc<dyn Storage<T>>,
    deque: VecDeque<T>,
}

impl<T: Identifiable> Mempool<T> {
    pub async fn new(storage: Arc<dyn Storage<T>>) -> Result<Self> {
        let mut deque = VecDeque::new();
        storage.fill_deque(&mut deque).await?;
        Ok(Self { storage, deque })
    }

    pub fn next(&mut self) -> Option<T> {
        // TODO(tuommaki): Should storage reflect the POP in state?
        self.deque.pop_front()
    }

    pub fn peek(&self) -> Option<&T> {
        self.deque.front()
    }

    pub async fn add(&mut self, obj: T) -> Result<()> {
        self.storage.set(&obj).await?;
        self.deque.push_back(obj);
        Ok(())
    }

    pub fn size(&self) -> usize {
        self.deque.len()
    }
}
