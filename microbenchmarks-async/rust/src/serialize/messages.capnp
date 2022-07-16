@0x94a43df6c359e805;

using Rust = import "rust.capnp";
$Rust.parentModule("serialize");

struct System {
    union {
        request   @0 :Request;
        reply     @1 :Reply;
        consensus @2 :Consensus;
        observerMessage  @3 :ObserverMessage;
    }
}

struct Request {
    sessionId   @0 :UInt32;
    operationId @1 :UInt32;
    data        @2 :Data;
}

struct Reply {
    sessionId   @0 :UInt32;
    operationId @1 :UInt32;
    data        @2 :Data;
}

struct Consensus {
    seqNo @0 :UInt32;
    view  @1 :UInt32;
    union {
        prePrepare @2 :List(ForwardedRequest);
        prepare    @3 :Data;
        commit     @4 :Data;
    }
}

struct ForwardedRequest {
    header  @0 :Data;
    request @1 :Request;
}

struct ObserverMessage {

    messageType: union {
        observerRegister         @0 :Void;
        observerRegisterResponse @1 :Bool;
        observerUnregister       @2 :Void;
        observedValue            @3 :ObservedValue;
    }

}

struct ObservedValue {

    value: union {
        checkpointStart     @0 :UInt32;
        checkpointEnd       @1 :UInt32;
        consensus           @2 :UInt32;
        normalPhase         @3 :NormalPhase;
        viewChange          @4 :Void;
        collabStateTransfer @5 :Void;
    }

}

struct NormalPhase {

    view   @0 :UInt32;
    seqNum @1 :UInt32;
    leader @2 :UInt32;

}