use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    task::Waker,
};

struct BagInner {
    wakers: Vec<Waker>,
    ids: Vec<u64>,
}

impl BagInner {
    fn new() -> Self {
        Self {
            wakers: Vec::new(),
            ids: Vec::new(),
        }
    }
}

#[derive(Clone)]
struct Bag {
    bag_inner: Arc<Mutex<BagInner>>,
}

impl std::future::Future for Bag {
    type Output = u64;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut inner = self.bag_inner.lock().unwrap();
        if let Some(id) = inner.ids.pop() {
            std::task::Poll::Ready(id)
        } else {
            inner.wakers.push(cx.waker().clone());
            std::task::Poll::Pending
        }
    }
}

pub(super) struct AsyncIdAllocator {
    max: u64,
    count: AtomicU64,
    bag: Bag,
}

impl AsyncIdAllocator {
    pub fn new(num: usize) -> Self {
        if num == 0 {
            panic!("cannot set num IDs as 0");
        }
        Self {
            max: num as u64 - 1,
            count: AtomicU64::new(0),
            bag: Bag {
                bag_inner: Arc::new(Mutex::new(BagInner::new())),
            },
        }
    }
    pub fn try_next(&self) -> Option<u64> {
        if self.count.load(Ordering::SeqCst) <= self.max {
            let id = self.count.fetch_add(1, Ordering::SeqCst);
            if id <= self.max {
                return Some(id);
            }
        }

        let mut inner = self.bag.bag_inner.lock().unwrap();
        inner.ids.pop()
    }

    pub async fn next(&self) -> u64 {
        if let Some(id) = self.try_next() {
            return id;
        }
        self.bag.clone().await
    }

    pub fn release_id(&self, id: u64) {
        let mut inner = self.bag.bag_inner.lock().unwrap();
        inner.ids.push(id);
        while let Some(w) = inner.wakers.pop() {
            w.wake();
        }
    }
}
