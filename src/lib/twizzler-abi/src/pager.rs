#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub struct RequestFromKernel {
    cmd: KernelCommand,
}

impl RequestFromKernel {
    pub fn new(cmd: KernelCommand) -> Self {
        Self { cmd }
    }

    pub fn cmd(&self) -> KernelCommand {
        self.cmd
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub enum KernelCommand {
    EchoReq,
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub struct CompletionToKernel {
    data: KernelCompletionData,
}

impl CompletionToKernel {
    pub fn new(data: KernelCompletionData) -> Self {
        Self { data }
    }

    pub fn data(&self) -> KernelCompletionData {
        self.data
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub enum KernelCompletionData {
    EchoResp,
}
