use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use sentry_types::protocol::v7::SpanId;

use crate::{protocol, Hub};

#[cfg(feature = "client")]
use crate::Client;

#[cfg(feature = "client")]
const MAX_SPANS: usize = 1_000;

// global API:

/// Start a new Performance Monitoring Transaction.
///
/// The transaction needs to be explicitly finished via [`Transaction::finish`],
/// otherwise it will be discarded.
/// The transaction itself also represents the root span in the span hierarchy.
/// Child spans can be started with the [`Transaction::start_child`] method.
pub fn start_transaction(ctx: TransactionContext) -> Transaction {
    #[cfg(feature = "client")]
    {
        let client = Hub::with_active(|hub| hub.client());
        Transaction::new(client, ctx)
    }
    #[cfg(not(feature = "client"))]
    {
        Transaction::new_noop(ctx)
    }
}

/// Start a new Performance Monitoring Transaction with the provided start timestamp.
///
/// The transaction needs to be explicitly finished via [`Transaction::finish`],
/// otherwise it will be discarded.
/// The transaction itself also represents the root span in the span hierarchy.
/// Child spans can be started with the [`Transaction::start_child`] method.
pub fn start_transaction_with_timestamp(
    ctx: TransactionContext,
    timestamp: SystemTime,
) -> Transaction {
    let transaction = start_transaction(ctx);
    if let Some(tx) = transaction.inner.lock().unwrap().transaction.as_mut() {
        tx.start_timestamp = timestamp;
    }
    transaction
}

// Hub API:

impl Hub {
    /// Start a new Performance Monitoring Transaction.
    ///
    /// See the global [`start_transaction`] for more documentation.
    pub fn start_transaction(&self, ctx: TransactionContext) -> Transaction {
        #[cfg(feature = "client")]
        {
            Transaction::new(self.client(), ctx)
        }
        #[cfg(not(feature = "client"))]
        {
            Transaction::new_noop(ctx)
        }
    }

    /// Start a new Performance Monitoring Transaction with the provided start timestamp.
    ///
    /// See the global [`start_transaction_with_timestamp`] for more documentation.
    pub fn start_transaction_with_timestamp(
        &self,
        ctx: TransactionContext,
        timestamp: SystemTime,
    ) -> Transaction {
        let transaction = start_transaction(ctx);
        if let Some(tx) = transaction.inner.lock().unwrap().transaction.as_mut() {
            tx.start_timestamp = timestamp;
        }
        transaction
    }
}

// "Context" Types:

/// Arbitrary data passed by the caller, when starting a transaction.
///
/// May be inspected by the user in the `traces_sampler` callback, if set.
///
/// Represents arbitrary JSON data, the top level of which must be a map.
pub type CustomTransactionContext = serde_json::Map<String, serde_json::Value>;

/// The Transaction Context used to start a new Performance Monitoring Transaction.
///
/// The Transaction Context defines the metadata for a Performance Monitoring
/// Transaction, and also the connection point for distributed tracing.
#[derive(Debug, Clone)]
pub struct TransactionContext {
    #[cfg_attr(not(feature = "client"), allow(dead_code))]
    name: String,
    op: String,
    trace_id: protocol::TraceId,
    parent_span_id: Option<protocol::SpanId>,
    span_id: protocol::SpanId,
    sampled: Option<bool>,
    custom: Option<CustomTransactionContext>,
}

impl TransactionContext {
    /// Creates a new Transaction Context with the given `name` and `op`. A random
    /// `trace_id` is assigned. Use [`TransactionContext::new_with_trace_id`] to
    /// specify a custom trace ID.
    ///
    /// See <https://docs.sentry.io/platforms/native/enriching-events/transaction-name/>
    /// for an explanation of a Transaction's `name`, and
    /// <https://develop.sentry.dev/sdk/performance/span-operations/> for conventions
    /// around an `operation`'s value.
    ///
    /// See also the [`TransactionContext::continue_from_headers`] function that
    /// can be used for distributed tracing.
    #[must_use = "this must be used with `start_transaction`"]
    pub fn new(name: &str, op: &str) -> Self {
        Self::new_with_trace_id(name, op, protocol::TraceId::default())
    }

    /// Creates a new Transaction Context with the given `name`, `op`, and `trace_id`.
    ///
    /// See <https://docs.sentry.io/platforms/native/enriching-events/transaction-name/>
    /// for an explanation of a Transaction's `name`, and
    /// <https://develop.sentry.dev/sdk/performance/span-operations/> for conventions
    /// around an `operation`'s value.
    #[must_use = "this must be used with `start_transaction`"]
    pub fn new_with_trace_id(name: &str, op: &str, trace_id: protocol::TraceId) -> Self {
        Self {
            name: name.into(),
            op: op.into(),
            trace_id,
            parent_span_id: None,
            span_id: Default::default(),
            sampled: None,
            custom: None,
        }
    }

    /// Creates a new Transaction Context with the given `name`, `op`, `trace_id`, and
    /// possibly the given `span_id` and `parent_span_id`.
    ///
    /// See <https://docs.sentry.io/platforms/native/enriching-events/transaction-name/>
    /// for an explanation of a Transaction's `name`, and
    /// <https://develop.sentry.dev/sdk/performance/span-operations/> for conventions
    /// around an `operation`'s value.
    #[must_use = "this must be used with `start_transaction`"]
    pub fn new_with_details(
        name: &str,
        op: &str,
        trace_id: protocol::TraceId,
        span_id: Option<protocol::SpanId>,
        parent_span_id: Option<protocol::SpanId>,
    ) -> Self {
        let mut slf = Self::new_with_trace_id(name, op, trace_id);
        if let Some(span_id) = span_id {
            slf.span_id = span_id;
        }
        slf.parent_span_id = parent_span_id;
        slf
    }

