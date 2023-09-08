pub trait Runtime: ThreadRuntime + ObjectRuntime + CoreRuntime {}

pub trait ThreadRuntime {}

pub trait ObjectRuntime {}

pub trait CoreRuntime {}
