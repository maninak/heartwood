mod features;
pub mod routing;
pub mod tracking;

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::ops::Deref;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::{fmt, io, net};

use amplify::WrapperMut;
use cyphernet::addr::{HostName, NetAddr};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json as json;

use crate::crypto::PublicKey;
use crate::identity::Id;
use crate::storage::RefUpdate;

pub use features::Features;

/// Default name for control socket file.
pub const DEFAULT_SOCKET_NAME: &str = "radicle.sock";
/// Default radicle protocol port.
pub const DEFAULT_PORT: u16 = 8776;
/// Filename of routing table database under the node directory.
pub const ROUTING_DB_FILE: &str = "routing.db";
/// Filename of address database under the node directory.
pub const ADDRESS_DB_FILE: &str = "addresses.db";
/// Filename of tracking table database under the node directory.
pub const TRACKING_DB_FILE: &str = "tracking.db";

/// Milliseconds since epoch.
pub type Timestamp = u64;

/// Result of a command, on the node control socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum CommandResult {
    /// Response on node socket indicating that a command was carried out successfully.
    #[serde(rename = "ok")]
    Okay {
        /// Whether the command had any effect.
        #[serde(default, skip_serializing_if = "crate::serde_ext::is_default")]
        updated: bool,
    },
    /// Response on node socket indicating that an error occured.
    Error {
        /// The reason for the error.
        reason: String,
    },
}

impl CommandResult {
    /// Create an "updated" response.
    pub fn updated() -> Self {
        Self::Okay { updated: true }
    }

    /// Create an "ok" response.
    pub fn ok() -> Self {
        Self::Okay { updated: false }
    }

    /// Create an error result.
    pub fn error(err: impl std::error::Error) -> Self {
        Self::Error {
            reason: err.to_string(),
        }
    }

    /// Write this command result to a stream, including a terminating LF character.
    pub fn to_writer(&self, mut w: impl io::Write) -> io::Result<()> {
        json::to_writer(&mut w, self).map_err(|_| io::ErrorKind::InvalidInput)?;
        w.write_all(b"\n")
    }
}

impl From<CommandResult> for Result<bool, Error> {
    fn from(value: CommandResult) -> Self {
        match value {
            CommandResult::Okay { updated } => Ok(updated),
            CommandResult::Error { reason } => Err(Error::Node(reason)),
        }
    }
}

/// Peer public protocol address.
#[derive(Wrapper, WrapperMut, Clone, Eq, PartialEq, Debug, From)]
#[wrapper(Deref, Display, FromStr)]
#[wrapper_mut(DerefMut)]
pub struct Address(NetAddr<HostName>);

impl cyphernet::addr::Host for Address {
    fn requires_proxy(&self) -> bool {
        self.0.requires_proxy()
    }
}

impl cyphernet::addr::Addr for Address {
    fn port(&self) -> u16 {
        self.0.port()
    }
}

impl From<net::SocketAddr> for Address {
    fn from(addr: net::SocketAddr) -> Self {
        Address(NetAddr {
            host: HostName::Ip(addr.ip()),
            port: addr.port(),
        })
    }
}

/// Command name.
#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommandName {
    /// Announce repository references for given repository to peers.
    AnnounceRefs,
    /// Announce local repositories to peers.
    AnnounceInventory,
    /// Sync local inventory with node.
    SyncInventory,
    /// Connect to node with the given address.
    Connect,
    /// Lookup seeds for the given repository in the routing table.
    Seeds,
    /// Fetch the given repository from the network.
    Fetch,
    /// Track the given repository.
    TrackRepo,
    /// Untrack the given repository.
    UntrackRepo,
    /// Track the given node.
    TrackNode,
    /// Untrack the given node.
    UntrackNode,
    /// Get the node's status.
    Status,
    /// Shutdown the node.
    Shutdown,
}

impl fmt::Display for CommandName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // SAFETY: The enum can always be converted to a value.
        #[allow(clippy::unwrap_used)]
        let val = json::to_value(self).unwrap();
        // SAFETY: The value is always a string.
        #[allow(clippy::unwrap_used)]
        let s = val.as_str().unwrap();

        write!(f, "{s}")
    }
}