    /// Creates a new Transaction Context based on the distributed tracing `headers`.
    ///
    /// The `headers` in particular need to include the `sentry-trace` header,
    /// which is used to associate the transaction with a distributed trace.
    #[must_use = "this must be used with `start_transaction`"]
    pub fn continue_from_headers<'a, I: IntoIterator<Item = (&'a str, &'a str)>>(
        name: &str,
        op: &str,
        headers: I,
    ) -> Self {
        parse_headers(headers)
            .map(|sentry_trace| Self::continue_from_sentry_trace(name, op, &sentry_trace, None))
            .unwrap_or_else(|| Self {
                name: name.into(),
                op: op.into(),
                trace_id: Default::default(),
                parent_span_id: None,
                span_id: Default::default(),
                sampled: None,
                custom: None,
            })
    }

    /// Creates a new Transaction Context based on the provided distributed tracing data,
    /// optionally creating the `TransactionContext` with the provided `span_id`.
    pub fn continue_from_sentry_trace(
        name: &str,
        op: &str,
        sentry_trace: &SentryTrace,
        span_id: Option<SpanId>,
    ) -> Self {
        Self {
            name: name.into(),
            op: op.into(),
            trace_id: sentry_trace.trace_id,
            parent_span_id: Some(sentry_trace.span_id),
            sampled: sentry_trace.sampled,
            span_id: span_id.unwrap_or_default(),
            custom: None,
        }
    }

    /// Creates a new Transaction Context based on an existing Span.
    ///
    /// This should be used when an independent computation is spawned on another
    /// thread and should be connected to the calling thread via a distributed
    /// tracing transaction.
    pub fn continue_from_span(name: &str, op: &str, span: Option<TransactionOrSpan>) -> Self {
        let span = match span {
            Some(span) => span,
            None => return Self::new(name, op),
        };

        let (trace_id, parent_span_id, sampled) = match span {
            TransactionOrSpan::Transaction(transaction) => {
                let inner = transaction.inner.lock().unwrap();
                (
                    inner.context.trace_id,
                    inner.context.span_id,
                    Some(inner.sampled),
                )
            }
            TransactionOrSpan::Span(span) => {
                let sampled = span.sampled;
                let span = span.span.lock().unwrap();
                (span.trace_id, span.span_id, Some(sampled))
            }
        };

        Self {
            name: name.into(),
            op: op.into(),
            trace_id,
            parent_span_id: Some(parent_span_id),
            span_id: protocol::SpanId::default(),
            sampled,
            custom: None,
        }
    }

    /// Set the sampling decision for this Transaction.
    ///
    /// This can be either an explicit boolean flag, or [`None`], which will fall
    /// back to use the configured `traces_sample_rate` option.
    pub fn set_sampled(&mut self, sampled: impl Into<Option<bool>>) {
        self.sampled = sampled.into();
    }

    /// Get the sampling decision for this Transaction.
    pub fn sampled(&self) -> Option<bool> {
        self.sampled
    }

    /// Get the name of this Transaction.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the operation of this Transaction.
    pub fn operation(&self) -> &str {
        &self.op
    }

    /// Get the Trace ID of this Transaction.
    pub fn trace_id(&self) -> protocol::TraceId {
        self.trace_id
    }

    /// Get the Span ID of this Transaction.
    pub fn span_id(&self) -> protocol::SpanId {
        self.span_id
    }

    /// Get the custom context of this Transaction.
    pub fn custom(&self) -> Option<&CustomTransactionContext> {
        self.custom.as_ref()
    }

    /// Update the custom context of this Transaction.
    ///
    /// For simply adding a key, use the `custom_insert` method.
    pub fn custom_mut(&mut self) -> &mut Option<CustomTransactionContext> {
        &mut self.custom
    }

    /// Inserts a key-value pair into the custom context.
    ///
    /// If the context did not have this key present, None is returned.
    ///
    /// If the context did have this key present, the value is updated, and the old value is
    /// returned.
    pub fn custom_insert(
        &mut self,
        key: String,
        value: serde_json::Value,
    ) -> Option<serde_json::Value> {
        // Get the custom context
        let mut custom = None;
        std::mem::swap(&mut self.custom, &mut custom);

        // Initialise the context, if not used yet
        let mut custom = custom.unwrap_or_default();

        // And set our key
        let existing_value = custom.insert(key, value);
        self.custom = Some(custom);
        existing_value
    }

    /// Creates a transaction context builder initialized with the given `name` and `op`.
    ///
    /// See <https://docs.sentry.io/platforms/native/enriching-events/transaction-name/>
    /// for an explanation of a Transaction's `name`, and
    /// <https://develop.sentry.dev/sdk/performance/span-operations/> for conventions
    /// around an `operation`'s value.
    #[must_use]
    pub fn builder(name: &str, op: &str) -> TransactionContextBuilder {
        TransactionContextBuilder {
            ctx: TransactionContext::new(name, op),
        }
    }
}

