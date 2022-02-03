% Permissions

A thread has permission to access an object if:
- They have not been restricted by a mask (including global mask)
- The thread has the capability, or delegated capability. (by attaching to a security
  context).
- The thread knows the object's name. (security by obscurity)

## Permission values for objects

There are 5 permissions an object can have: read, write, execute, use, and delete. Except for use (and to an extent delete), these permissions exist in Unix systems, and are used in the same way.
- **Read:** This allows a thread the ability to look at the contents of an object.
- **Write:** This allows a thread the ability to modify an object. 
- **Execute:** The object can be run as a program.
- **Delete:** The object can be deleted. Usually Unix systems include this as part of write
  permissions, and Windows systems allow this to be a separate permission.
- **Use:** This marks the object as available for the kernel to operate on, such as a kernel state
  object, further explained on [kernel state objects](./KSO.md).

## Masks

Masks further restrict permissions to objects

We do not need signatures on masks because **TODO**

Like umask of Linux

## Capabilities

Capabilities are when permissions a provided to objects as tokens, where the program can access the data if it has a valid token. Unlike previous implementations of capability systems, Twizzler includes an object ID as part of the capability signature to prevent a capability from being stolen by leaking the signature to malicious parties. While this does require identity to be checked in addition to the validity of the signature, this prevents simple leaks of secrets from breaking the security of an object.

## Delegation

Delegation allow for capabilities to be shared and futher restricted with other views. 

## Late Binding Access Control
