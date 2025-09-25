# Gates

Gates, also known as secure API calls, are a means of exposing a limited set of functions available to an external user. This is similar to system calls, where the user can call into the kernel to do specific actions. Gates are used for interprocess communication.

Gates are a way of an object exposing a system call like interface. This allows an object to define arbitrary behavior other threads can call. Because external threads can only access the object through the gate, they are restricted from detrimental actions, provided the gate is correctly written. While this does place the responsibility for secure code in the hands of any programmer rather than the typical relegation of secure code to security experts, gates are optional and can be avoided if there is worry about security flaws.

When writing gates, best security practices are required to avoid vulnerabilities in the gates. As such, beware of timing attacks and other side channels that can be used to subtly exploit the object.
