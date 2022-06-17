use std::future::Future;

use twizzler_abi::pager::{KernelCompletion, KernelRequest, PagerCompletion, PagerRequest};
use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};
use twizzler_queue::{CallbackQueueReceiver, Queue, QueueError, QueueSender};

pub struct PagerContext {
    pager_queue: QueueSender<PagerRequest, PagerCompletion>,
    kernel_queue: CallbackQueueReceiver<KernelRequest, KernelCompletion>,
}

impl PagerContext {
    pub fn new(kqid: ObjID, pqid: ObjID) -> Result<Self, anyhow::Error> {
        let kqo = Object::init_id(
            kqid,
            Protections::READ | Protections::WRITE,
            ObjectInitFlags::empty(),
        )?;
        let pqo = Object::init_id(
            pqid,
            Protections::READ | Protections::WRITE,
            ObjectInitFlags::empty(),
        )?;

        let kernel_queue =
            CallbackQueueReceiver::new(Queue::<KernelRequest, KernelCompletion>::from(kqo));
        let pager_queue =
            twizzler_queue::QueueSender::new(Queue::<PagerRequest, PagerCompletion>::from(pqo));

        Ok(Self {
            pager_queue,
            kernel_queue,
        })
    }

    pub async fn submit_req(&self, req: PagerRequest) -> PagerCompletion {
        self.pager_queue
            .submit_and_wait(req)
            .await
            .expect("failed to submit pager request")
    }

    pub async fn handle_kernel_req<FC: Future<Output = KernelCompletion>>(
        &self,
        f: impl FnOnce(u32, KernelRequest) -> FC,
    ) -> Result<(), QueueError> {
        self.kernel_queue.handle(f).await
    }
}
