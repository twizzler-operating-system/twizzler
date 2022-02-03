# Gates {#gates}

Gates are IPC or Secure API calls

Gates are a way of an object exposing a system call like interface. This allows an object to define arbitrary behavior other threads can call. Because external threads can only access the object through the gate, they are restricted from detrimental actions, provided the gate is correctly written. While this does place the responsibility for secure code in the hands of any programmer rather than the typical relegation of secure code to security experts, gates are optional and can be avoided if there is worry about security flaws.


They are secure because there are stringent requirements for where you can enter (similar to
Google's 👿 Native Client).