# message_protocol
A protocol for seperating binary messages over TCP, similar to WebSockets but more minimal.


Our protocol for message size is similar to WebSockets, but not exactly the same.

If the first byte is 0-253, this is the length of the message content.

If the first byte is 254, read the next 2 bytes, this 16 bit number is the length.

If the first byte is 255, read the next 8 bytes, this 64 bit number is the length.
