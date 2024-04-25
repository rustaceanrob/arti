//! Declare RPC functionality on for the `arti-client` crate.

use derive_deftly::Deftly;
use futures::{SinkExt as _, StreamExt as _};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use tor_rpcbase as rpc;
use tor_rtcompat::Runtime;

use crate::TorClient;

impl<R: Runtime> crate::TorClient<R> {
    /// Ensure that every RPC method is registered for this instantiation of TorClient.
    ///
    /// We can't use [`rpc::static_rpc_invoke_fn`] for these, since TorClient is
    /// parameterized.
    pub fn register_rpc_methods(table: &mut rpc::DispatchTable) {
        table.insert(rpc::invoker_ent!(get_client_status::<R>));
        table.insert(rpc::invoker_ent!(watch_client_status::<R>));
    }
}

/// RPC method: Return the current ClientStatusInfo.
#[derive(Deftly, Debug, Serialize, Deserialize)]
#[derive_deftly(rpc::DynMethod)]
#[deftly(rpc(method_name = "get-client-status"))]
struct GetClientStatus {}

impl rpc::Method for GetClientStatus {
    type Output = ClientStatusInfo;
    type Update = rpc::NoUpdates;
}

/// RPC method: Run forever, delivering an updated view of the ClientStatusInfo whenever it changes.
///
/// (This method can return updates that have no visible changes.)
#[derive(Deftly, Debug, Serialize, Deserialize)]
#[derive_deftly(rpc::DynMethod)]
#[deftly(rpc(method_name = "watch-client-status"))]
struct WatchClientStatus {}

impl rpc::Method for WatchClientStatus {
    type Output = rpc::Nil;
    type Update = ClientStatusInfo;
}

/// RPC result: The reported status of a TorClient.
#[derive(Serialize, Deserialize)]
struct ClientStatusInfo {
    /// True if the client is ready for traffic.
    ready: bool,
    /// Approximate estimate of how close the client is to being ready for traffic.
    fraction: f32,
    /// If present, a description of possible problem(s) that may be stopping
    /// the client from bootstrapping successfully.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    blocked: Option<String>,
}

impl From<crate::status::BootstrapStatus> for ClientStatusInfo {
    fn from(s: crate::status::BootstrapStatus) -> Self {
        let ready = s.ready_for_traffic();
        let fraction = s.as_frac();
        let blocked = s.blocked().map(|b| b.to_string());
        Self {
            ready,
            fraction,
            blocked,
        }
    }
}

/// Invocable function to run [`GetClientStatus`] on a [`TorClient`].
async fn get_client_status<R: Runtime>(
    client: Arc<TorClient<R>>,
    _method: Box<GetClientStatus>,
    _ctx: Box<dyn rpc::Context>,
) -> Result<ClientStatusInfo, rpc::RpcError> {
    Ok(client.bootstrap_status().into())
}

/// Invocable function to run [`WatchClientStatus`] on a [`TorClient`].
async fn watch_client_status<R: Runtime>(
    client: Arc<TorClient<R>>,
    _method: Box<WatchClientStatus>,
    _ctx: Box<dyn rpc::Context>,
    mut updates: rpc::UpdateSink<ClientStatusInfo>,
) -> Result<rpc::Nil, rpc::RpcError> {
    let mut events = client.bootstrap_events();

    // Send the _current_ status, no matter what.
    // (We do this after constructing er)
    updates.send(client.bootstrap_status().into()).await?;

    // Send additional updates whenever the status changes.
    while let Some(status) = events.next().await {
        updates.send(status.into()).await?;
    }

    // This can only happen if the client exits.
    Ok(rpc::NIL)
}
