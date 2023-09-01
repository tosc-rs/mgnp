# Specification

The goal of this protocol is to cover the lifecycle of communication between a **Client** and a **Service**.

## Qualities of a Service

A service has the following attributes:

* A services has **declared types** that are associated with it:
    * Types associated with **connection phase**:
        * An **Introduction Message** type
            * This is sent ONCE by the client to request a connection
        * A **Confirmation Message** type
            * This is sent ONCE by a service confirming a connection
        * A **Rejection Message** type
            * This is sent ONCE by a service refusing a connection
    * Types associated with the **steady phase**:
        * A **Client Message** type
            * This is sent MULTIPLE times by the client
        * A **Service Message** type
            * This is sent MULTIPLE times by the service
    * Types associated with the **closing phase**:
        * A **Client Disconnect** type
            * This is sent ONCE by the client
        * A **Service Disconnect** type
            * This is sent ONCE by the service
* A services has a **Unique ID**, as a UUIDv4, that uniquely identifies the service.
  All services with the same Unique ID are guaranteed to have the same **declared types**,
  and are expected to operate in an interchangable manner.
* Each *instance* of a service has a **name**, as a UTF-8 string.
* The tuple of `(name, unique id)` is guaranteed to be unique per machine.

## The connection lifecycle

> NOTE: This assumes a client has already discovered/found the desired service.

* A client sends an Introduction Message to the service
* The service replies with one of:
    * A Confirmation message - a connection is established
    * A Rejection message - no connection is established
* If the connection is established, for a period of time the client and
  service will exchange messages asynchronously:
    * Client Messages, from the client to the service
    * Service Messages, from the service to the client
* At some point, the client or service may choose to end the connection.
    * If the client ends the connection, it sends a Client Disconnect message.
    * If the service ends the connection, it sends a Service Disconnect message.
* At any point, the connection may end unexpectedly, without sending a Disconnect
  message of either kind.

# Vocab

## Client

TODO

## Service

TODO
