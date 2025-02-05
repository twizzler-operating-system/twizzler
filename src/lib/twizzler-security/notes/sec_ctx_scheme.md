what do we gotta do?

need to come up with a scheme for how the object is going to be organized
- delegations are variable length
  - could be any length
- capabilities are fixed length
  - 79 bytes long (as of now)

Ideas for how this could work

1. LUT / Dictionary method

Set aside n bytes at the start of a security context such that
it specifies all the capabilities for the objects inside the object
and what "byte ranges" are for each capability.

would just have to parse this Dict first and then we would have O(1) access to the cap we want
