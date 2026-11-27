//! Recovery for a transfer whose apply report never arrived.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::Path;

use agent_core::sync::plan::{self, Action, BaseRecord, Stamp};
use serde::{Deserialize, Serialize};

use super::{Base, Listing, write_json_atomic};

#[derive(Clone, Copy, Deserialize, Serialize)]
pub(super) enum Destination {
    Host,
    Guest,
}

#[derive(Deserialize, Serialize)]
struct PendingTransfer {
    destination: Destination,
    source: Option<Stamp>,
    prior_destination: Option<Stamp>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(transparent)]
pub(super) struct Pending(BTreeMap<String, PendingTransfer>);

impl Pending {
    pub(super) fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("Could not read the pending sync transfers: {error}")),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!(
                "Could not read the pending sync transfers: {error}"
            )),
        }
    }

    pub(super) fn save(&self, path: &Path) -> Result<(), String> {
        if self.is_empty() {
            match std::fs::remove_file(path) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
                Err(error) => {
                    return Err(format!(
                        "Could not clear the pending sync transfers: {error}"
                    ));
                }
            }
        }
        write_json_atomic(path, self)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn prepare<'a>(
        &mut self,
        destination: Destination,
        source: &Listing,
        prior_destination: &Listing,
        paths: impl Iterator<Item = &'a String>,
    ) {
        for path in paths {
            self.0.insert(
                path.clone(),
                PendingTransfer {
                    destination,
                    source: source.get(path).map(Stamp::from),
                    prior_destination: prior_destination.get(path).map(Stamp::from),
                },
            );
        }
    }

    pub(super) fn settle<'a>(&mut self, paths: impl Iterator<Item = &'a String>) {
        for path in paths {
            self.0.remove(path);
        }
    }

    pub(super) fn observe(&mut self, base: &mut Base, host: &Listing, guest: &Listing) {
        self.0.retain(|path, transfer| {
            let ours = host.get(path);
            let theirs = guest.get(path);
            if matches!((ours, theirs), (None, None))
                || ours
                    .zip(theirs)
                    .is_some_and(|(ours, theirs)| plan::same(ours, theirs))
            {
                return false;
            }
            let actual = match transfer.destination {
                Destination::Host => ours,
                Destination::Guest => theirs,
            };
            if plan::changed(transfer.source.as_ref(), actual) {
                return plan::changed(transfer.prior_destination.as_ref(), actual);
            }
            match &transfer.source {
                Some(source) => {
                    base.insert(
                        path.clone(),
                        BaseRecord {
                            mac: source.clone(),
                            vm: source.clone(),
                        },
                    );
                }
                None => {
                    base.remove(path);
                }
            }
            false
        });
    }

    pub(super) fn protect(&self, actions: &mut Vec<Action>) {
        actions.retain(|action| !self.0.contains_key(action.path()));
        actions.extend(self.0.keys().cloned().map(Action::Conflict));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::sync::{Entry, EntryKind, SyncMode};

    fn file(byte: u8) -> Listing {
        BTreeMap::from([(
            "file".into(),
            Entry {
                path: "file".into(),
                kind: EntryKind::File,
                size: 1,
                mtime_ns: 0,
                mode: 0o644,
                target: None,
                digest: Some([byte; 32]),
            },
        )])
    }

    #[test]
    fn published_transfer_survives_lost_acknowledgment_and_source_revert() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pending.json");
        let old = file(1);
        let new = file(2);
        let mut pending = Pending::default();
        pending.prepare(Destination::Guest, &new, &old, new.keys());
        pending.save(&path).unwrap();
        let mut pending = Pending::load(&path).unwrap();
        let mut base = plan::next_base(&Base::new(), &old, &old, &[]);
        pending.observe(&mut base, &old, &new);
        assert!(pending.is_empty());
        assert_eq!(
            plan::reconcile(SyncMode::TwoWay, &base, &old, &new),
            [Action::Push("file".into())]
        );
    }

    #[test]
    fn uncertain_destination_changes_are_kept_as_conflicts() {
        let old = file(1);
        let wanted = file(2);
        let different = file(3);
        let mut pending = Pending::default();
        pending.prepare(Destination::Guest, &wanted, &old, wanted.keys());
        let mut base = plan::next_base(&Base::new(), &old, &old, &[]);
        pending.observe(&mut base, &wanted, &different);
        let mut actions = plan::reconcile(SyncMode::Mirror, &base, &wanted, &different);
        pending.protect(&mut actions);
        assert_eq!(actions, [Action::Conflict("file".into())]);
    }
}
