#![allow(clippy::or_fun_call)]
use std::ffi::OsString;
use std::path::Path;
use std::str::FromStr;

use anyhow::anyhow;
use thiserror::Error;

use radicle::git::raw;
use radicle::identity::doc::{DocError, Id};
use radicle::identity::{doc, IdentityError};
use radicle::node;
use radicle::node::tracking::Scope;
use radicle::node::{Handle as _, Node};
use radicle::prelude::*;
use radicle::rad;
use radicle::storage;
use radicle::storage::git::Storage;

use crate::commands::rad_checkout as checkout;
use crate::commands::rad_fetch as fetch;
use crate::project;
use crate::terminal as term;
use crate::terminal::args::{Args, Error, Help};
use crate::terminal::Interactive;

pub const HELP: Help = Help {
    name: "clone",
    description: "Clone a project",
    version: env!("CARGO_PKG_VERSION"),
    usage: r#"
Usage

    rad clone <rid> [<option>...]

Options

    --no-announce   Do not announce our new refs to the network
    --no-confirm    Don't ask for confirmation during clone
    --help          Print help

"#,
};

#[derive(Debug)]
pub struct Options {
    id: Id,
    #[allow(dead_code)]
    interactive: Interactive,
    announce: bool,
}

impl Args for Options {
    fn from_args(args: Vec<OsString>) -> anyhow::Result<(Self, Vec<OsString>)> {
        use lexopt::prelude::*;

        let mut parser = lexopt::Parser::from_args(args);
        let mut id: Option<Id> = None;
        let mut interactive = Interactive::Yes;
        let mut announce = true;

        while let Some(arg) = parser.next()? {
            match arg {
                Long("no-confirm") => {
                    interactive = Interactive::No;
                }
                Long("no-announce") => {
                    announce = false;
                }
                Long("announce") => {
                    announce = true;
                }
                Long("help") => {
                    return Err(Error::Help.into());
                }
                Value(val) if id.is_none() => {
                    let val = val.to_string_lossy();
                    let val = val.strip_prefix("rad://").unwrap_or(&val);
                    let val = Id::from_str(val)?;

                    id = Some(val);
                }
                _ => return Err(anyhow!(arg.unexpected())),
            }
        }
        let id = id.ok_or_else(|| {
            anyhow!("to clone, a radicle id must be provided; see `rad clone --help`")
        })?;

        Ok((
            Options {
                id,
                interactive,
                announce,
            },
            vec![],
        ))
    }
}

pub fn run(options: Options, ctx: impl term::Context) -> anyhow::Result<()> {
    let profile = ctx.profile()?;
    let signer = term::signer(&profile)?;
    let mut node = radicle::Node::new(profile.socket());
    let (working, doc, proj) = clone(
        options.id,
        &signer,
        &profile.storage,
        &mut node,
        options.announce,
    )?;
    let delegates = doc
        .delegates
        .iter()
        .map(|d| **d)
        .filter(|id| id != profile.id())
        .collect::<Vec<_>>();
    let default_branch = proj.default_branch().clone();
    let path = working.workdir().unwrap(); // SAFETY: The working copy is not bare.

    // Setup tracking for project delegates.
    checkout::setup_remotes(
        project::SetupRemote {
            project: options.id,
            default_branch,
            repo: &working,
            fetch: true,
            tracking: true,
        },
        &delegates,
    )?;

    term::headline(format!(
        "🌱 Project successfully cloned under {}",
        term::format::highlight(Path::new(".").join(path).display())
    ));

    Ok(())
}

#[derive(Error, Debug)]
pub enum CloneError {
    #[error("node: {0}")]
    Node(#[from] node::Error),
    #[error("fork: {0}")]
    Fork(#[from] rad::ForkError),
    #[error("storage: {0}")]
    Storage(#[from] storage::Error),
    #[error("checkout: {0}")]
    Checkout(#[from] rad::CheckoutError),
    #[error("identity document error: {0}")]
    Doc(#[from] DocError),
    #[error("payload: {0}")]
    Payload(#[from] doc::PayloadError),
    #[error("project error: {0}")]
    Identity(#[from] IdentityError),
    #[error("repository {0} not found")]
    NotFound(Id),
    #[error("no seeds found for {0}")]
    NoSeeds(Id),
}

pub fn clone<G: Signer>(
    id: Id,
    signer: &G,
    storage: &Storage,
    node: &mut Node,
    announce: bool,
) -> Result<(raw::Repository, Doc<Verified>, Project), CloneError> {
    let me = *signer.public_key();

    // Track.
    if node.track_repo(id, Scope::default())? {
        term::success!(
            "Tracking relationship established for {}",
            term::format::tertiary(id)
        );
    }

    let results = fetch::fetch(id, node)?;
    let Ok(repository) = storage.repository(id) else {
        // If we don't have the project locally, even after attempting to fetch,
        // there's nothing we can do.
        if results.is_empty() {
            return Err(CloneError::NoSeeds(id));
        } else {
            return Err(CloneError::NotFound(id));
        }
    };

    // Create a local fork of the project, under our own id, unless we have one already.
    if repository.remote(signer.public_key()).is_err() {
        let mut spinner = term::spinner(format!(
            "Forking under {}..",
            term::format::tertiary(term::format::node(&me))
        ));
        rad::fork(id, signer, &storage)?;

        if announce {
            if let Err(e) = node.announce_refs(id) {
                spinner.message("Announcing fork..");
                spinner.error(e);
            } else {
                spinner.finish();
            }
        } else {
            spinner.finish();
        }
    }

    let doc = repository.identity_doc_of(&me)?;
    let proj = doc.project()?;
    let path = Path::new(proj.name());

    if results.success().next().is_none() {
        if results.failed().next().is_some() {
            term::warning("Fetching failed, local copy is potentially stale");
        } else {
            term::warning("No seeds found, local copy is potentially stale");
        }
    }

    // Checkout.
    let spinner = term::spinner(format!(
        "Creating checkout in ./{}..",
        term::format::tertiary(path.display())
    ));
    let repo = rad::checkout(id, &me, path, &storage)?;

    spinner.finish();

    Ok((repo, doc, proj))
}