/// A transaction context builder created by [`TransactionContext::builder`].
pub struct TransactionContextBuilder {
    ctx: TransactionContext,
}

impl TransactionContextBuilder {
    /// Defines the name of the transaction.
    #[must_use]
    pub fn with_name(mut self, name: String) -> Self {
        self.ctx.name = name;
        self
    }

    /// Defines the operation of the transaction.
    #[must_use]
    pub fn with_op(mut self, op: String) -> Self {
        self.ctx.op = op;
        self
    }

    /// Defines the trace ID.
    #[must_use]
    pub fn with_trace_id(mut self, trace_id: protocol::TraceId) -> Self {
        self.ctx.trace_id = trace_id;
        self
    }

    /// Defines a parent span ID for the created transaction.
    #[must_use]
    pub fn with_parent_span_id(mut self, parent_span_id: Option<protocol::SpanId>) -> Self {
        self.ctx.parent_span_id = parent_span_id;
        self
    }

    /// Defines the span ID to be used when creating the transaction.
    #[must_use]
    pub fn with_span_id(mut self, span_id: protocol::SpanId) -> Self {
        self.ctx.span_id = span_id;
        self
    }

    /// Defines whether the transaction will be sampled.
    #[must_use]
    pub fn with_sampled(mut self, sampled: Option<bool>) -> Self {
        self.ctx.sampled = sampled;
        self
    }

    /// Adds a custom key and value to the transaction context.
    #[must_use]
    pub fn with_custom(mut self, key: String, value: serde_json::Value) -> Self {
        self.ctx.custom_insert(key, value);
        self
    }

    /// Finishes building a transaction.
    pub fn finish(self) -> TransactionContext {
        self.ctx
    }
}

/// A function to be run for each new transaction, to determine the rate at which
/// it should be sampled.
///
/// This function may choose to respect the sampling of the parent transaction (`ctx.sampled`)
/// or ignore it.
pub type TracesSampler = dyn Fn(&TransactionContext) -> f32 + Send + Sync;

// global API types:

/// A wrapper that groups a [`Transaction`] and a [`Span`] together.
#[derive(Clone, Debug)]
pub enum TransactionOrSpan {
    /// A [`Transaction`].
    Transaction(Transaction),
    /// A [`Span`].
    Span(Span),
}

impl From<Transaction> for TransactionOrSpan {
    fn from(transaction: Transaction) -> Self {
        Self::Transaction(transaction)
    }
}

impl From<Span> for TransactionOrSpan {
    fn from(span: Span) -> Self {
        Self::Span(span)
    }
}

impl TransactionOrSpan {
    /// Set some extra information to be sent with this Transaction/Span.
    pub fn set_data(&self, key: &str, value: protocol::Value) {
        match self {
            TransactionOrSpan::Transaction(transaction) => transaction.set_data(key, value),
            TransactionOrSpan::Span(span) => span.set_data(key, value),
        }
    }

    /// Sets a tag to a specific value.
    pub fn set_tag<V: ToString>(&self, key: &str, value: V) {
        match self {
            TransactionOrSpan::Transaction(transaction) => transaction.set_tag(key, value),
            TransactionOrSpan::Span(span) => span.set_tag(key, value),
        }
    }

    /// Get the TransactionContext of the Transaction/Span.
    ///
    /// Note that this clones the underlying value.
    pub fn get_trace_context(&self) -> protocol::TraceContext {
        match self {
            TransactionOrSpan::Transaction(transaction) => transaction.get_trace_context(),
            TransactionOrSpan::Span(span) => span.get_trace_context(),
        }
    }

    /// Set the status of the Transaction/Span.
    pub fn get_status(&self) -> Option<protocol::SpanStatus> {
        match self {
            TransactionOrSpan::Transaction(transaction) => transaction.get_status(),
            TransactionOrSpan::Span(span) => span.get_status(),
        }
    }

    /// Set the status of the Transaction/Span.
    pub fn set_status(&self, status: protocol::SpanStatus) {
        match self {
            TransactionOrSpan::Transaction(transaction) => transaction.set_status(status),
            TransactionOrSpan::Span(span) => span.set_status(status),
        }
    }

    /// Set the HTTP request information for this Transaction/Span.
    pub fn set_request(&self, request: protocol::Request) {
        match self {
            TransactionOrSpan::Transaction(transaction) => transaction.set_request(request),
            TransactionOrSpan::Span(span) => span.set_request(request),
        }
    }

    /// Returns the headers needed for distributed tracing.
    /// Use [`crate::Scope::iter_trace_propagation_headers`] to obtain the active
    /// trace's distributed tracing headers.
    pub fn iter_headers(&self) -> TraceHeadersIter {
        match self {
            TransactionOrSpan::Transaction(transaction) => transaction.iter_headers(),
            TransactionOrSpan::Span(span) => span.iter_headers(),
        }
    }

    /// Get the sampling decision for this Transaction/Span.
    pub fn is_sampled(&self) -> bool {
        match self {
            TransactionOrSpan::Transaction(transaction) => transaction.is_sampled(),
            TransactionOrSpan::Span(span) => span.is_sampled(),
        }
    }

    /// Starts a new child Span with the given `op` and `description`.
    ///
    /// The span must be explicitly finished via [`Span::finish`], as it will
    /// otherwise not be sent to Sentry.
    #[must_use = "a span must be explicitly closed via `finish()`"]
    pub fn start_child(&self, op: &str, description: &str) -> Span {
        match self {
            TransactionOrSpan::Transaction(transaction) => transaction.start_child(op, description),
            TransactionOrSpan::Span(span) => span.start_child(op, description),
        }
    }

