# Thinking day 2

So, what parts are there?

## The Registry

The registry takes some kind of identifier, and optionally gives back somewhere to send the Introduction message to. The registry also needs to have some way of turning a Frame into an Introduction message.

The Service will return an Accept or a Decline, which will also need to be serialized back into a Frame.

The Registry might also want to have a "get all services" api for the interface to query later when we want to do discovery.

The Registry (or something else) might also want to have a "get all interfaces" API for remotes to be able to "spider" the network later when we want to do discovery.

## Frames

Frames are a type that are conceptually something like `Vec<u8>`. The idea is to keep the
type generic so we can support devices with or without a global allocator. Devices without
will need to use something like a pool allocator

They might need a couple of specific abilities, like:

* Ability to "consume" bytes from the front, so we can "hide" the header without realloc
* Ability to have separate size/capacity

For example, we might have a pool allocator with 32 byte slabs.

```
 0                                     31
 v                                     v
[XXXX_XXXX_XXXX_XXXX_XXXX_XXXX_XXXX_XXXX]
 |-| -> HDR
    |-----------------| -> Body
                       |---------------| -> Unused
```

In this case, the header was 3 bytes, and the body was 15 bytes. The remaining 14 bytes are
unused.

## The Wire

The wire needs to be able to send and receive Frames.

## The Interface

The interface has potentially a lot of jobs.

* Handling all outgoing messages


## Tricky Pipe

We might want some kind of tricky pipe. The tricky part is that each end (and the pipe
itself!) actually has two options:

* The Sender can send either `T` or `Frame`
* The Receiver can receive either `T` or `Frame`
* The pipe itself can hold either `T` or `Frame`.

| Sender    | Receiver  | Pipe      | Use Case                                      |
| :---      | :---      | :---      | :---                                          |
| T         | T         | T         | Client/Service on same machine                |
| T         | T         | Frame     | Not allowed                                   |
| T         | Frame     | T         | C/S sending to interface? (Intfc Ser)         |
| T         | Frame     | Frame     | C/S sending to interface? (C/S Ser)           |
| Frame     | T         | T         | Interface sending to C/S? (Intfc Deser)       |
| Frame     | T         | Frame     | Interface sending to C/S? (C/S Deser)         |
| Frame     | Frame     | T         | Not allowed                                   |
| Frame     | Frame     | Frame     | Interface sending to Interface (no ser/de)    |

C/S already have one channel per connection. It feels like they should handle the ser/de,
because they know the right type. That being said, we could handle this at connection
time.

Can we make the `Receiver` and `Sender`s have the same type, even if they aren't the same under the hood (e.g. one that ser/des from a Frame pipe, and one that doesn't)? So what would this selection look like?

| Sender    | Pipe      | Receiver  | Use Case                                      |
| :---      | :---      | :---      | :---                                          |
| T         | T         | T         | Client/Service on same machine                |
| T         | Frame     | Frame     | C/S sending to interface? (C/S Ser)           |
| Frame     | Frame     | T         | Interface sending to C/S? (C/S Deser)         |
| Frame     | Frame     | Frame     | Interface sending to Interface (no ser/de)    |

| Sender    | Pipe      | Receiver  | Use Case                                      |
| :---      | :---      | :---      | :---                                          |
| T         | T         | T         | No Frames                                     |
| T         | Frame     | Frame     | Summoned and given away                       |
| Frame     | Frame     | T         | Given and thrown away                         |
| Frame     | Frame     | Frame     | Given and given away                          |

| Sender    | Pipe      | Receiver  | Use Case
| :---      | :---      | :---      | :---
| T         | T         | T         | Never needs to store frames                   |
| T         | T         | Frame     | Frame must be created at withdrawl            |
| Frame     | T         | T         | Never needs to store frames                   |
| Frame     | Frame     | Frame     | Stores frames, but 1 in one out               |
