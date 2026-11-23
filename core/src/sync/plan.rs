//! Deciding, per path, what a sync cycle does in each mode.
//!
//! Each side is compared with the base: the state both sides had right
//! after the last time this path was in sync. A side whose stamp differs
//! from its base has changed since. Across sides, files match when kind,
//! size and whole seconds of mtime agree (transfers carry whole seconds);
//! within one side the full nanosecond stamp is compared.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::SyncMode;
use super::scan::{Entry, EntryKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stamp {
    pub kind: EntryKind,
    pub size: u64,
    pub mtime_ns: i64,
    pub target: Option<String>,
}

impl From<&Entry> for Stamp {
    fn from(entry: &Entry) -> Self {
        Self { kind: entry.kind, size: entry.size, mtime_ns: entry.mtime_ns, target: entry.target.clone() }
    }
}

impl Stamp {
    fn matches(&self, entry: &Entry) -> bool {
        self.kind == entry.kind && self.size == entry.size && self.mtime_ns == entry.mtime_ns && self.target == entry.target
    }
}

/// Both sides' stamps when the path was last in sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseRecord {
    pub mac: Stamp,
    pub vm: Stamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Copy the Mac's version into the machine.
    Push(String),
    /// Copy the machine's version to the Mac (two-way only).
    Pull(String),
    DeleteVm(String),
    DeleteMac(String),
    /// Changed on both sides; nothing is touched until it is resolved.
    Conflict(String),
}

impl Action {
    pub fn path(&self) -> &str {
        match self {
            Self::Push(p) | Self::Pull(p) | Self::DeleteVm(p) | Self::DeleteMac(p) | Self::Conflict(p) => p,
        }
    }
}

pub fn changed(base: Option<&Stamp>, now: Option<&Entry>) -> bool {
    match (base, now) {
        (None, None) => false,
        (Some(base), Some(now)) => {
            base.kind != now.kind
                || match now.kind {
                    EntryKind::Dir => false,
                    EntryKind::Symlink => base.target != now.target,
                    EntryKind::File => base.size != now.size || base.mtime_ns != now.mtime_ns,
                }
        }
        _ => true,
    }
}

/// Whether the Mac side differs from the base anywhere. Safe mode only has work
/// to do for Mac changes, so it can skip the remote scan when this is false.
pub fn mac_changed(base: &BTreeMap<String, BaseRecord>, mac: &BTreeMap<String, Entry>) -> bool {
    base.keys().chain(mac.keys()).any(|path| {
        changed(
            base.get(path).map(|record| &record.mac),
            mac.get(path),
        )
    })
}

/// The base already describes both current sides exactly. A cycle with no
/// actions can then skip serializing and rewriting the whole base.
pub fn base_is_current(
    base: &BTreeMap<String, BaseRecord>,
    mac: &BTreeMap<String, Entry>,
    vm: &BTreeMap<String, Entry>,
) -> bool {
    base.len() == mac.len()
        && base.len() == vm.len()
        && base.iter().zip(mac).zip(vm).all(|(((path, record), (mac_path, mac)), (vm_path, vm))| {
            path == mac_path && path == vm_path
                && same(mac, vm) && record.mac.matches(mac) && record.vm.matches(vm)
        })
}

/// The two sides hold the same thing at this path.
pub fn same(a: &Entry, b: &Entry) -> bool {
    a.kind == b.kind
        && match a.kind {
            EntryKind::Dir => true,
            EntryKind::Symlink => a.target == b.target,
            EntryKind::File => a.size == b.size && a.mtime_ns.div_euclid(1_000_000_000) == b.mtime_ns.div_euclid(1_000_000_000),
        }
}

/// Visit the sorted union without allocating a separate set of paths.
fn paths<'a>(
    base: &'a BTreeMap<String, BaseRecord>,
    mac: &'a BTreeMap<String, Entry>,
    vm: &'a BTreeMap<String, Entry>,
) -> impl Iterator<Item = (&'a String, Option<&'a BaseRecord>, Option<&'a Entry>, Option<&'a Entry>)> {
    let (mut bases, mut macs, mut vms) = (base.iter().peekable(), mac.iter().peekable(), vm.iter().peekable());
    std::iter::from_fn(move || {
        let path = [bases.peek().map(|(p, _)| *p), macs.peek().map(|(p, _)| *p), vms.peek().map(|(p, _)| *p)]
            .into_iter().flatten().min()?;
        let b = if bases.peek().is_some_and(|(p, _)| *p == path) { bases.next().map(|(_, v)| v) } else { None };
        let m = if macs.peek().is_some_and(|(p, _)| *p == path) { macs.next().map(|(_, v)| v) } else { None };
        let v = if vms.peek().is_some_and(|(p, _)| *p == path) { vms.next().map(|(_, v)| v) } else { None };
        Some((path, b, m, v))
    })
}

