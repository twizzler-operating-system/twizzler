# Kernel State Objects {#kso}

These are normal objects used by both userspace programs and the kernel. For them to be used by the
kernel, the use permission must be set. To learn more about the use permission, see @ref permissions
"Permissions".

## Security Contexts

A security context is an object that contains information about which objects can be accessed and
how (such as managing capabilities). A thread attaches to the security context to gain access to the
objects. This can be useful for operations similar to the `sudo` command on UNIX, where privileges
are temporarily increased in order to perform certain privileged operations without fully changing
user ID.

Additionally, security contexts can be used to limit permissions. To prevent a limited thread from
shedding their limited permission state, attached contexts can be set as undetachable.

## Views

To learn more, see the @ref views "page on views".