# Plan for mocking the security model


## objects
- Alice's private data
- Bob's private data

- the security contexts are also objects but for the purposes
of just logically playing with the idea lets just keep them
as structs

## Security contexts
- Alice's "user" context
  - holds the capability for alice's private data
- [x ] Bob's "user" context

  - holds the capability for bobs private data

- "Root" ctx
  - holds capabilities for all objects

- remember that the security context is the accessor when creating the capability

- need a user space context representation


## processes
- Both Alice and Bob are running and will both try to access all objects

## bootstrapping
- going to start with the alice and bob threads already attached to their
respective security contexts because the init process would do that
on sign in (im too lazy to do allat)
