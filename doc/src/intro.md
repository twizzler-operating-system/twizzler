# User Space Documentation

## Introduction

Since Twizzler is a microkernel, many operations handled by monolithic kernels are instead implemented in userspace. 

## Where to begin

Twizzler introduces objects to organize persistent data, rather than files in traditional systems. This provides the benefit of not having to serialize and deserialize data to make it persistent.

Pages explaining the  main abstractions of the OS are available at the following links: [Objects](./Object.md) (for the main data abstraction), [Views](./Views.md) (for thread environments), and [Kernel State Objects](./KSO.md) (the security model). From these basics, there are a number of features provided by the Twizzler userspace that can be used to enhance programs, but are not necessary for understanding the fundamentals of the OS.

- To get a background on the motivations and a high level understanding of the goals of the operating system, we recommend [Twizzler: a Data-Centric OS for Non-Volatile Memory](https://dl.acm.org/doi/10.1145/3454129). This is a research paper explaining the system for academic readers.
- To just jump in, follow the [build guide](./BUILD.md) and look at [the code documentation](https://twizzler-operating-system.github.io/nightly/doc/) (essentially manual pages) for primitive functions.