/// What to do so the two sides agree as `mode` wants. Deletions come
/// deepest first, everything else in path order (parents before children).
pub fn reconcile(
    mode: SyncMode,
    base: &BTreeMap<String, BaseRecord>,
    mac: &BTreeMap<String, Entry>,
    vm: &BTreeMap<String, Entry>,
) -> Vec<Action> {
    let (mut deletes, mut rest) = (Vec::new(), Vec::new());
    for (path, b, m, v) in paths(base, mac, vm) {
        let in_sync = match (m, v) {
            (Some(m), Some(v)) => same(m, v),
            (None, None) => true,
            _ => false,
        };
        if in_sync {
            continue;
        }
        let mac_changed = changed(b.map(|b| &b.mac), m);
        let vm_changed = changed(b.map(|b| &b.vm), v);
        let toward_vm = || if m.is_some() { Action::Push(path.clone()) } else { Action::DeleteVm(path.clone()) };
        let toward_mac = || if v.is_some() { Action::Pull(path.clone()) } else { Action::DeleteMac(path.clone()) };
        let action = match mode {
            SyncMode::Mirror => Some(toward_vm()),
            SyncMode::Safe => match (mac_changed, vm_changed) {
                (true, false) => Some(toward_vm()),
                (true, true) => Some(Action::Conflict(path.clone())),
                // The machine's own edits and new files are kept.
                (false, _) => None,
            },
            SyncMode::TwoWay => match (mac_changed, vm_changed) {
                (true, false) => Some(toward_vm()),
                (false, true) => Some(toward_mac()),
                _ => Some(Action::Conflict(path.clone())),
            },
        };
        if let Some(action) = action {
            if matches!(action, Action::DeleteVm(_) | Action::DeleteMac(_)) {
                deletes.push(action);
            } else {
                rest.push(action);
            }
        }
    }
    // Paths were visited in order; reverse deletes so children come first.
    deletes.reverse();
    deletes.extend(rest);
    deletes
}

/// A stamp as it lands on the other side: a transfer keeps whole seconds.
fn landed_stamp(entry: &Entry) -> Stamp {
    let mut stamp = Stamp::from(entry);
    stamp.mtime_ns = entry.mtime_ns.div_euclid(1_000_000_000) * 1_000_000_000;
    stamp
}

