//! Request objects used to implement onion services.
//!
//! These requests are yielded on a stream, and the calling code needs to decide
//! whether to permit or reject them.

use std::net::SocketAddr;

use tor_linkspec::OwnedChanTarget;
use tor_llcrypto::pk::curve25519;
use tor_proto::stream::DataStream;

/// Request to complete an introduction/rendezvous handshake.
///
/// A request of this kind indicates that a client has asked permission to
/// connect to an onion service through an introduction point.  The caller needs
/// to decide whether or not to complete the handshake.
///
/// Protocol details: More specifically, we create one of these whenever we get a well-formed
/// `INTRODUCE2` message.  Based on this, the caller decides whether to send a
/// `RENDEZVOUS1` message.
#[derive(Debug)]
pub struct RendRequest {
    /// Which introduction point gave us this request?
    from_intro_point: crate::svc::IntroPointId,

    /// What proof-of-work did the client send us?
    proof_of_work_provided: Option<ProofOfWork>,

    /// Information about the rendezvous point that the client wanted us to
    /// connect to.
    rend_pt: RendPt,
    //
    // TODO HSS: We'll also need additional information to actually complete the
    // request, maybe including a Weak<OnionService>, or maybe including a
    // oneshot::Sender.
}

/// Information needed to complete a rendezvous handshake.
#[derive(Debug, Clone)]
struct RendPt {
    /// The location and identity of the rendezvous point.
    location: OwnedChanTarget,
    /// The public Ntor key for the rendezvous point.
    ntor_key: curve25519::PublicKey,
    /// Cryptographic state to use when completing the handshake.
    ///
    /// TODO HSS: This is not at all final, and should be refactored freely.
    handshake: HandshakeState,
}

/// The cryptographic state needed to complete an introduce/rendezvous
/// handshake.
#[derive(Debug, Clone)]
struct HandshakeState {
    // TODO HSS: replace this type or its contents as needed.
}

/// Information about a proof of work received from a client's introduction
/// point.
///  
// Todo: use Beth's API instead.
#[derive(Debug, Clone)]
enum ProofOfWork {
    /// TODO HSS document or replace.
    EquixV1 {
        /// TODO HSS document or replace
        effort_level: usize,
    },
}

/// A request from a client to open a new stream to an onion service.
///
/// We can only receive these _after_ we have already permitted the client to
/// connect via a [`RendRequest`].
///
/// Protocol details: More specifically, we create one of these whenever we get a well-formed
/// `BEGIN` message.  Based on this, the caller decides whether to send a
/// `CONNECTED` message.
#[derive(Debug)]
pub struct StreamRequest {
    /// The object that will be used to send data to and from the client.
    ///
    /// TODO HSS: Possibly instead this will be some type from tor_proto that
    /// can turn into a DataStream.
    stream: DataStream,

    /// The address that the client has asked to connect to.
    ///
    /// TODO HSS: This is the wrong type! It may be a hostname.
    target: SocketAddr,
}

/// A stream opened over an onion service.
//
// TODO HSS: This may belong in another module.
#[derive(Debug)]
pub struct OnionServiceDataStream {
    /// The underlying data stream; this type is just a thin wrapper.
    inner: DataStream,
}
