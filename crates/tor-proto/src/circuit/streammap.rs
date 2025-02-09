//! Types and code for mapping StreamIDs to streams on a circuit.

mod counted_map;

use crate::circuit::halfstream::HalfStream;
use crate::circuit::sendme;
use crate::stream::AnyCmdChecker;
use crate::util::stream_poll_set::StreamPollSet;
use crate::{Error, Result};
use tor_cell::relaycell::UnparsedRelayMsg;
/// Mapping from stream ID to streams.
// NOTE: This is a work in progress and I bet I'll refactor it a lot;
// it needs to stay opaque!
use tor_cell::relaycell::{msg::AnyRelayMsg, StreamId};

use futures::channel::mpsc;
use std::num::NonZeroU16;
use tor_error::{bad_api_usage, internal};

use rand::Rng;

use crate::circuit::reactor::RECV_WINDOW_INIT;
use crate::circuit::sendme::StreamRecvWindow;
use tracing::debug;

use self::counted_map::{CountedHashMap, Entry};

/// Entry for an open stream
///
/// (For the purposes of this module, an open stream is one where we have not
/// sent or received any message indicating that the stream is ended.)
pub(super) struct OpenStreamEnt {
    /// Sink to send relay cells tagged for this stream into.
    pub(super) sink: mpsc::Sender<UnparsedRelayMsg>,
    /// Send window, for congestion control purposes.
    pub(super) send_window: sendme::StreamSendWindow,
    /// Number of cells dropped due to the stream disappearing before we can
    /// transform this into an `EndSent`.
    pub(super) dropped: u16,
    /// A `CmdChecker` used to tell whether cells on this stream are valid.
    pub(super) cmd_checker: AnyCmdChecker,
}

/// Entry for a stream where we have sent an END, or other message
/// indicating that the stream is terminated.
pub(super) struct EndSentStreamEnt {
    /// A "half-stream" that we use to check the validity of incoming
    /// messages on this stream.
    pub(super) half_stream: HalfStream,
    /// True if the sender on this stream has been explicitly dropped;
    /// false if we got an explicit close from `close_pending`
    explicitly_dropped: bool,
}

/// The entry for a stream.
enum StreamEnt {
    /// An open stream.
    Open(OpenStreamEnt),
    /// A stream for which we have received an END cell, but not yet
    /// had the stream object get dropped.
    EndReceived,
    /// A stream for which we have sent an END cell but not yet received an END
    /// cell.
    ///
    /// TODO(arti#264) Can we ever throw this out? Do we really get END cells for
    /// these?
    EndSent(EndSentStreamEnt),
}

/// Mutable reference to a stream entry.
///
/// We don't expose `&mut StreamEnt` directly outside this module, to prevent
/// other code from changing it from one of these variants to another: only this module is allowed to do that.
pub(super) enum StreamEntMut<'a> {
    /// An open stream.
    Open(&'a mut OpenStreamEnt),
    /// A stream for which we have received an END cell, but not yet
    /// had the stream object get dropped.
    EndReceived,
    /// A stream for which we have sent an END cell but not yet received an END
    /// cell.
    EndSent(&'a mut EndSentStreamEnt),
}

impl<'a> From<&'a mut StreamEnt> for StreamEntMut<'a> {
    fn from(value: &'a mut StreamEnt) -> Self {
        match value {
            StreamEnt::Open(e) => Self::Open(e),
            StreamEnt::EndReceived => Self::EndReceived,
            StreamEnt::EndSent(e) => Self::EndSent(e),
        }
    }
}

/// Return value to indicate whether or not we send an END cell upon
/// terminating a given stream.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(super) enum ShouldSendEnd {
    /// An END cell should be sent.
    Send,
    /// An END cell should not be sent.
    DontSend,
}

/// Predicate used with CountedHashMap to count open streams.
struct IsOpen;
impl counted_map::Predicate for IsOpen {
    type Item = StreamEnt;

    fn check(item: &Self::Item) -> bool {
        matches!(item, StreamEnt::Open(_))
    }
}

/// A priority for use with [`StreamPollSet`].
#[derive(Debug, Copy, Clone, Eq, PartialEq, PartialOrd, Ord)]
struct Priority(u64);