    /// Starts a new child Span with the given `op`, `description` and `id`.
    ///
    /// The span must be explicitly finished via [`Span::finish`], as it will
    /// otherwise not be sent to Sentry.
    #[must_use = "a span must be explicitly closed via `finish()`"]
    pub fn start_child_with_details(
        &self,
        op: &str,
        description: &str,
        id: SpanId,
        timestamp: SystemTime,
    ) -> Span {
        match self {
            TransactionOrSpan::Transaction(transaction) => {
                transaction.start_child_with_details(op, description, id, timestamp)
            }
            TransactionOrSpan::Span(span) => {
                span.start_child_with_details(op, description, id, timestamp)
            }
        }
    }

    #[cfg(feature = "client")]
    pub(crate) fn apply_to_event(&self, event: &mut protocol::Event<'_>) {
        if event.contexts.contains_key("trace") {
            return;
        }

        let context = match self {
            TransactionOrSpan::Transaction(transaction) => {
                transaction.inner.lock().unwrap().context.clone()
            }
            TransactionOrSpan::Span(span) => {
                let span = span.span.lock().unwrap();
                protocol::TraceContext {
                    span_id: span.span_id,
                    trace_id: span.trace_id,
                    ..Default::default()
                }
            }
        };
        event.contexts.insert("trace".into(), context.into());
    }

    /// Finishes the Transaction/Span with the provided end timestamp.
    ///
    /// This records the end timestamp and either sends the inner [`Transaction`]
    /// directly to Sentry, or adds the [`Span`] to its transaction.
    pub fn finish_with_timestamp(self, timestamp: SystemTime) {
        match self {
            TransactionOrSpan::Transaction(transaction) => {
                transaction.finish_with_timestamp(timestamp)
            }
            TransactionOrSpan::Span(span) => span.finish_with_timestamp(timestamp),
        }
    }

    /// Finishes the Transaction/Span.
    ///
    /// This records the current timestamp as the end timestamp and either sends the inner [`Transaction`]
    /// directly to Sentry, or adds the [`Span`] to its transaction.
    pub fn finish(self) {
        match self {
            TransactionOrSpan::Transaction(transaction) => transaction.finish(),
            TransactionOrSpan::Span(span) => span.finish(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct TransactionInner {
    #[cfg(feature = "client")]
    client: Option<Arc<Client>>,
    sampled: bool,
    pub(crate) context: protocol::TraceContext,
    pub(crate) transaction: Option<protocol::Transaction<'static>>,
}

type TransactionArc = Arc<Mutex<TransactionInner>>;

/// Functional implementation of how a new transaction's sample rate is chosen.
///
/// Split out from `Client.is_transaction_sampled` for testing.
#[cfg(feature = "client")]
fn transaction_sample_rate(
    traces_sampler: Option<&TracesSampler>,
    ctx: &TransactionContext,
    traces_sample_rate: f32,
) -> f32 {
    match (traces_sampler, traces_sample_rate) {
        (Some(traces_sampler), _) => traces_sampler(ctx),
        (None, traces_sample_rate) => ctx
            .sampled
            .map(|sampled| if sampled { 1.0 } else { 0.0 })
            .unwrap_or(traces_sample_rate),
    }
}

/// Determine whether the new transaction should be sampled.
#[cfg(feature = "client")]
impl Client {
    fn is_transaction_sampled(&self, ctx: &TransactionContext) -> bool {
        let client_options = self.options();
        self.sample_should_send(transaction_sample_rate(
            client_options.traces_sampler.as_deref(),
            ctx,
            client_options.traces_sample_rate,
        ))
    }
}

/// A running Performance Monitoring Transaction.
///
/// The transaction needs to be explicitly finished via [`Transaction::finish`],
/// otherwise neither the transaction nor any of its child spans will be sent
/// to Sentry.
#[derive(Clone, Debug)]
pub struct Transaction {
    pub(crate) inner: TransactionArc,
}

/// Iterable for a transaction's [data attributes](protocol::TraceContext::data).
pub struct TransactionData<'a>(MutexGuard<'a, TransactionInner>);

impl<'a> TransactionData<'a> {
    /// Iterate over the [data attributes](protocol::TraceContext::data)
    /// associated with this [transaction][protocol::Transaction].
    ///
    /// If the transaction is not sampled for sending,
    /// the metadata will not be populated at all,
    /// so the produced iterator is empty.
    pub fn iter(&self) -> Box<dyn Iterator<Item = (&String, &protocol::Value)> + '_> {
        if self.0.transaction.is_some() {
            Box::new(self.0.context.data.iter())
        } else {
            Box::new(std::iter::empty())
        }
    }

    /// Set a data attribute to be sent with this Transaction.
    pub fn set_data(&mut self, key: Cow<'a, str>, value: protocol::Value) {
        if self.0.transaction.is_some() {
            self.0.context.data.insert(key.into(), value);
        }
    }

    /// Set a tag to be sent with this Transaction.
    pub fn set_tag(&mut self, key: Cow<'_, str>, value: String) {
        if let Some(transaction) = self.0.transaction.as_mut() {
            transaction.tags.insert(key.into(), value);
        }
    }
}

impl Transaction {
    #[cfg(feature = "client")]
    fn new(client: Option<Arc<Client>>, ctx: TransactionContext) -> Self {
        let (sampled, transaction) = match client.as_ref() {
            Some(client) => (
                client.is_transaction_sampled(&ctx),
                Some(protocol::Transaction {
                    name: Some(ctx.name),
                    ..Default::default()
                }),
            ),
            None => (ctx.sampled.unwrap_or(false), None),
        };

        let context = protocol::TraceContext {
            trace_id: ctx.trace_id,
            parent_span_id: ctx.parent_span_id,
            span_id: ctx.span_id,
            op: Some(ctx.op),
            ..Default::default()
        };

        Self {
            inner: Arc::new(Mutex::new(TransactionInner {
                client,
                sampled,
                context,
                transaction,
            })),
        }
    }

    #[cfg(not(feature = "client"))]
    fn new_noop(ctx: TransactionContext) -> Self {
        let context = protocol::TraceContext {
            trace_id: ctx.trace_id,
            parent_span_id: ctx.parent_span_id,
            op: Some(ctx.op),
            ..Default::default()
        };
        let sampled = ctx.sampled.unwrap_or(false);

        Self {
            inner: Arc::new(Mutex::new(TransactionInner {
                sampled,
                context,
                transaction: None,
            })),
        }
    }

    /// Set a data attribute to be sent with this Transaction.
    pub fn set_data(&self, key: &str, value: protocol::Value) {
        let mut inner = self.inner.lock().unwrap();
        if inner.transaction.is_some() {
            inner.context.data.insert(key.into(), value);
        }
    }

    /// Set some extra information to be sent with this Transaction.
    pub fn set_extra(&self, key: &str, value: protocol::Value) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(transaction) = inner.transaction.as_mut() {
            transaction.extra.insert(key.into(), value);
        }
    }

    /// Sets a tag to a specific value.
    pub fn set_tag<V: ToString>(&self, key: &str, value: V) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(transaction) = inner.transaction.as_mut() {
            transaction.tags.insert(key.into(), value.to_string());
        }
    }

