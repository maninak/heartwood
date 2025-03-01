use std::{fmt, time};

pub use radicle_term::format::*;
pub use radicle_term::{style, Paint};

use radicle::cob::{ObjectId, Timestamp};
use radicle::node::NodeId;
use radicle::profile::Profile;

use crate::terminal as term;

/// Format a node id to be more compact.
pub fn node(node: &NodeId) -> String {
    let node = node.to_human();
    let start = node.chars().take(7).collect::<String>();
    let end = node.chars().skip(node.len() - 7).collect::<String>();

    format!("{start}…{end}")
}

/// Format a git Oid.
pub fn oid(oid: impl Into<radicle::git::Oid>) -> String {
    format!("{:.7}", oid.into())
}

/// Wrap parenthesis around styled input, eg. `"input"` -> `"(input)"`.
pub fn parens<D: fmt::Display>(input: Paint<D>) -> Paint<String> {
    Paint::new(format!("({})", input.item)).with_style(input.style)
}

/// Format a COB id.
pub fn cob(id: &ObjectId) -> String {
    format!("{:.7}", id.to_string())
}

/// Format a timestamp.
pub fn timestamp(time: &Timestamp) -> String {
    let fmt = timeago::Formatter::new();
    let now = Timestamp::now();
    let duration = time::Duration::from_secs(now.as_secs() - time.as_secs());

    fmt.convert(duration)
}

/// Identity formatter that takes a profile and displays it as
/// `<node-id> (<username>)` depending on the configuration.
pub struct Identity<'a> {
    profile: &'a Profile,
    /// If true, node id is printed in its compact form.
    short: bool,
    /// If true, node id and username are printed using the terminal's
    /// styled formatters.
    styled: bool,
}

impl<'a> Identity<'a> {
    pub fn new(profile: &'a Profile) -> Self {
        Self {
            profile,
            short: false,
            styled: false,
        }
    }

    pub fn short(mut self) -> Self {
        self.short = true;
        self
    }

    pub fn styled(mut self) -> Self {
        self.styled = true;
        self
    }
}

impl<'a> fmt::Display for Identity<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let username = "(me)";
        let node_id = match self.short {
            true => self::node(self.profile.id()),
            false => self.profile.id().to_human(),
        };

        if self.styled {
            write!(
                f,
                "{} {}",
                term::format::highlight(node_id),
                term::format::dim(username)
            )
        } else {
            write!(f, "{node_id} {username}")
        }
    }
}