/// A map from stream IDs to stream entries. Each circuit has one for each
/// hop.
pub(super) struct StreamMap {
    /// Map from StreamId to StreamEnt.  If there is no entry for a
    /// StreamId, that stream doesn't exist.
    // Invariants:
    // * Every open stream also has an entry with the same `StreamId` in `rxs`.
    m: CountedHashMap<StreamId, StreamEnt, IsOpen>,
    /// Streams for cells that should be sent down this stream.
    // Invariants:
    // * Every `StreamId` has an entry in `m` with an open stream.
    rxs: StreamPollSet<StreamId, AnyRelayMsg, Priority, mpsc::Receiver<AnyRelayMsg>>,
    /// The next StreamId that we should use for a newly allocated
    /// circuit.
    next_stream_id: StreamId,
    /// Next priority to use in `rxs`. We implement round-robin scheduling of
    /// handling outgoing messages from streams by assigning a stream the next
    /// priority whenever an outgoing message is processed from that stream,
    /// putting it last in line.
    next_priority: Priority,
}

impl StreamMap {
    /// Make a new empty StreamMap.
    pub(super) fn new() -> Self {
        let mut rng = rand::thread_rng();
        let next_stream_id: NonZeroU16 = rng.gen();
        StreamMap {
            m: CountedHashMap::new(),
            rxs: StreamPollSet::new(),
            next_stream_id: next_stream_id.into(),
            next_priority: Priority(0),
        }
    }

    /// Return an iterator over the entries in this StreamMap.
    // TODO: Consider removing. May no longer be needed.
    #[allow(dead_code)]
    pub(super) fn iter_mut(&mut self) -> impl Iterator<Item = (StreamId, StreamEntMut<'_>)> {
        // CORRECTNESS: before we return any of the value references from this iterator,
        // we convert them into a StreamEntMut,
        // to prevent any changes that would change their Open status.
        self.m
            .iter_mut_unchecked()
            .map(|(id, ent)| (*id, ent.into()))
    }

    /// Return the number of open streams in this map.
    pub(super) fn n_open_streams(&self) -> usize {
        self.m.count()
    }

    /// Return the next available priority.
    fn take_next_priority(&mut self) -> Priority {
        let rv = self.next_priority;
        self.next_priority = Priority(rv.0 + 1);
        rv
    }

    /// Add an entry to this map; return the newly allocated StreamId.
    pub(super) fn add_ent(
        &mut self,
        sink: mpsc::Sender<UnparsedRelayMsg>,
        rx: mpsc::Receiver<AnyRelayMsg>,
        send_window: sendme::StreamSendWindow,
        cmd_checker: AnyCmdChecker,
    ) -> Result<StreamId> {
        let stream_ent = StreamEnt::Open(OpenStreamEnt {
            sink,
            send_window,
            dropped: 0,
            cmd_checker,
        });
        // This "65536" seems too aggressive, but it's what tor does.
        //
        // Also, going around in a loop here is (sadly) needed in order
        // to look like Tor clients.
        for _ in 1..=65536 {
            let id: StreamId = self.next_stream_id;
            self.next_stream_id = wrapping_next_stream_id(self.next_stream_id);
            let ent = self.m.entry(id);
            if let Entry::Vacant(ent) = ent {
                ent.insert(stream_ent);
                let priority = self.take_next_priority();
                self.rxs
                    .try_insert(id, priority, rx)
                    // By
                    // * rxs invariant that every key is also in `m`
                    // * We verified this key is not in `m`.
                    .expect("Unexpected rx entry for unused StreamId");
                return Ok(id);
            }
        }

        Err(Error::IdRangeFull)
    }

    /// Add an entry to this map using the specified StreamId.
    #[cfg(feature = "hs-service")]
    pub(super) fn add_ent_with_id(
        &mut self,
        sink: mpsc::Sender<UnparsedRelayMsg>,
        rx: mpsc::Receiver<AnyRelayMsg>,
        send_window: sendme::StreamSendWindow,
        id: StreamId,
        cmd_checker: AnyCmdChecker,
    ) -> Result<()> {
        let stream_ent = StreamEnt::Open(OpenStreamEnt {
            sink,
            send_window,
            dropped: 0,
            cmd_checker,
        });

        let ent = self.m.entry(id);
        if let Entry::Vacant(ent) = ent {
            ent.insert(stream_ent);
            let priority = self.take_next_priority();
            self.rxs
                .try_insert(id, priority, rx)
                // By
                // * rxs invariant that every key is also in `m`
                // * We verified this key is not in `m`.
                .expect("Unexpected rx");
            Ok(())
        } else {
            Err(Error::IdUnavailable(id))
        }
    }

