//! Importers for other readers' data.

pub mod opml;
pub mod quiterss;

pub use opml::{export as export_opml, export_to_file as export_opml_to_file, looks_like_opml};
pub use quiterss::{import_quiterss, looks_like_quiterss};

use std::path::Path;

use crate::db::{Db, DbError};
use crate::models::ImportReport;

/// What a file turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    QuiteRssDatabase,
    Opml,
    Unknown,
}

/// Sniff by content, not by extension. People rename files, and QuiteRSS
/// databases are not always called `feeds.db`.
pub fn identify(path: impl AsRef<Path>) -> SourceKind {
    let path = path.as_ref();
    if looks_like_quiterss(path) {
        SourceKind::QuiteRssDatabase
    } else if looks_like_opml(path) {
        SourceKind::Opml
    } else {
        SourceKind::Unknown
    }
}

/// Import whatever the file happens to be.
pub fn import_any(
    db: &mut Db,
    path: impl AsRef<Path>,
) -> Result<(SourceKind, ImportReport), DbError> {
    let path = path.as_ref();
    match identify(path) {
        SourceKind::QuiteRssDatabase => {
            Ok((SourceKind::QuiteRssDatabase, import_quiterss(db, path)?))
        }
        SourceKind::Opml => {
            let r = opml::import_file(db, path).map_err(|e| match e {
                opml::OpmlError::Db(d) => d,
                other => DbError::Import(other.to_string()),
            })?;
            Ok((SourceKind::Opml, r))
        }
        SourceKind::Unknown if quiterss::looks_like_snaprss(path) => Err(DbError::Import(format!(
            "{} is a SnapRSS database, not something to import",
            path.display()
        ))),
        SourceKind::Unknown => Err(DbError::Import(format!(
            "{} is neither a QuiteRSS database nor an OPML file",
            path.display()
        ))),
    }
}