    /// Returns an iterating accessor to the transaction's
    /// [data attributes](protocol::TraceContext::data).
    ///
    /// # Concurrency
    /// In order to obtain any kind of reference to the `TraceContext::data` field,
    /// a `Mutex` needs to be locked. The returned `TransactionData` holds on to this lock
    /// for as long as it lives. Therefore you must take care not to keep the returned
    /// `TransactionData` around too long or it will never relinquish the lock and you may run into
    /// a deadlock.
    pub fn data(&self) -> TransactionData {
        TransactionData(self.inner.lock().unwrap())
    }

    /// Get the TransactionContext of the Transaction.
    ///
    /// Note that this clones the underlying value.
    pub fn get_trace_context(&self) -> protocol::TraceContext {
        let inner = self.inner.lock().unwrap();
        inner.context.clone()
    }

    /// Get the status of the Transaction.
    pub fn get_status(&self) -> Option<protocol::SpanStatus> {
        let inner = self.inner.lock().unwrap();
        inner.context.status
    }

    /// Set the status of the Transaction.
    pub fn set_status(&self, status: protocol::SpanStatus) {
        let mut inner = self.inner.lock().unwrap();
        inner.context.status = Some(status);
    }

    /// Set the HTTP request information for this Transaction.
    pub fn set_request(&self, request: protocol::Request) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(transaction) = inner.transaction.as_mut() {
            transaction.request = Some(request);
        }
    }

    /// Returns the headers needed for distributed tracing.
    /// Use [`crate::Scope::iter_trace_propagation_headers`] to obtain the active
    /// trace's distributed tracing headers.
    pub fn iter_headers(&self) -> TraceHeadersIter {
        let inner = self.inner.lock().unwrap();
        let trace = SentryTrace::new(
            inner.context.trace_id,
            inner.context.span_id,
            Some(inner.sampled),
        );
        TraceHeadersIter {
            sentry_trace: Some(trace.to_string()),
        }
    }

    /// Get the sampling decision for this Transaction.
    pub fn is_sampled(&self) -> bool {
        self.inner.lock().unwrap().sampled
    }

    /// Finishes the Transaction with the provided end timestamp.
    ///
    /// This records the end timestamp and sends the transaction together with
    /// all finished child spans to Sentry.
    pub fn finish_with_timestamp(self, _timestamp: SystemTime) {
        with_client_impl! {{
            let mut inner = self.inner.lock().unwrap();

            // Discard `Transaction` unless sampled.
            if !inner.sampled {
                return;
            }

            if let Some(mut transaction) = inner.transaction.take() {
                if let Some(client) = inner.client.take() {
                    transaction.finish_with_timestamp(_timestamp);
                    transaction
                        .contexts
                        .insert("trace".into(), inner.context.clone().into());

                    Hub::current().with_current_scope(|scope| scope.apply_to_transaction(&mut transaction));
                    let opts = client.options();
                    transaction.release.clone_from(&opts.release);
                    transaction.environment.clone_from(&opts.environment);
                    transaction.sdk = Some(std::borrow::Cow::Owned(client.sdk_info.clone()));
                    transaction.server_name.clone_from(&opts.server_name);

                    drop(inner);

                    let mut envelope = protocol::Envelope::new();
                    envelope.add_item(transaction);

                    client.send_envelope(envelope)
                }
            }
        }}
    }

    /// Finishes the Transaction.
    ///
    /// This records the current timestamp as the end timestamp and sends the transaction together with
    /// all finished child spans to Sentry.
    pub fn finish(self) {
        self.finish_with_timestamp(SystemTime::now());
    }

    /// Starts a new child Span with the given `op` and `description`.
    ///
    /// The span must be explicitly finished via [`Span::finish`].
    #[must_use = "a span must be explicitly closed via `finish()`"]
    pub fn start_child(&self, op: &str, description: &str) -> Span {
        let inner = self.inner.lock().unwrap();
        let span = protocol::Span {
            trace_id: inner.context.trace_id,
            parent_span_id: Some(inner.context.span_id),
            op: Some(op.into()),
            description: if description.is_empty() {
                None
            } else {
                Some(description.into())
            },
            ..Default::default()
        };
        Span {
            transaction: Arc::clone(&self.inner),
            sampled: inner.sampled,
            span: Arc::new(Mutex::new(span)),
        }
    }

    /// Starts a new child Span with the given `op` and `description`.
    ///
    /// The span must be explicitly finished via [`Span::finish`].
    #[must_use = "a span must be explicitly closed via `finish()`"]
    pub fn start_child_with_details(
        &self,
        op: &str,
        description: &str,
        id: SpanId,
        timestamp: SystemTime,
    ) -> Span {
        let inner = self.inner.lock().unwrap();
        let span = protocol::Span {
            trace_id: inner.context.trace_id,
            parent_span_id: Some(inner.context.span_id),
            op: Some(op.into()),
            description: if description.is_empty() {
                None
            } else {
                Some(description.into())
            },
            span_id: id,
            start_timestamp: timestamp,
            ..Default::default()
        };
        Span {
            transaction: Arc::clone(&self.inner),
            sampled: inner.sampled,
            span: Arc::new(Mutex::new(span)),
        }
    }
}