    /// Return the entry for `id` in this map, if any.
    pub(super) fn get_mut(&mut self, id: StreamId) -> Option<StreamEntMut<'_>> {
        // CORRECTNESS: before we return a reference to one of this map's values,
        // we convert it into a StreamEntMut,
        // to prevent any changes that would change its Open status.
        self.m.get_mut_unchecked(&id).map(StreamEntMut::from)
    }

    /// Note that we received an END message (or other message indicating the end of
    /// the stream) on the stream with `id`.
    ///
    /// Returns true if there was really a stream there.
    pub(super) fn ending_msg_received(&mut self, id: StreamId) -> Result<()> {
        // Check the hashmap for the right stream. Bail if not found.
        // Also keep the hashmap handle so that we can do more efficient inserts/removals
        let mut stream_entry = match self.m.entry(id) {
            Entry::Vacant(_) => {
                return Err(Error::CircProto(
                    "Received END cell on nonexistent stream".into(),
                ))
            }
            Entry::Occupied(o) => o,
        };

        // Progress the stream's state machine accordingly
        match stream_entry.get() {
            StreamEnt::EndReceived => Err(Error::CircProto(
                "Received two END cells on same stream".into(),
            )),
            StreamEnt::EndSent { .. } => {
                debug!("Actually got an end cell on a half-closed stream!");
                // We got an END, and we already sent an END. Great!
                // we can forget about this stream.
                stream_entry.remove_entry();
                Ok(())
            }
            StreamEnt::Open { .. } => {
                stream_entry.insert(StreamEnt::EndReceived);

                Ok(())
            }
        }
    }

    /// Handle a termination of the stream with `id` from this side of
    /// the circuit. Return true if the stream was open and an END
    /// ought to be sent.
    pub(super) fn terminate(
        &mut self,
        id: StreamId,
        why: TerminateReason,
    ) -> Result<ShouldSendEnd> {
        use TerminateReason as TR;

        // Progress the stream's state machine accordingly
        match self
            .m
            .remove(&id)
            .ok_or_else(|| Error::from(internal!("Somehow we terminated a nonexistent stream?")))?
        {
            StreamEnt::EndReceived => Ok(ShouldSendEnd::DontSend),
            StreamEnt::Open(OpenStreamEnt {
                send_window,
                dropped,
                cmd_checker,
                // notably absent: the channels for sink and stream, which will get dropped and
                // closed (meaning reads/writes from/to this stream will now fail)
                ..
            }) => {
                // FIXME(eta): we don't copy the receive window, instead just creating a new one,
                //             so a malicious peer can send us slightly more data than they should
                //             be able to; see arti#230.
                let mut recv_window = StreamRecvWindow::new(RECV_WINDOW_INIT);
                recv_window.decrement_n(dropped)?;
                // TODO: would be nice to avoid new_ref.
                let half_stream = HalfStream::new(send_window, recv_window, cmd_checker);
                let explicitly_dropped = why == TR::StreamTargetClosed;
                self.m.insert(
                    id,
                    StreamEnt::EndSent(EndSentStreamEnt {
                        half_stream,
                        explicitly_dropped,
                    }),
                );
                self.rxs
                    .remove(&id)
                    // By:
                    // * Invariant on `m` that every open stream has a corresponding entry in `rxs`.
                    // * We verified above that this id had an open stream in `m`.
                    .expect("Missing receiver");

                Ok(ShouldSendEnd::Send)
            }
            StreamEnt::EndSent(EndSentStreamEnt {
                ref mut explicitly_dropped,
                ..
            }) => match (*explicitly_dropped, why) {
                (false, TR::StreamTargetClosed) => {
                    *explicitly_dropped = true;
                    Ok(ShouldSendEnd::DontSend)
                }
                (true, TR::StreamTargetClosed) => {
                    Err(bad_api_usage!("Tried to close an already closed stream.").into())
                }
                (_, TR::ExplicitEnd) => Err(bad_api_usage!(
                    "Tried to end an already closed stream. (explicitly_dropped={:?})",
                    *explicitly_dropped
                )
                .into()),
            },
        }
    }

    /// Get an up-to-date iterator of streams with ready items. `Option<AnyRelayMsg>::None`
    /// indicates that the local sender has been dropped.
    ///
    /// Conceptually all streams are in a queue; new streams are added to the
    /// back of the queue, and a stream is sent to the back of the queue
    /// whenever a ready message is taken from it (via
    /// [`Self::take_ready_msg`]). The returned iterator is an ordered view of
    /// this queue, showing the subset of streams that have a message ready to
    /// send, or whose sender has been dropped.
    pub(super) fn poll_ready_streams_iter<'a>(
        &'a mut self,
        cx: &mut std::task::Context,
    ) -> impl Iterator<Item = (StreamId, Option<&'a AnyRelayMsg>, &'a OpenStreamEnt)> + 'a {
        self.rxs.poll_ready_iter(cx).map(|(sid, msg, _priority)| {
            let Some(StreamEnt::Open(o)) = self.m.get(sid) else {
                // By:
                // * Invariant on `rxs` that every key has a corresponding open strema in `m`.
                panic!("Missing open stream");
            };
            (*sid, msg, o)
        })
    }

    /// If the stream `sid` has a message ready, take it, and reprioritize `sid`
    /// to the "back of the line" with respec to
    /// [`Self::poll_ready_streams_iter`].
    pub(super) fn take_ready_msg(&mut self, sid: StreamId) -> Option<AnyRelayMsg> {
        let new_priority = self.take_next_priority();
        let (_prev_priority, val) = self
            .rxs
            .take_ready_value_and_reprioritize(&sid, new_priority)?;
        Some(val)
    }

    // TODO: Eventually if we want relay support, we'll need to support
    // stream IDs chosen by somebody else. But for now, we don't need those.
}

