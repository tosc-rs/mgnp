## First take

(this is sort of conceptually right, but not actually right, see below):

```rust
trait Service {
    type Introduction;
    type Confirmation;
    type Rejection;

    type ClientToService;
    type ServiceToClient;

    type ClientDisconnect;
    type ServiceDisconnect;

    const UniqueId: Uuid;
}

enum InterfaceError<R>
where
    R: Wirable,
{
    ServiceNotFound,
    Rejected(R),
    // ...
}

enum InterfaceToClient<S: Service> {
    Message(S::ServiceToClient),
    Disconnect(S::ServiceDisconnect),
}

enum ClientToInterface<S: Service> {
    Message(S::ClientToService),
    Disconnect(S::ClientDisconnect),
}

struct WelcomePackage<S: Service> {
    confirmation: S::Confirmation,
    channel: Receiver<InterfaceToClient<S>>,
}

struct Identity {
    id: Uuid,
    kind: IdentityKind,
}

enum IdentityKind {
    Name(String),
}

trait Interface {
    fn connect<S: Service>(
        &mut self,
        identity: Identity,
        hello: S::Introduction,
        receiver: Receiver<ClientToInterface<S>>,
    ) -> Result<WelcomePackage<S>, InterfaceError<S>>;
}
```

## Okay but we want the interface to be dumb

```
+-----------> Client Node
|
|    +------> Intermediary node
|    |
|    |    +-> Service node
|    |    |
A -> B -> C
|    |    |
|    |    +--> Deserialized
|    |
|    +-------> NOT deserialized, just forwarded
|
+------------> Serialized here, Interface gets Frame
```

So ACTUALLY, this ends up looking a LOT dumber, so the same interface works for both A and B:

```rust
struct WelcomePackage<F: Frame> {
    // Conceptually Vec<u8> => S::Confirmation
    confirmation: F,
    // Conceptually Vec<u8> => InterfaceToClient<S>
    channel: Receiver<F>,
}

trait Interface<F: Frame> {
    fn connect(
        &mut self,
        // Conceptually Vec<u8> => (
        //     Identity,
        //     S::Introduction,
        // )
        hello: F,
        // Conceptually Vec<u8> => ClientToInterface<S>
        receiver: Receiver<F>,
    ) -> Result<WelcomePackage, InterfaceError<F>;
}
```

## So what do wire messages look like?

vibe note: use POSITIVE numbers for client-initiated items, and NEGATIVE numbers for service-initiated items.

For example, if a CLIENT initiates a request, it should use a positive sequence number. Similarly, when establishing a link ID, the CLIENT will be assigned a positive link ID, while the SERVICE side will be assigned a negative link ID.

```rust
// Note: DON'T serde the outer layer - we essentially
// want an "externally tagged enum":
//
// **When link_id == 0:**
//
// struct InterfaceWire<F: Frame> {
//     link_id: 0,
//     frame: WireType<F>,
// }
//
// **Otherwise, it's basically just:**
//
// struct InterfaceWire<F: Frame> {
//     link_id: i16,
//     sequence: i32,
//     frame: F,
// }

enum WireError {
    DecodeError,
    // ...
}

// ONLY valid for link id == 0
enum WireType<F: Frame> {
    Initiate {
        identity: Identity,
        introduction: F,
        initiator_link_id: i16,
        sequence: i32,
    },
    Confirm {
        confirmation: F,
        confirmer_link_id: i16,
        sequence: i32,
    },
    Reject {
        rejection: F,
        sequence: i32,
    },
    WireNak {
        reason: WireError,
        on_link: i16,
    },
    Disconnect {
        disconnect: F,
        on_link: i16,
    }
}
```