/// A smart pointer to a span's [`data` field](protocol::Span::data).
pub struct Data<'a>(MutexGuard<'a, protocol::Span>);

impl Data<'_> {
    /// Set some extra information to be sent with this Span.
    pub fn set_data(&mut self, key: String, value: protocol::Value) {
        self.0.data.insert(key, value);
    }

    /// Set some tag to be sent with this Span.
    pub fn set_tag(&mut self, key: String, value: String) {
        self.0.tags.insert(key, value);
    }
}

impl Deref for Data<'_> {
    type Target = BTreeMap<String, protocol::Value>;

    fn deref(&self) -> &Self::Target {
        &self.0.data
    }
}

impl DerefMut for Data<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0.data
    }
}

/// A running Performance Monitoring Span.
///
/// The span needs to be explicitly finished via [`Span::finish`], otherwise it
/// will not be sent to Sentry.
#[derive(Clone, Debug)]
pub struct Span {
    pub(crate) transaction: TransactionArc,
    sampled: bool,
    span: SpanArc,
}

type SpanArc = Arc<Mutex<protocol::Span>>;

impl Span {
    /// Set some extra information to be sent with this Transaction.
    pub fn set_data(&self, key: &str, value: protocol::Value) {
        let mut span = self.span.lock().unwrap();
        span.data.insert(key.into(), value);
    }

    /// Sets a tag to a specific value.
    pub fn set_tag<V: ToString>(&self, key: &str, value: V) {
        let mut span = self.span.lock().unwrap();
        span.tags.insert(key.into(), value.to_string());
    }

    /// Returns a smart pointer to the span's [`data` field](protocol::Span::data).
    ///
    /// Since [`Data`] implements `Deref` and `DerefMut`, this can be used to read and mutate
    /// the span data.
    ///
    /// # Concurrency
    /// In order to obtain any kind of reference to the `data` field,
    /// a `Mutex` needs to be locked. The returned `Data` holds on to this lock
    /// for as long as it lives. Therefore you must take care not to keep the returned
    /// `Data` around too long or it will never relinquish the lock and you may run into
    /// a deadlock.
    pub fn data(&self) -> Data {
        Data(self.span.lock().unwrap())
    }

    /// Get the TransactionContext of the Span.
    ///
    /// Note that this clones the underlying value.
    pub fn get_trace_context(&self) -> protocol::TraceContext {
        let transaction = self.transaction.lock().unwrap();
        transaction.context.clone()
    }

    /// Get the current span ID.
    pub fn get_span_id(&self) -> protocol::SpanId {
        let span = self.span.lock().unwrap();
        span.span_id
    }

    /// Get the status of the Span.
    pub fn get_status(&self) -> Option<protocol::SpanStatus> {
        let span = self.span.lock().unwrap();
        span.status
    }

    /// Set the status of the Span.
    pub fn set_status(&self, status: protocol::SpanStatus) {
        let mut span = self.span.lock().unwrap();
        span.status = Some(status);
    }

