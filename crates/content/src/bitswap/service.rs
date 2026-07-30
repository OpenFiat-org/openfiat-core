//! Bitswap traffic, moved on and off the node's actor thread.
//!
//! # Why this is two halves rather than one loop
//!
//! The node's state — its held content, its attachment records — lives
//! behind `Rc` on a single-threaded actor, because every RPC handler in
//! this workspace reads it synchronously. Stream I/O cannot run there: a
//! peer that stops reading mid-message would stall every RPC the node
//! serves for as long as it liked.
//!
//! So the reading and writing happen in spawned tasks, which hold nothing
//! but a `Control` and bytes, and the *decisions* happen on the actor,
//! which holds the state and no sockets. [`spawn_inbound`] delivers parsed
//! messages to the actor over a channel; [`spawn_send`] takes a finished
//! reply away from it. Neither one touches the store, and the actor never
//! awaits a peer.

use super::message::Message;
use super::serve::{PROTOCOL, read_all, write_message};
use futures::StreamExt;
use libp2p_identity::PeerId;
use libp2p_stream::{AlreadyRegistered, Control};
use tokio::sync::mpsc::UnboundedSender;

/// Accepts inbound bitswap streams, forwarding every message to `actor`.
///
/// Returns `Err` if something already claimed the protocol — which would
/// mean two things were serving content on one identity, and is a bug
/// rather than a condition to recover from.
///
/// One accept loop covers both directions deliberately. A reply to
/// something this node asked for arrives as an inbound stream exactly
/// like a stranger's wantlist does, because that is how bitswap answers;
/// there is no second channel to listen on, and a node that registered
/// one would be waiting on a stream no peer will ever open.
pub fn spawn_inbound(
    control: &mut Control,
    actor: UnboundedSender<(PeerId, Message)>,
) -> Result<(), AlreadyRegistered> {
    let mut incoming = control.accept(PROTOCOL)?;

    tokio::spawn(async move {
        while let Some((peer, mut stream)) = incoming.next().await {
            let actor = actor.clone();
            // Per stream, so one slow or silent peer delays only itself.
            // The alternative — reading them in turn — is the same
            // stall this module exists to keep off the actor, moved one
            // step away and no less real.
            tokio::spawn(async move {
                for message in read_all(&mut stream).await {
                    if actor.send((peer, message)).is_err() {
                        // The actor is gone, so the node is shutting
                        // down. Nothing left to deliver to.
                        return;
                    }
                }
            });
        }
    });

    Ok(())
}

/// Sends one message to `peer` on a stream of its own.
///
/// Fire and forget, because bitswap is: there is no acknowledgement to
/// wait for, and the answer to a wantlist — if it comes at all — arrives
/// later through [`spawn_inbound`]. A failure to open the stream means
/// the peer is gone or does not speak bitswap, neither of which the
/// caller can do anything about.
pub fn spawn_send(mut control: Control, peer: PeerId, message: Message) {
    if message.is_empty() {
        return;
    }
    tokio::spawn(async move {
        match control.open_stream(peer, PROTOCOL).await {
            Ok(mut stream) => {
                if let Err(err) = write_message(&mut stream, &message).await {
                    tracing::debug!(%peer, %err, "could not send a bitswap message");
                }
            }
            Err(err) => tracing::debug!(%peer, %err, "peer does not accept bitswap streams"),
        }
    });
}

/// A wantlist for `cids`, asking to be told when a peer does not have one.
///
/// `sendDontHave` is set on every entry because this node asks a specific
/// set of peers rather than the open network. On the open network silence
/// is cheap and common; here, a peer that stays silent about content it
/// lacks is indistinguishable from one that has gone away, and this node
/// would keep asking it.
pub fn wantlist(cids: &[openfiat_crypto::Cid]) -> Message {
    Message {
        wants: cids
            .iter()
            .map(|cid| super::Want {
                cid: cid.clone(),
                want_type: super::WantType::Block,
                cancel: false,
                send_dont_have: true,
            })
            .collect(),
        ..Message::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    #[test]
    fn a_wantlist_asks_for_the_bytes_and_to_be_told_about_absence() {
        let cids = [fixtures::probe_cid(), fixtures::other_cid()];
        let message = wantlist(&cids);

        assert_eq!(message.wants.len(), 2);
        assert!(message.wants.iter().all(|w| w.send_dont_have && !w.cancel));
        assert!(
            message
                .wants
                .iter()
                .all(|w| w.want_type == super::super::WantType::Block)
        );
        assert!(message.blocks.is_empty() && message.presences.is_empty());
    }

    #[test]
    fn a_wantlist_for_nothing_is_empty_so_no_stream_is_opened_for_it() {
        assert!(wantlist(&[]).is_empty());
    }
}