/// Commands sent to the node via the control socket.
#[derive(Debug, Serialize, Deserialize)]
pub struct Command {
    /// Command name.
    #[serde(rename = "cmd")]
    pub name: CommandName,
    /// Command arguments.
    #[serde(rename = "args")]
    pub args: Vec<String>,
}

impl Command {
    /// Shutdown command.
    pub const SHUTDOWN: Self = Self {
        name: CommandName::Shutdown,
        args: vec![],
    };

    /// Create a new command.
    pub fn new<T: ToString>(name: CommandName, args: impl IntoIterator<Item = T>) -> Self {
        Self {
            name,
            args: args.into_iter().map(|a| a.to_string()).collect(),
        }
    }

    /// Write this command to a stream, including a terminating LF character.
    pub fn to_writer(&self, mut w: impl io::Write) -> io::Result<()> {
        json::to_writer(&mut w, self).map_err(|_| io::ErrorKind::InvalidInput)?;
        w.write_all(b"\n")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[serde(tag = "state", content = "id")]
pub enum Seed {
    Disconnected(NodeId),
    Fetching(NodeId),
    Connected(NodeId),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Seeds(BTreeSet<Seed>);

impl Seeds {
    pub fn insert(&mut self, seed: Seed) {
        self.0.insert(seed);
    }

    pub fn connected(&mut self) -> impl Iterator<Item = &NodeId> {
        self.0.iter().filter_map(|s| match s {
            Seed::Connected(node) => Some(node),
            Seed::Fetching(_) | Seed::Disconnected(_) => None,
        })
    }

    pub fn disconnected(&mut self) -> impl Iterator<Item = &NodeId> {
        self.0.iter().filter_map(|s| match s {
            Seed::Disconnected(node) => Some(node),
            Seed::Fetching(_) | Seed::Connected(_) => None,
        })
    }

    pub fn fetching(&mut self) -> impl Iterator<Item = &NodeId> {
        self.0.iter().filter_map(|s| match s {
            Seed::Fetching(node) => Some(node),
            Seed::Connected(_) | Seed::Disconnected(_) => None,
        })
    }

    pub fn has_connections(&self) -> bool {
        self.0.iter().any(|s| match s {
            Seed::Connected(_) => true,
            Seed::Disconnected(_) | Seed::Fetching(_) => false,
        })
    }

    pub fn is_connected(&self, node: &NodeId) -> bool {
        self.0.contains(&Seed::Connected(*node))
    }

    pub fn is_disconnected(&self, node: &NodeId) -> bool {
        self.0.contains(&Seed::Disconnected(*node))
    }

    pub fn is_fetching(&self, node: &NodeId) -> bool {
        self.0.contains(&Seed::Fetching(*node))
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum FetchResult {
    Success { updated: Vec<RefUpdate> },
    Failed { reason: String },
}

impl FetchResult {
    pub fn is_success(&self) -> bool {
        matches!(self, FetchResult::Success { .. })
    }

    pub fn success(self) -> Option<Vec<RefUpdate>> {
        match self {
            Self::Success { updated } => Some(updated),
            _ => None,
        }
    }
}

impl<S: ToString> From<Result<Vec<RefUpdate>, S>> for FetchResult {
    fn from(value: Result<Vec<RefUpdate>, S>) -> Self {
        match value {
            Ok(updated) => Self::Success { updated },
            Err(err) => Self::Failed {
                reason: err.to_string(),
            },
        }
    }
}

/// Holds multiple fetch results.
#[derive(Debug, Default)]
pub struct FetchResults(Vec<(NodeId, FetchResult)>);

impl FetchResults {
    /// Push a fetch result.
    pub fn push(&mut self, nid: NodeId, result: FetchResult) {
        self.0.push((nid, result));
    }

    /// Iterate over all fetch results.
    pub fn iter(&self) -> impl Iterator<Item = (&NodeId, &FetchResult)> {
        self.0.iter().map(|(nid, r)| (nid, r))
    }

    /// Iterate over successful fetches.
    pub fn success(&self) -> impl Iterator<Item = (&NodeId, &[RefUpdate])> {
        self.0.iter().filter_map(|(nid, r)| {
            if let FetchResult::Success { updated } = r {
                Some((nid, updated.as_slice()))
            } else {
                None
            }
        })
    }

    /// Iterate over failed fetches.
    pub fn failed(&self) -> impl Iterator<Item = (&NodeId, &str)> {
        self.0.iter().filter_map(|(nid, r)| {
            if let FetchResult::Failed { reason } = r {
                Some((nid, reason.as_str()))
            } else {
                None
            }
        })
    }
}

impl From<Vec<(NodeId, FetchResult)>> for FetchResults {
    fn from(value: Vec<(NodeId, FetchResult)>) -> Self {
        Self(value)
    }
}

impl Deref for FetchResults {
    type Target = [(NodeId, FetchResult)];

    fn deref(&self) -> &Self::Target {
        self.0.as_slice()
    }
}

impl IntoIterator for FetchResults {
    type Item = (NodeId, FetchResult);
    type IntoIter = std::vec::IntoIter<(NodeId, FetchResult)>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// Error returned by [`Handle`] functions.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to connect to node: {0}")]
    Connect(#[from] io::Error),
    #[error("failed to call node: {0}")]
    Call(#[from] CallError),
    #[error("node: {0}")]
    Node(String),
    #[error("received empty response for `{cmd}` command")]
    EmptyResponse { cmd: CommandName },
}

impl Error {
    /// Check if the error is due to the not being able to connect to the local node.
    pub fn is_connection_err(&self) -> bool {
        matches!(self, Self::Connect(_))
    }
}

/// Error returned by [`Node::call`] iterator.
#[derive(thiserror::Error, Debug)]
pub enum CallError {
    #[error("i/o: {0}")]
    Io(#[from] io::Error),
    #[error("received invalid json in response for `{cmd}` command: '{response}': {error}")]
    InvalidJson {
        cmd: CommandName,
        response: String,
        error: json::Error,
    },
}

/// A handle to send commands to the node or request information.
pub trait Handle {
    /// The peer sessions type.
    type Sessions;
    /// The error returned by all methods.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Check if the node is running. to a peer.
    fn is_running(&self) -> bool;
    /// Connect to a peer.
    fn connect(&mut self, node: NodeId, addr: Address) -> Result<(), Self::Error>;
    /// Lookup the seeds of a given repository in the routing table.
    fn seeds(&mut self, id: Id) -> Result<Seeds, Self::Error>;
    /// Fetch a repository from the network.
    fn fetch(&mut self, id: Id, from: NodeId) -> Result<FetchResult, Self::Error>;
    /// Start tracking the given project. Doesn't do anything if the project is already
    /// tracked.
    fn track_repo(&mut self, id: Id, scope: tracking::Scope) -> Result<bool, Self::Error>;
    /// Start tracking the given node.
    fn track_node(&mut self, id: NodeId, alias: Option<String>) -> Result<bool, Self::Error>;
    /// Untrack the given project and delete it from storage.
    fn untrack_repo(&mut self, id: Id) -> Result<bool, Self::Error>;
    /// Untrack the given node.
    fn untrack_node(&mut self, id: NodeId) -> Result<bool, Self::Error>;
    /// Notify the service that a project has been updated, and announce local refs.
    fn announce_refs(&mut self, id: Id) -> Result<(), Self::Error>;
    /// Announce local inventory.
    fn announce_inventory(&mut self) -> Result<(), Self::Error>;
    /// Notify the service that our inventory was updated.
    fn sync_inventory(&mut self) -> Result<bool, Self::Error>;
    /// Ask the service to shutdown.
    fn shutdown(self) -> Result<(), Self::Error>;
    /// Query the peer session state.
    fn sessions(&self) -> Result<Self::Sessions, Self::Error>;
}

/// Public node & device identifier.
pub type NodeId = PublicKey;

/// Node controller.
#[derive(Debug)]
pub struct Node {
    socket: PathBuf,
}

impl Node {
    /// Connect to the node, via the socket at the given path.
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        Self {
            socket: path.as_ref().to_path_buf(),
        }
    }

    /// Call a command on the node.
    pub fn call<A: ToString, T: DeserializeOwned>(
        &self,
        name: CommandName,
        args: impl IntoIterator<Item = A>,
    ) -> Result<impl Iterator<Item = Result<T, CallError>>, io::Error> {
        let stream = UnixStream::connect(&self.socket)?;
        Command::new(name, args).to_writer(&stream)?;

        Ok(BufReader::new(stream).lines().map(move |l| {
            let l = l?;
            let v = json::from_str(&l).map_err(|e| CallError::InvalidJson {
                cmd: name,
                response: l,
                error: e,
            })?;

            Ok(v)
        }))
    }
}

// TODO(finto): repo_policies, node_policies, and routing should all
// attempt to return iterators instead of allocating vecs.
impl Handle for Node {
    type Sessions = ();
    type Error = Error;

    fn is_running(&self) -> bool {
        let Ok(mut lines) = self.call::<&str, CommandResult>(CommandName::Status, []) else {
            return false;
        };
        let Some(Ok(result)) = lines.next() else {
            return false;
        };
        matches!(result, CommandResult::Okay { .. })
    }

    fn connect(&mut self, nid: NodeId, addr: Address) -> Result<(), Error> {
        self.call::<_, CommandResult>(CommandName::Connect, [nid.to_human(), addr.to_string()])?
            .next()
            .ok_or(Error::EmptyResponse {
                cmd: CommandName::Connect,
            })??;
        Ok(())
    }

    fn seeds(&mut self, id: Id) -> Result<Seeds, Error> {
        let seeds: Seeds =
            self.call(CommandName::Seeds, [id.urn()])?
                .next()
                .ok_or(Error::EmptyResponse {
                    cmd: CommandName::Seeds,
                })??;

        Ok(seeds)
    }

    fn fetch(&mut self, id: Id, from: NodeId) -> Result<FetchResult, Error> {
        let result = self
            .call(CommandName::Fetch, [id.urn(), from.to_human()])?
            .next()
            .ok_or(Error::EmptyResponse {
                cmd: CommandName::Fetch,
            })??;

        Ok(result)
    }

    fn track_node(&mut self, id: NodeId, alias: Option<String>) -> Result<bool, Error> {
        let id = id.to_human();
        let args = if let Some(alias) = alias.as_deref() {
            vec![id.as_str(), alias]
        } else {
            vec![id.as_str()]
        };

        let mut line = self.call(CommandName::TrackNode, args)?;
        let response: CommandResult = line.next().ok_or(Error::EmptyResponse {
            cmd: CommandName::TrackNode,
        })??;

        response.into()
    }

    fn track_repo(&mut self, id: Id, scope: tracking::Scope) -> Result<bool, Error> {
        let mut line = self.call(CommandName::TrackRepo, [id.urn(), scope.to_string()])?;
        let response: CommandResult = line.next().ok_or(Error::EmptyResponse {
            cmd: CommandName::TrackRepo,
        })??;

        response.into()
    }

    fn untrack_node(&mut self, id: NodeId) -> Result<bool, Error> {
        let mut line = self.call(CommandName::UntrackNode, [id])?;
        let response: CommandResult = line.next().ok_or(Error::EmptyResponse {
            cmd: CommandName::UntrackNode,
        })??;

        response.into()
    }

    fn untrack_repo(&mut self, id: Id) -> Result<bool, Error> {
        let mut line = self.call(CommandName::UntrackRepo, [id.urn()])?;
        let response: CommandResult = line.next().ok_or(Error::EmptyResponse {
            cmd: CommandName::UntrackRepo,
        })??;

        response.into()
    }

    fn announce_refs(&mut self, id: Id) -> Result<(), Error> {
        for line in self.call::<_, CommandResult>(CommandName::AnnounceRefs, [id.urn()])? {
            line?;
        }
        Ok(())
    }

    fn announce_inventory(&mut self) -> Result<(), Error> {
        for line in self.call::<&str, CommandResult>(CommandName::AnnounceInventory, [])? {
            line?;
        }
        Ok(())
    }

    fn sync_inventory(&mut self) -> Result<bool, Error> {
        let mut line = self.call::<&str, _>(CommandName::SyncInventory, [])?;
        let response: CommandResult = line.next().ok_or(Error::EmptyResponse {
            cmd: CommandName::SyncInventory,
        })??;

        response.into()
    }

    fn sessions(&self) -> Result<Self::Sessions, Error> {
        todo!();
    }

    fn shutdown(self) -> Result<(), Error> {
        todo!();
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_command_name_display() {
        assert_eq!(CommandName::TrackNode.to_string(), "track-node");
    }
}