    /// Set the HTTP request information for this Span.
    pub fn set_request(&self, request: protocol::Request) {
        let mut span = self.span.lock().unwrap();
        // Extract values from the request to be used as data in the span.
        if let Some(method) = request.method {
            span.data.insert("method".into(), method.into());
        }
        if let Some(url) = request.url {
            span.data.insert("url".into(), url.to_string().into());
        }
        if let Some(data) = request.data {
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&data) {
                span.data.insert("data".into(), data);
            } else {
                span.data.insert("data".into(), data.into());
            }
        }
        if let Some(query_string) = request.query_string {
            span.data.insert("query_string".into(), query_string.into());
        }
        if let Some(cookies) = request.cookies {
            span.data.insert("cookies".into(), cookies.into());
        }
        if !request.headers.is_empty() {
            if let Ok(headers) = serde_json::to_value(request.headers) {
                span.data.insert("headers".into(), headers);
            }
        }
        if !request.env.is_empty() {
            if let Ok(env) = serde_json::to_value(request.env) {
                span.data.insert("env".into(), env);
            }
        }
    }

    /// Returns the headers needed for distributed tracing.
    /// Use [`crate::Scope::iter_trace_propagation_headers`] to obtain the active
    /// trace's distributed tracing headers.
    pub fn iter_headers(&self) -> TraceHeadersIter {
        let span = self.span.lock().unwrap();
        let trace = SentryTrace::new(span.trace_id, span.span_id, Some(self.sampled));
        TraceHeadersIter {
            sentry_trace: Some(trace.to_string()),
        }
    }

    /// Get the sampling decision for this Span.
    pub fn is_sampled(&self) -> bool {
        self.sampled
    }

    /// Finishes the Span with the provided end timestamp.
    ///
    /// This will record the end timestamp and add the span to the transaction
    /// in which it was started.
    pub fn finish_with_timestamp(self, _timestamp: SystemTime) {
        with_client_impl! {{
            let mut span = self.span.lock().unwrap();
            if span.timestamp.is_some() {
                // the span was already finished
                return;
            }
            span.finish_with_timestamp(_timestamp);
            let mut inner = self.transaction.lock().unwrap();
            if let Some(transaction) = inner.transaction.as_mut() {
                if transaction.spans.len() <= MAX_SPANS {
                    transaction.spans.push(span.clone());
                }
            }
        }}
    }

    /// Finishes the Span.
    ///
    /// This will record the current timestamp as the end timestamp and add the span to the
    /// transaction in which it was started.
    pub fn finish(self) {
        self.finish_with_timestamp(SystemTime::now());
    }

    /// Starts a new child Span with the given `op` and `description`.
    ///
    /// The span must be explicitly finished via [`Span::finish`].
    #[must_use = "a span must be explicitly closed via `finish()`"]
    pub fn start_child(&self, op: &str, description: &str) -> Span {
        let span = self.span.lock().unwrap();
        let span = protocol::Span {
            trace_id: span.trace_id,
            parent_span_id: Some(span.span_id),
            op: Some(op.into()),
            description: if description.is_empty() {
                None
            } else {
                Some(description.into())
            },
            ..Default::default()
        };
        Span {
            transaction: self.transaction.clone(),
            sampled: self.sampled,
            span: Arc::new(Mutex::new(span)),
        }
    }

    /// Starts a new child Span with the given `op` and `description`.
    ///
    /// The span must be explicitly finished via [`Span::finish`].
    #[must_use = "a span must be explicitly closed via `finish()`"]
    fn start_child_with_details(
        &self,
        op: &str,
        description: &str,
        id: SpanId,
        timestamp: SystemTime,
    ) -> Span {
        let span = self.span.lock().unwrap();
        let span = protocol::Span {
            trace_id: span.trace_id,
            parent_span_id: Some(span.span_id),
            op: Some(op.into()),
            description: if description.is_empty() {
                None
            } else {
                Some(description.into())
            },
            span_id: id,
            start_timestamp: timestamp,
            ..Default::default()
        };
        Span {
            transaction: self.transaction.clone(),
            sampled: self.sampled,
            span: Arc::new(Mutex::new(span)),
        }
    }
}