/// The base after `applied` succeeded (failed actions are left out by the
/// caller). Paths in sync get both sides' stamps; conflicts and one-sided
/// paths keep their old record, so a kept edit or deletion stays kept.
pub fn next_base(
    base: &BTreeMap<String, BaseRecord>,
    mac: &BTreeMap<String, Entry>,
    vm: &BTreeMap<String, Entry>,
    applied: &[Action],
) -> BTreeMap<String, BaseRecord> {
    let by_path: BTreeMap<&str, &Action> = applied.iter().map(|a| (a.path(), a)).collect();
    let mut next = BTreeMap::new();
    for (path, old, mut m, mut v) in paths(base, mac, vm) {
        let mut mac_landed = false;
        let mut vm_landed = false;
        match by_path.get(path.as_str()) {
            Some(Action::Push(_)) => { v = m; vm_landed = true; }
            Some(Action::Pull(_)) => { m = v; mac_landed = true; }
            Some(Action::DeleteVm(_)) => v = None,
            Some(Action::DeleteMac(_)) => m = None,
            Some(Action::Conflict(_)) | None => {}
        }
        match (m, v) {
            (Some(m), Some(v)) if same(m, v) => {
                next.insert(path.clone(), BaseRecord {
                    mac: if mac_landed { landed_stamp(m) } else { m.into() },
                    vm: if vm_landed { landed_stamp(v) } else { v.into() },
                });
            }
            (None, None) => {}
            _ => {
                if let Some(old) = old {
                    next.insert(path.clone(), old.clone());
                }
            }
        }
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: u64, secs: i64) -> Entry {
        Entry { path: path.into(), kind: EntryKind::File, size, mtime_ns: secs * 1_000_000_000 + 123, mode: 0o644, target: None }
    }

    fn landed(entry: &Entry) -> Entry {
        let mut copy = entry.clone();
        copy.mtime_ns = entry.mtime_ns.div_euclid(1_000_000_000) * 1_000_000_000;
        copy
    }

    fn map(entries: &[Entry]) -> BTreeMap<String, Entry> {
        entries.iter().map(|e| (e.path.clone(), e.clone())).collect()
    }

    /// A base where both sides held `entries` (as after a sync).
    fn synced(entries: &[Entry]) -> BTreeMap<String, BaseRecord> {
        entries.iter().map(|e| (e.path.clone(), BaseRecord { mac: e.into(), vm: (&landed(e)).into() })).collect()
    }

    #[test]
    fn first_sync_pushes_everything_in_every_mode() {
        let mac = map(&[file("a", 1, 10), file("b", 2, 10)]);
        for mode in [SyncMode::Mirror, SyncMode::Safe, SyncMode::TwoWay] {
            let plan = reconcile(mode, &BTreeMap::new(), &mac, &BTreeMap::new());
            assert_eq!(plan, [Action::Push("a".into()), Action::Push("b".into())], "{mode:?}");
        }
    }

    #[test]
    fn a_current_base_skips_its_rewrite_and_stale_stamps_do_not() {
        let a = file("a", 1, 10);
        let base = synced(std::slice::from_ref(&a));
        let mac = map(std::slice::from_ref(&a));
        let vm = map(&[landed(&a)]);
        assert!(base_is_current(&base, &mac, &vm));
        assert!(!base_is_current(&BTreeMap::new(), &mac, &vm));
        assert!(!base_is_current(&base, &map(&[file("a", 2, 10)]), &vm));
        assert!(!base_is_current(&base, &map(&[file("b", 1, 10)]), &vm));
        assert!(!base_is_current(&base, &mac, &map(&[file("A", 1, 10)])));
    }

    #[test]
    fn mac_changes_are_detected_against_the_base() {
        let a = file("a", 1, 10);
        let base = synced(std::slice::from_ref(&a));
        assert!(!mac_changed(&base, &map(std::slice::from_ref(&a))));
        assert!(mac_changed(&base, &map(&[file("a", 2, 10)])));
        assert!(mac_changed(&base, &map(&[])));
        assert!(mac_changed(&base, &map(&[a, file("new", 1, 20)])));
    }

    #[test]
    fn identical_files_need_nothing_even_with_different_nanoseconds() {
        let mac = map(&[file("a", 1, 10)]);
        let vm = map(&[landed(&file("a", 1, 10))]);
        assert!(reconcile(SyncMode::TwoWay, &BTreeMap::new(), &mac, &vm).is_empty());
        let base = next_base(&BTreeMap::new(), &mac, &vm, &[]);
        assert!(base.contains_key("a"));
    }

    #[test]
    fn mirror_replaces_machine_edits_and_removes_machine_only_files() {
        let a = file("a", 1, 10);
        let base = synced(std::slice::from_ref(&a));
        let mac = map(std::slice::from_ref(&a));
        let vm = map(&[file("a", 9, 20), file("extra", 1, 20)]);
        assert_eq!(
            reconcile(SyncMode::Mirror, &base, &mac, &vm),
            [Action::DeleteVm("extra".into()), Action::Push("a".into())]
        );
    }

    #[test]
    fn safe_keeps_machine_edits_new_files_and_deletions() {
        let (a, b, c) = (file("a", 1, 10), file("b", 1, 10), file("c", 1, 10));
        let base = synced(&[a.clone(), b.clone(), c.clone()]);
        // Mac edits a; the machine edits b, deletes c and adds d.
        let mac = map(&[file("a", 2, 11), b.clone(), c.clone()]);
        let vm = map(&[landed(&a), file("b", 5, 12), file("d", 1, 12)]);
        assert_eq!(reconcile(SyncMode::Safe, &base, &mac, &vm), [Action::Push("a".into())]);
        // The machine's deletion of c survives the next cycle too.
        let next = next_base(&base, &mac, &vm, &[Action::Push("a".into())]);
        assert!(reconcile(SyncMode::Safe, &next, &mac, &vm).is_empty());
    }

    #[test]
    fn safe_and_two_way_report_edits_on_both_sides_as_conflicts() {
        let a = file("a", 1, 10);
        let base = synced(std::slice::from_ref(&a));
        let mac = map(&[file("a", 2, 11)]);
        let vm = map(&[file("a", 3, 12)]);
        for mode in [SyncMode::Safe, SyncMode::TwoWay] {
            assert_eq!(reconcile(mode, &base, &mac, &vm), [Action::Conflict("a".into())]);
        }
        // A conflict keeps its old base, so it stays a conflict until resolved.
        let next = next_base(&base, &mac, &vm, &[Action::Conflict("a".into())]);
        assert_eq!(next.get("a"), base.get("a"));
    }

    #[test]
    fn two_way_brings_machine_edits_and_deletions_back() {
        let (a, b) = (file("a", 1, 10), file("b", 1, 10));
        let base = synced(&[a.clone(), b.clone()]);
        let mac = map(&[a.clone(), b.clone()]);
        let vm = map(&[file("a", 7, 20), file("new", 1, 20)]);
        assert_eq!(
            reconcile(SyncMode::TwoWay, &base, &mac, &vm),
            [Action::DeleteMac("b".into()), Action::Pull("a".into()), Action::Pull("new".into())]
        );
        let applied = reconcile(SyncMode::TwoWay, &base, &mac, &vm);
        let next = next_base(&base, &mac, &vm, &applied);
        assert!(!next.contains_key("b"));
        assert_eq!(next["a"].vm.size, 7);
        assert_eq!(next["a"].mac.mtime_ns, 20 * 1_000_000_000);
    }

    #[test]
    fn deletions_run_deepest_first() {
        let dir = Entry { path: "d".into(), kind: EntryKind::Dir, size: 0, mtime_ns: 0, mode: 0o755, target: None };
        let inner = file("d/x", 1, 10);
        let base = synced(&[dir.clone(), inner.clone()]);
        let vm = map(&[dir, landed(&inner)]);
        assert_eq!(
            reconcile(SyncMode::Mirror, &base, &BTreeMap::new(), &vm),
            [Action::DeleteVm("d/x".into()), Action::DeleteVm("d".into())]
        );
    }

    #[test]
    fn merged_paths_cover_each_map_once_in_order() {
        let base = synced(&[file("a", 1, 1), file("c", 1, 1), file("e", 1, 1)]);
        let mac = map(&[file("b", 1, 1), file("c", 1, 1), file("e", 1, 1)]);
        let vm = map(&[file("c", 1, 1), file("d", 1, 1), file("e", 1, 1)]);
        let found: Vec<_> = paths(&base, &mac, &vm)
            .map(|(path, b, m, v)| (path.as_str(), b.is_some(), m.is_some(), v.is_some()))
            .collect();
        assert_eq!(found, [
            ("a", true, false, false),
            ("b", false, true, false),
            ("c", true, true, true),
            ("d", false, false, true),
            ("e", true, true, true),
        ]);
    }

    #[test]
    fn next_base_preserves_transfer_and_action_semantics() {
        let original = file("conflict", 1, 1);
        let mut link = file("link", 0, -2);
        link.kind = EntryKind::Symlink;
        link.target = Some("some/target".into());
        let mut before_epoch = file("push", 3, -1);
        before_epoch.mtime_ns = -1;
        let mac = map(&[before_epoch.clone(), file("conflict", 2, 2), file("delete", 1, 1)]);
        let vm = map(&[link.clone(), file("conflict", 3, 3), file("delete", 1, 1), file("missing", 1, 1)]);
        let base = synced(&[original, file("delete", 1, 1), file("missing", 1, 1)]);
        // Actions need not be sorted; when paths repeat, the last action wins.
        let actions = [Action::Push("link".into()), Action::DeleteMac("delete".into()),
            Action::Push("push".into()), Action::Conflict("conflict".into()),
            Action::Pull("link".into()), Action::Push("missing".into())];
        let next = next_base(&base, &mac, &vm, &actions);
        assert_eq!(next["push"], BaseRecord { mac: (&before_epoch).into(), vm: landed_stamp(&before_epoch) });
        assert_eq!(next["push"].vm.mtime_ns, -1_000_000_000);
        assert_eq!(next["link"], BaseRecord { mac: landed_stamp(&link), vm: (&link).into() });
        assert_eq!(next["conflict"], base["conflict"]);
        assert_eq!(next["delete"], base["delete"]);
        assert!(!next.contains_key("missing"));
        assert_eq!(next.len(), 4);
    }

    #[test]
    fn a_kind_change_or_new_symlink_target_is_a_change() {
        let mut link = file("l", 0, 10);
        link.kind = EntryKind::Symlink;
        link.target = Some("a".into());
        let base = synced(&[link.clone()]);
        let mut moved = link.clone();
        moved.target = Some("b".into());
        assert_eq!(
            reconcile(SyncMode::TwoWay, &base, &map(&[moved]), &map(&[landed(&link)])),
            [Action::Push("l".into())]
        );
    }
}