/// A reason for terminating a stream.
///
/// We use this type in order to ensure that we obey the API restrictions of [`StreamMap::terminate`]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum TerminateReason {
    /// Closing a stream because the receiver got `Ok(None)`, indicating that the
    /// corresponding senders were all dropped.
    StreamTargetClosed,
    /// Closing a stream because we were explicitly told to end it via
    /// [`StreamTarget::close_pending`](crate::circuit::StreamTarget::close_pending).
    ExplicitEnd,
}

/// Convenience function for doing a wrapping increment of a `StreamId`.
fn wrapping_next_stream_id(id: StreamId) -> StreamId {
    let next_val = NonZeroU16::from(id)
        .checked_add(1)
        .unwrap_or_else(|| NonZeroU16::new(1).expect("Impossibly got 0 value"));
    next_val.into()
}

#[cfg(test)]
mod test {
    // @@ begin test lint list maintained by maint/add_warning @@
    #![allow(clippy::bool_assert_comparison)]
    #![allow(clippy::clone_on_copy)]
    #![allow(clippy::dbg_macro)]
    #![allow(clippy::mixed_attributes_style)]
    #![allow(clippy::print_stderr)]
    #![allow(clippy::print_stdout)]
    #![allow(clippy::single_char_pattern)]
    #![allow(clippy::unwrap_used)]
    #![allow(clippy::unchecked_duration_subtraction)]
    #![allow(clippy::useless_vec)]
    #![allow(clippy::needless_pass_by_value)]
    //! <!-- @@ end test lint list maintained by maint/add_warning @@ -->
    use super::*;
    use crate::{circuit::sendme::StreamSendWindow, stream::DataCmdChecker};

    #[test]
    fn test_wrapping_next_stream_id() {
        let one = StreamId::new(1).unwrap();
        let two = StreamId::new(2).unwrap();
        let max = StreamId::new(0xffff).unwrap();
        assert_eq!(wrapping_next_stream_id(one), two);
        assert_eq!(wrapping_next_stream_id(max), one);
    }

    #[test]
    fn streammap_basics() -> Result<()> {
        let mut map = StreamMap::new();
        let mut next_id = map.next_stream_id;
        let mut ids = Vec::new();

        // Try add_ent
        for _ in 0..128 {
            let (sink, _) = mpsc::channel(128);
            let (_, rx) = mpsc::channel(2);
            let id = map.add_ent(
                sink,
                rx,
                StreamSendWindow::new(500),
                DataCmdChecker::new_any(),
            )?;
            let expect_id: StreamId = next_id;
            assert_eq!(expect_id, id);
            next_id = wrapping_next_stream_id(next_id);
            ids.push(id);
        }

        // Test get_mut.
        let nonesuch_id = next_id;
        assert!(matches!(
            map.get_mut(ids[0]),
            Some(StreamEntMut::Open { .. })
        ));
        assert!(map.get_mut(nonesuch_id).is_none());

        // Test end_received
        assert!(map.ending_msg_received(nonesuch_id).is_err());
        assert!(map.ending_msg_received(ids[1]).is_ok());
        assert!(matches!(
            map.get_mut(ids[1]),
            Some(StreamEntMut::EndReceived)
        ));
        assert!(map.ending_msg_received(ids[1]).is_err());

        // Test terminate
        use TerminateReason as TR;
        assert!(map.terminate(nonesuch_id, TR::ExplicitEnd).is_err());
        assert_eq!(
            map.terminate(ids[2], TR::ExplicitEnd).unwrap(),
            ShouldSendEnd::Send
        );
        assert!(matches!(
            map.get_mut(ids[2]),
            Some(StreamEntMut::EndSent { .. })
        ));
        assert_eq!(
            map.terminate(ids[1], TR::ExplicitEnd).unwrap(),
            ShouldSendEnd::DontSend
        );
        assert!(map.get_mut(ids[1]).is_none());

        // Try receiving an end after a terminate.
        assert!(map.ending_msg_received(ids[2]).is_ok());
        assert!(map.get_mut(ids[2]).is_none());

        Ok(())
    }
}