/// Represents a key-value pair such as an HTTP header.
pub type TraceHeader = (&'static str, String);

/// An Iterator over HTTP header names and values needed for distributed tracing.
///
/// This currently only yields the `sentry-trace` header, but other headers
/// may be added in the future.
pub struct TraceHeadersIter {
    sentry_trace: Option<String>,
}

impl TraceHeadersIter {
    #[cfg(feature = "client")]
    pub(crate) fn new(sentry_trace: String) -> Self {
        Self {
            sentry_trace: Some(sentry_trace),
        }
    }
}

impl Iterator for TraceHeadersIter {
    type Item = (&'static str, String);

    fn next(&mut self) -> Option<Self::Item> {
        self.sentry_trace.take().map(|st| ("sentry-trace", st))
    }
}

/// A container for distributed tracing metadata that can be extracted from e.g. the `sentry-trace`
/// HTTP header.
#[derive(Debug, PartialEq, Clone, Copy, Default)]
pub struct SentryTrace {
    pub(crate) trace_id: protocol::TraceId,
    pub(crate) span_id: protocol::SpanId,
    pub(crate) sampled: Option<bool>,
}

impl SentryTrace {
    /// Creates a new [`SentryTrace`] from the provided parameters
    pub fn new(
        trace_id: protocol::TraceId,
        span_id: protocol::SpanId,
        sampled: Option<bool>,
    ) -> Self {
        SentryTrace {
            trace_id,
            span_id,
            sampled,
        }
    }
}

fn parse_sentry_trace(header: &str) -> Option<SentryTrace> {
    let header = header.trim();
    let mut parts = header.splitn(3, '-');

    let trace_id = parts.next()?.parse().ok()?;
    let parent_span_id = parts.next()?.parse().ok()?;
    let parent_sampled = parts.next().and_then(|sampled| match sampled {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    });

    Some(SentryTrace::new(trace_id, parent_span_id, parent_sampled))
}

/// Extracts distributed tracing metadata from headers (or, generally, key-value pairs),
/// considering the values for `sentry-trace`.
pub fn parse_headers<'a, I: IntoIterator<Item = (&'a str, &'a str)>>(
    headers: I,
) -> Option<SentryTrace> {
    let mut trace = None;
    for (k, v) in headers.into_iter() {
        if k.eq_ignore_ascii_case("sentry-trace") {
            trace = parse_sentry_trace(v);
            break;
        }
    }
    trace
}

impl std::fmt::Display for SentryTrace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}", self.trace_id, self.span_id)?;
        if let Some(sampled) = self.sampled {
            write!(f, "-{}", if sampled { '1' } else { '0' })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn parses_sentry_trace() {
        let trace_id = protocol::TraceId::from_str("09e04486820349518ac7b5d2adbf6ba5").unwrap();
        let parent_trace_id = protocol::SpanId::from_str("9cf635fa5b870b3a").unwrap();

        let trace = parse_sentry_trace("09e04486820349518ac7b5d2adbf6ba5-9cf635fa5b870b3a-0");
        assert_eq!(
            trace,
            Some(SentryTrace::new(trace_id, parent_trace_id, Some(false)))
        );

        let trace = SentryTrace::new(Default::default(), Default::default(), None);
        let parsed = parse_sentry_trace(&trace.to_string());
        assert_eq!(parsed, Some(trace));
    }

    #[test]
    fn disabled_forwards_trace_id() {
        let headers = [(
            "SenTrY-TRAce",
            "09e04486820349518ac7b5d2adbf6ba5-9cf635fa5b870b3a-1",
        )];
        let ctx = TransactionContext::continue_from_headers("noop", "noop", headers);
        let trx = start_transaction(ctx);

        let span = trx.start_child("noop", "noop");

        let header = span.iter_headers().next().unwrap().1;
        let parsed = parse_sentry_trace(&header).unwrap();

        assert_eq!(
            &parsed.trace_id.to_string(),
            "09e04486820349518ac7b5d2adbf6ba5"
        );
        assert_eq!(parsed.sampled, Some(true));
    }

    #[test]
    fn transaction_context_public_getters() {
        let mut ctx = TransactionContext::new("test-name", "test-operation");
        assert_eq!(ctx.name(), "test-name");
        assert_eq!(ctx.operation(), "test-operation");
        assert_eq!(ctx.sampled(), None);

        ctx.set_sampled(true);
        assert_eq!(ctx.sampled(), Some(true));
    }

    #[cfg(feature = "client")]
    #[test]
    fn compute_transaction_sample_rate() {
        // Global rate used as fallback.
        let ctx = TransactionContext::new("noop", "noop");
        assert_eq!(transaction_sample_rate(None, &ctx, 0.3), 0.3);
        assert_eq!(transaction_sample_rate(None, &ctx, 0.7), 0.7);

        // If only global rate, setting sampled overrides it
        let mut ctx = TransactionContext::new("noop", "noop");
        ctx.set_sampled(true);
        assert_eq!(transaction_sample_rate(None, &ctx, 0.3), 1.0);
        ctx.set_sampled(false);
        assert_eq!(transaction_sample_rate(None, &ctx, 0.3), 0.0);

        // If given, sampler function overrides everything else.
        let mut ctx = TransactionContext::new("noop", "noop");
        assert_eq!(transaction_sample_rate(Some(&|_| { 0.7 }), &ctx, 0.3), 0.7);
        ctx.set_sampled(false);
        assert_eq!(transaction_sample_rate(Some(&|_| { 0.7 }), &ctx, 0.3), 0.7);
        // But the sampler may choose to inspect parent sampling
        let sampler = |ctx: &TransactionContext| match ctx.sampled() {
            Some(true) => 0.8,
            Some(false) => 0.4,
            None => 0.6,
        };
        ctx.set_sampled(true);
        assert_eq!(transaction_sample_rate(Some(&sampler), &ctx, 0.3), 0.8);
        ctx.set_sampled(None);
        assert_eq!(transaction_sample_rate(Some(&sampler), &ctx, 0.3), 0.6);

        // Can use first-class and custom attributes of the context.
        let sampler = |ctx: &TransactionContext| {
            if ctx.name() == "must-name" || ctx.operation() == "must-operation" {
                return 1.0;
            }

            if let Some(custom) = ctx.custom() {
                if let Some(rate) = custom.get("rate") {
                    if let Some(rate) = rate.as_f64() {
                        return rate as f32;
                    }
                }
            }

            0.1
        };
        // First class attributes
        let ctx = TransactionContext::new("noop", "must-operation");
        assert_eq!(transaction_sample_rate(Some(&sampler), &ctx, 0.3), 1.0);
        let ctx = TransactionContext::new("must-name", "noop");
        assert_eq!(transaction_sample_rate(Some(&sampler), &ctx, 0.3), 1.0);
        // Custom data payload
        let mut ctx = TransactionContext::new("noop", "noop");
        ctx.custom_insert("rate".to_owned(), serde_json::json!(0.7));
        assert_eq!(transaction_sample_rate(Some(&sampler), &ctx, 0.3), 0.7);
    }
}
