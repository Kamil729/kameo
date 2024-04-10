use std::{any, fmt};

use futures::{future::BoxFuture, Future, FutureExt};
use tokio::sync::oneshot;

use crate::{
    actor::ActorRef,
    error::BoxSendError,
    reply::{DelegatedReply, Reply, ReplySender},
};

pub(crate) type BoxDebug = Box<dyn fmt::Debug + Send + 'static>;
pub(crate) type BoxReply = Box<dyn any::Any + Send>;

/// A message that can modify an actors state.
///
/// Messages are processed sequentially one at a time, with exclusive mutable access to the actors state.
///
/// The reply type must implement [Reply].
pub trait Message<T>: Send + 'static {
    /// The reply sent back to the message caller.
    type Reply: Reply + Send + 'static;

    /// Handler for this message.
    fn handle(
        &mut self,
        msg: T,
        ctx: Context<'_, Self, Self::Reply>,
    ) -> impl Future<Output = Self::Reply> + Send;
}

/// Queries the actor for some data.
///
/// Unlike regular messages, queries can be processed by the actor in parallel
/// if multiple queries are sent in sequence. This means queries only have read access
/// to the actors state.
///
/// The reply type must implement [Reply].
pub trait Query<T>: Send + 'static {
    /// The reply sent back to the query caller.
    type Reply: Reply + Send + 'static;

    /// Handler for this query.
    fn handle(
        &self,
        query: T,
        ctx: Context<'_, Self, Self::Reply>,
    ) -> impl Future<Output = Self::Reply> + Send;
}

/// A context provided to message and query handlers providing access
/// to the current actor ref, and reply channel.
#[derive(Debug)]
pub struct Context<'r, A: ?Sized, R: ?Sized>
where
    R: Reply,
{
    actor_ref: ActorRef<A>,
    reply: &'r mut Option<ReplySender<R::Value>>,
}

impl<'r, A, R> Context<'r, A, R>
where
    R: Reply,
{
    pub(crate) fn new(
        actor_ref: ActorRef<A>,
        reply: &'r mut Option<ReplySender<R::Value>>,
    ) -> Self {
        Context { actor_ref, reply }
    }

    /// Returns the current actor's ref, allowing messages to be sent to itself.
    pub fn actor_ref(&self) -> ActorRef<A> {
        self.actor_ref.clone()
    }

    /// Extracts the reply sender, providing a mechanism for delegated responses and an optional reply sender.
    ///
    /// This method is designed for scenarios where the response to a message is not immediate and needs to be
    /// handled by another actor or elsewhere. Upon calling this method, if the reply sender exists (is `Some`),
    /// it must be utilized through [ReplySender::send] to send the response back to the original requester.
    ///
    /// This method returns a tuple consisting of [DelegatedReply] and an optional [ReplySender]. The [DelegatedReply]
    /// is a marker type indicating that the message handler will delegate the task of replying to another part of the
    /// system. It should be returned by the message handler to signify this intention. The [ReplySender], if present,
    /// should be used to actually send the response back to the caller. The [ReplySender] will not be present if the
    /// message was sent as async (no repsonse is needed by the caller).
    ///
    /// # Usage
    ///
    /// - The [DelegatedReply] marker should be returned by the handler to indicate that the response will be delegated.
    /// - The [ReplySender], if not `None`, should be used by the delegated responder to send the actual reply.
    ///
    /// It is important to ensure that [ReplySender::send] is called to complete the transaction and send the response
    /// back to the requester. Failure to do so could result in the requester waiting indefinitely for a response.
    pub fn reply_sender(&mut self) -> (DelegatedReply<R::Value>, Option<ReplySender<R::Value>>) {
        (DelegatedReply::new(), self.reply.take())
    }
}

pub(crate) trait DynMessage<A>
where
    Self: Send,
{
    fn handle_dyn(
        self: Box<Self>,
        state: &mut A,
        actor_ref: ActorRef<A>,
        tx: Option<oneshot::Sender<Result<BoxReply, BoxSendError>>>,
    ) -> BoxFuture<'_, ()>
    where
        A: Send;
    fn as_any(self: Box<Self>) -> Box<dyn any::Any>;
}

impl<A, T> DynMessage<A> for T
where
    A: Message<T>,
    T: Send + 'static,
{
    fn handle_dyn(
        self: Box<Self>,
        state: &mut A,
        actor_ref: ActorRef<A>,
        tx: Option<oneshot::Sender<Result<BoxReply, BoxSendError>>>,
    ) -> BoxFuture<'_, ()>
    where
        A: Send,
    {
        async move {
            let mut reply_sender = tx.map(ReplySender::new);
            let ctx: Context<'_, A, <A as Message<T>>::Reply> =
                Context::new(actor_ref, &mut reply_sender);
            let reply = Message::handle(state, *self, ctx).await;
            if let Some(tx) = reply_sender.take() {
                tx.send(reply.into_value());
            } else if let Some(err) = reply.into_boxed_err() {
                std::panic::panic_any(err);
            }
        }
        .boxed()
    }

    fn as_any(self: Box<Self>) -> Box<dyn any::Any> {
        self
    }
}

pub(crate) trait DynQuery<A>: Send {
    fn handle_dyn(
        self: Box<Self>,
        state: &A,
        actor_ref: ActorRef<A>,
        tx: Option<oneshot::Sender<Result<BoxReply, BoxSendError>>>,
    ) -> BoxFuture<'_, ()>
    where
        A: Send + Sync;
    fn as_any(self: Box<Self>) -> Box<dyn any::Any>;
}

impl<A, T> DynQuery<A> for T
where
    A: Query<T>,
    T: Send + 'static,
{
    fn handle_dyn(
        self: Box<Self>,
        state: &A,
        actor_ref: ActorRef<A>,
        tx: Option<oneshot::Sender<Result<BoxReply, BoxSendError>>>,
    ) -> BoxFuture<'_, ()>
    where
        A: Send + Sync,
    {
        async move {
            let mut reply_sender = tx.map(ReplySender::new);
            let ctx: Context<'_, A, <A as Query<T>>::Reply> =
                Context::new(actor_ref, &mut reply_sender);
            let reply = Query::handle(state, *self, ctx).await;
            if let Some(tx) = reply_sender.take() {
                tx.send(reply.into_value());
            } else if let Some(err) = reply.into_boxed_err() {
                std::panic::panic_any(err);
            }
        }
        .boxed()
    }

    fn as_any(self: Box<Self>) -> Box<dyn any::Any> {
        self
    }
}
