# Permissions

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
  object, further explained on [kernel state objects](./KSO.md). Often times this is used for attaching a thread to a security context.

## Masks

Masks further restrict permissions to objects. This is similar to `umask` in Unix systems. For example, while by default any object may have access to an object called *bloom*, we may want a specific security context called *Fall* to not have access to the object.

We do not need signatures on masks because they are part of the security context, meaning threads can only modify the mask if they can modify the security context object.

## Capabilities

Capabilities are when permissions a provided to objects as tokens, where the program can access the data if it has a valid token. Unlike previous implementations of capability systems, Twizzler includes an object ID as part of the capability signature to prevent a capability from being stolen by leaking the signature to malicious parties. While this does require identity to be checked in addition to the validity of the signature, this prevents simple leaks of secrets from breaking the security of an object.

## Delegation

Delegation allow for capabilities to be shared and futher restricted with other views. In order to delegate a capability, it must have high permissions within the object it wishes to delegate (enough so as to access the private key of the object).

## Late Binding Access Control

Rather than checking an object when it is initially accessed, such as in Unix with a call to `open()`, Twizzler checks access at the time when the operation is done, such as a read or write. This means that a thread can open an object with more permissions than allowed and not cause a fault, and only once that illegal operation is attempted will the fault occur.

This method for enforcing access control is different from Unix systems because the kernel is not involved for memory access, which is how Twizzler formats all data. However protection still exists because when loading a security context, the MMU is programmed to limit access.
