//! Noticing that protected folders changed.
//!
//! # What this is, and what it deliberately is not
//!
//! It is not a sync engine. It does not talk to the server, decide what to
//! upload, or know what an operation id is. It turns filesystem notifications
//! into rows in the queue the existing engine already drains, and stops.
//!
//! That division is the whole design. Filesystem notification streams are
//! noisy, duplicated, occasionally reordered, and silently incomplete — a
//! single save in a text editor can produce a create, three modifies and a
//! rename, and a busy folder can overflow the kernel's buffer and drop
//! everything. Anything built as "one event, one API call" inherits all of
//! that. So an event here means only *look at this path again*; what is
//! actually true is read from disk, and what to do about it is written down as
//! an intent that supersedes whatever was there before.
//!
//! The consequence worth stating: this is an optimisation. Deleting this file
//! entirely would leave a client that still syncs correctly, just not until
//! the next reconciliation. Every guarantee lives in [`super::reconcile`]; this
//! only makes the common case feel immediate.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::{EventKind, RecursiveMode};
use notify_debouncer_full::notify::event::{ModifyKind, RenameMode};
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, RecommendedCache};

use crate::backup::store::Root;
use crate::error::AppError;

/// How long a path must be quiet before its change is acted on.
///
/// Chosen for how Windows applications actually save. Word, Excel and most
/// editors write a temporary file, delete the original and rename the
/// temporary into its place; a browser download appends for as long as it
/// takes and then renames off `.crdownload`. Acting on the first event in any
/// of those sequences uploads a file that is about to stop existing.
///
/// 800ms is long enough to swallow those bursts and short enough that saving a
/// document and looking at Arciin feels immediate. It is a coalescing window,
/// not a delay budget: the file is read when the window closes, so a slow save
/// simply extends it rather than uploading something half-written.
const DEBOUNCE: Duration = Duration::from_millis(800);

/// How often the debouncer wakes to flush whatever has gone quiet.
const TICK: Duration = Duration::from_millis(200);

/// One normalised thing that happened to a protected folder.
///
/// Deliberately not `notify`'s event type. That type is a faithful description
/// of what a particular platform reported, which is exactly what the rest of
/// the client must not have to reason about — the engine should not contain a
/// match arm for `ModifyKind::Metadata(MetadataKind::WriteTime)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalChange {
    /// Something appeared or changed at this path. Whether it is a file, a
    /// folder, or already gone again is decided by reading the disk, not by
    /// the event that said so.
    Touched { root_id: String, relative: String },
    /// Something is no longer at this path.
    Gone { root_id: String, relative: String },
    /// The same thing, somewhere else, within one protected folder.
    Renamed {
        root_id: String,
        from: String,
        to: String,
    },
    /// The stream admitted it lost events, or said something this cannot
    /// interpret. The only honest response is to stop trusting it for this
    /// folder and read the folder instead.
    RescanNeeded { root_id: String },
}

impl LocalChange {
    pub fn root_id(&self) -> &str {
        match self {
            LocalChange::Touched { root_id, .. }
            | LocalChange::Gone { root_id, .. }
            | LocalChange::Renamed { root_id, .. }
            | LocalChange::RescanNeeded { root_id } => root_id,
        }
    }
}

/// Where a path sits relative to the folders being watched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Located {
    /// Inside this protected folder, at this relative path.
    Inside { root_id: String, relative: String },
    /// The protected folder itself, rather than something in it.
    TheRootItself { root_id: String },
    /// Not in any protected folder. Nothing here is any of our business.
    Outside,
}

/// Which protected folder a path belongs to, if any.
///
/// Longest match wins, because one protected folder can sit inside another and
/// the inner one is the more specific answer.
///
/// Comparison is case-insensitive, as Windows is. A path that arrives spelled
/// `C:\USERS\...` when the root was recorded as `C:\Users\...` is the same
/// place, and treating it as a different one would silently watch nothing.
pub fn locate(roots: &[Root], path: &Path) -> Located {
    let needle = normalise(path);
    let mut best: Option<(usize, Located)> = None;

    for root in roots {
        let base = normalise(&root.local_path);
        if needle == base {
            let found = Located::TheRootItself {
                root_id: root.id.clone(),
            };
            if best.as_ref().is_none_or(|(len, _)| base.len() > *len) {
                best = Some((base.len(), found));
            }
            continue;
        }
        // The separator matters: `C:\Data2` must not match the root `C:\Data`.
        let prefix = format!("{base}\\");
        let Some(rest) = needle.strip_prefix(&prefix) else {
            continue;
        };
        if best.as_ref().is_some_and(|(len, _)| base.len() <= *len) {
            continue;
        }
        best = Some((
            base.len(),
            Located::Inside {
                root_id: root.id.clone(),
                // The protocol's separator, not Windows'.
                relative: rest.replace('\\', "/"),
            },
        ));
    }

    best.map(|(_, found)| found).unwrap_or(Located::Outside)
}

fn normalise(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// Turn one raw event into zero or more normalised changes.
///
/// Zero is a perfectly good answer: most of what a watcher reports is about
/// paths nobody asked to protect.
pub fn normalise_event(roots: &[Root], event: &notify::Event) -> Vec<LocalChange> {
    // The stream itself saying it fell behind. Nothing it reports after this
    // can be trusted to be complete, so the folder gets read instead.
    if event.need_rescan() {
        let mut out = Vec::new();
        for path in &event.paths {
            match locate(roots, path) {
                Located::Inside { root_id, .. } | Located::TheRootItself { root_id } => {
                    out.push(LocalChange::RescanNeeded { root_id })
                }
                Located::Outside => {}
            }
        }
        // An overflow with no usable path is still an overflow: every folder
        // has to be re-read, because there is no way to know which one it was.
        if out.is_empty() {
            out.extend(roots.iter().map(|root| LocalChange::RescanNeeded {
                root_id: root.id.clone(),
            }));
        }
        return out;
    }

    match &event.kind {
        // A rename the debouncer managed to pair up. This is the only event
        // that carries identity, and it is why a renamed 2 GB file costs one
        // small request rather than a fresh upload.
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if event.paths.len() >= 2 => {
            let from = locate(roots, &event.paths[0]);
            let to = locate(roots, &event.paths[1]);
            match (from, to) {
                (
                    Located::Inside {
                        root_id: a,
                        relative: from,
                    },
                    Located::Inside {
                        root_id: b,
                        relative: to,
                    },
                ) if a == b => vec![LocalChange::Renamed {
                    root_id: a,
                    from,
                    to,
                }],

                // Between two protected folders. Not one move: each folder has
                // its own identity on the server, and an entry cannot belong to
                // both. It leaves one and arrives in the other.
                (
                    Located::Inside {
                        root_id: a,
                        relative: from,
                    },
                    Located::Inside {
                        root_id: b,
                        relative: to,
                    },
                ) => vec![
                    LocalChange::Gone {
                        root_id: a,
                        relative: from,
                    },
                    LocalChange::Touched {
                        root_id: b,
                        relative: to,
                    },
                ],

                // Moved out of a protected folder. From this folder's point of
                // view the file is gone, and it is not followed: backup does
                // not extend to wherever somebody dragged it.
                (
                    Located::Inside {
                        root_id,
                        relative: from,
                    },
                    _,
                ) => vec![LocalChange::Gone {
                    root_id,
                    relative: from,
                }],

                // Moved in from somewhere unprotected: new content here.
                (
                    _,
                    Located::Inside {
                        root_id,
                        relative: to,
                    },
                ) => vec![LocalChange::Touched {
                    root_id,
                    relative: to,
                }],

                // The protected folder itself was renamed. Nothing inside it
                // was deleted, and pretending otherwise would tombstone the
                // lot; the folder is re-read and reported unavailable instead.
                (Located::TheRootItself { root_id }, _)
                | (_, Located::TheRootItself { root_id }) => {
                    vec![LocalChange::RescanNeeded { root_id }]
                }

                (Located::Outside, Located::Outside) => Vec::new(),
            }
        }

        // Half a rename. The debouncer pairs these when it can; when it cannot
        // — the other half landed outside anything watched, or was lost — the
        // safe reading is the literal one.
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
            paths_as(roots, event, |root_id, relative| LocalChange::Gone {
                root_id,
                relative,
            })
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
            paths_as(roots, event, |root_id, relative| LocalChange::Touched {
                root_id,
                relative,
            })
        }

        EventKind::Remove(_) => paths_as(roots, event, |root_id, relative| LocalChange::Gone {
            root_id,
            relative,
        }),

        EventKind::Create(_) | EventKind::Modify(_) => {
            paths_as(roots, event, |root_id, relative| LocalChange::Touched {
                root_id,
                relative,
            })
        }

        // `Access` is reads and opens — nothing changed, so nothing to do.
        // `Any`/`Other` carry no meaning worth guessing at; the folder is
        // re-read rather than acted on.
        EventKind::Access(_) => Vec::new(),
        EventKind::Any | EventKind::Other => paths_as(roots, event, |root_id, _| {
            LocalChange::RescanNeeded { root_id }
        }),
    }
}

fn paths_as(
    roots: &[Root],
    event: &notify::Event,
    make: impl Fn(String, String) -> LocalChange,
) -> Vec<LocalChange> {
    let mut out = Vec::new();
    for path in &event.paths {
        match locate(roots, path) {
            Located::Inside { root_id, relative } => out.push(make(root_id, relative)),
            // Something happened to the protected folder itself. Whatever it
            // was, the answer is to read the folder rather than infer.
            Located::TheRootItself { root_id } => out.push(LocalChange::RescanNeeded { root_id }),
            Located::Outside => {}
        }
    }
    out
}

/// The registrations this process is holding.
///
/// One manager, one debouncer, one registration per protected folder. The
/// alternative — a thread per folder, started wherever it seemed convenient —
/// is how a client ends up watching the same folder three times after a resume
/// and uploading everything three times with it.
pub struct WatcherManager {
    inner: Mutex<Option<Active>>,
    /// The folder set the event handler resolves paths against.
    ///
    /// Shared with the watcher thread rather than captured, because the set
    /// changes — a folder is resumed, another is switched off — and a handler
    /// holding a snapshot would keep placing events in folders that are no
    /// longer protected.
    known_roots: Arc<Mutex<Vec<Root>>>,
}

struct Active {
    debouncer: Debouncer<notify::RecommendedWatcher, RecommendedCache>,
    /// Which folders are registered, and where they are. Kept so unwatching is
    /// exact and so a second `watch` of the same folder is a no-op rather than
    /// a duplicate.
    registered: HashMap<String, PathBuf>,
}

impl Default for WatcherManager {
    fn default() -> Self {
        Self::new()
    }
}

impl WatcherManager {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
            known_roots: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Start delivering changes for `roots`, replacing any existing set.
    ///
    /// Idempotent by construction: it computes the difference against what is
    /// already registered, so calling it again after a resume adjusts rather
    /// than duplicates.
    pub fn watch(
        &self,
        roots: &[Root],
        sink: Arc<dyn Fn(Vec<LocalChange>) + Send + Sync>,
    ) -> Result<(), AppError> {
        let mut guard = self.inner.lock().unwrap();

        // Visible to the handler before anything is registered, so an event
        // that arrives immediately can already be placed.
        *self.known_roots.lock().unwrap() = roots.to_vec();

        if guard.is_none() {
            let known_for_handler = Arc::clone(&self.known_roots);
            let sink_for_handler = Arc::clone(&sink);

            let debouncer = new_debouncer(
                DEBOUNCE,
                Some(TICK),
                move |result: DebounceEventResult| match result {
                    Ok(events) => {
                        let roots = known_for_handler.lock().unwrap().clone();
                        let mut changes = Vec::new();
                        for event in events {
                            changes.extend(normalise_event(&roots, &event));
                        }
                        if !changes.is_empty() {
                            sink_for_handler(changes);
                        }
                    }
                    Err(errors) => {
                        // A watcher error is not a statement about the files.
                        // Re-read the folders rather than infer anything from
                        // events that did not arrive.
                        let roots = known_for_handler.lock().unwrap().clone();
                        for error in &errors {
                            tracing::warn!(kind = ?error.kind, "the filesystem watcher reported an error");
                        }
                        sink_for_handler(
                            roots
                                .iter()
                                .map(|root| LocalChange::RescanNeeded {
                                    root_id: root.id.clone(),
                                })
                                .collect(),
                        );
                    }
                },
            )
            .map_err(|err| {
                tracing::error!(error = %err, "the filesystem watcher could not be created");
                AppError::internal("Arciin could not watch your folders for changes.")
            })?;

            *guard = Some(Active {
                debouncer,
                registered: HashMap::new(),
            });
        }

        let active = guard.as_mut().expect("just created");

        let wanted: HashMap<String, PathBuf> = roots
            .iter()
            .map(|root| (root.id.clone(), root.local_path.clone()))
            .collect();

        // Stop watching folders that are no longer protected, or that moved.
        let stale: Vec<String> = active
            .registered
            .iter()
            .filter(|(id, path)| wanted.get(*id) != Some(*path))
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            if let Some(path) = active.registered.remove(&id) {
                let _ = active.debouncer.unwatch(&path);
                tracing::info!(root_id = %id, "stopped watching a protected folder");
            }
        }

        for (id, path) in wanted {
            if active.registered.contains_key(&id) {
                continue;
            }
            match active.debouncer.watch(&path, RecursiveMode::Recursive) {
                Ok(()) => {
                    active.registered.insert(id.clone(), path);
                    tracing::info!(root_id = %id, "watching a protected folder");
                }
                Err(err) => {
                    // A folder that cannot be watched is not a folder that is
                    // gone. Reconciliation still covers it; it simply will not
                    // feel immediate.
                    tracing::warn!(
                        root_id = %id,
                        error = %err,
                        "a protected folder could not be watched; it will be reconciled instead"
                    );
                }
            }
        }

        Ok(())
    }

    /// Stop watching everything and release the handles.
    ///
    /// Called when backup stops, the profile is disabled, or the device is
    /// revoked — all of which must leave nothing running in the background.
    pub fn stop(&self) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(active) = guard.take() {
            tracing::info!(
                folders = active.registered.len(),
                "stopped watching all protected folders"
            );
            drop(active);
        }
        self.known_roots.lock().unwrap().clear();
    }

    /// Which folders are currently registered. For diagnostics and tests.
    pub fn registered(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap()
            .as_ref()
            .map(|active| active.registered.keys().cloned().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::store::RootStatus;

    fn root(id: &str, path: &str) -> Root {
        Root {
            id: id.into(),
            kind: "CUSTOM".into(),
            display_name: id.into(),
            local_path: PathBuf::from(path),
            enabled: true,
            status: RootStatus::Active,
        }
    }

    fn event(kind: EventKind, paths: &[&str]) -> notify::Event {
        notify::Event {
            kind,
            paths: paths.iter().map(PathBuf::from).collect(),
            attrs: Default::default(),
        }
    }

    // --- Placing a path ---------------------------------------------------

    #[test]
    fn a_file_in_a_protected_folder_is_placed_in_it() {
        let roots = [root("r1", r"C:\Protected")];
        assert_eq!(
            locate(&roots, Path::new(r"C:\Protected\notes\a.txt")),
            Located::Inside {
                root_id: "r1".into(),
                relative: "notes/a.txt".into(),
            }
        );
    }

    #[test]
    fn placement_ignores_case_because_windows_does() {
        let roots = [root("r1", r"C:\Protected")];
        assert!(matches!(
            locate(&roots, Path::new(r"c:\PROTECTED\A.TXT")),
            Located::Inside { .. }
        ));
    }

    #[test]
    fn a_sibling_folder_with_a_shared_prefix_is_not_inside() {
        // `C:\Data2` starts with `C:\Data` as a string and is a different
        // folder entirely. Without the separator check, everything in it
        // would be backed up under the wrong root.
        let roots = [root("r1", r"C:\Data")];
        assert_eq!(
            locate(&roots, Path::new(r"C:\Data2\secret.txt")),
            Located::Outside
        );
    }

    #[test]
    fn the_innermost_protected_folder_wins() {
        // Nesting is allowed, and the more specific answer is the right one.
        let roots = [root("outer", r"C:\Work"), root("inner", r"C:\Work\Current")];
        assert_eq!(
            locate(&roots, Path::new(r"C:\Work\Current\a.txt")),
            Located::Inside {
                root_id: "inner".into(),
                relative: "a.txt".into(),
            }
        );
    }

    #[test]
    fn the_folder_itself_is_distinguished_from_things_in_it() {
        let roots = [root("r1", r"C:\Protected")];
        assert_eq!(
            locate(&roots, Path::new(r"C:\Protected")),
            Located::TheRootItself {
                root_id: "r1".into()
            }
        );
    }

    #[test]
    fn a_path_nobody_protects_is_nobodys_business() {
        let roots = [root("r1", r"C:\Protected")];
        assert_eq!(
            locate(&roots, Path::new(r"C:\Windows\System32\config")),
            Located::Outside
        );
    }

    #[test]
    fn relative_paths_use_the_protocols_separator() {
        let roots = [root("r1", r"C:\Protected")];
        let Located::Inside { relative, .. } = locate(&roots, Path::new(r"C:\Protected\a\b\c.txt"))
        else {
            panic!("should be inside")
        };
        assert_eq!(relative, "a/b/c.txt");
        assert!(!relative.contains('\\'));
    }

    // --- Normalising events ------------------------------------------------

    #[test]
    fn a_created_file_is_something_to_look_at() {
        let roots = [root("r1", r"C:\Protected")];
        let changes = normalise_event(
            &roots,
            &event(
                EventKind::Create(notify::event::CreateKind::File),
                &[r"C:\Protected\new.txt"],
            ),
        );
        assert_eq!(
            changes,
            vec![LocalChange::Touched {
                root_id: "r1".into(),
                relative: "new.txt".into()
            }]
        );
    }

    #[test]
    fn a_removed_file_is_gone() {
        let roots = [root("r1", r"C:\Protected")];
        let changes = normalise_event(
            &roots,
            &event(
                EventKind::Remove(notify::event::RemoveKind::File),
                &[r"C:\Protected\old.txt"],
            ),
        );
        assert_eq!(
            changes,
            vec![LocalChange::Gone {
                root_id: "r1".into(),
                relative: "old.txt".into()
            }]
        );
    }

    #[test]
    fn a_rename_within_one_folder_keeps_its_identity() {
        // The case worth getting right: this is what turns a renamed 2 GB file
        // into one small request instead of a fresh upload and a deletion.
        let roots = [root("r1", r"C:\Protected")];
        let changes = normalise_event(
            &roots,
            &event(
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                &[r"C:\Protected\a.txt", r"C:\Protected\b.txt"],
            ),
        );
        assert_eq!(
            changes,
            vec![LocalChange::Renamed {
                root_id: "r1".into(),
                from: "a.txt".into(),
                to: "b.txt".into()
            }]
        );
    }

    #[test]
    fn a_move_between_two_protected_folders_is_a_leave_and_an_arrive() {
        // Not one move. Each protected folder is its own root on the server
        // with its own opaque identity, and one entry cannot belong to both.
        let roots = [root("r1", r"C:\One"), root("r2", r"C:\Two")];
        let changes = normalise_event(
            &roots,
            &event(
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                &[r"C:\One\a.txt", r"C:\Two\a.txt"],
            ),
        );
        assert_eq!(
            changes,
            vec![
                LocalChange::Gone {
                    root_id: "r1".into(),
                    relative: "a.txt".into()
                },
                LocalChange::Touched {
                    root_id: "r2".into(),
                    relative: "a.txt".into()
                },
            ]
        );
    }

    #[test]
    fn a_move_out_of_a_protected_folder_is_a_deletion_from_it() {
        // And the file is not followed. Backup does not extend to wherever
        // somebody dragged it.
        let roots = [root("r1", r"C:\Protected")];
        let changes = normalise_event(
            &roots,
            &event(
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                &[r"C:\Protected\a.txt", r"C:\Elsewhere\a.txt"],
            ),
        );
        assert_eq!(
            changes,
            vec![LocalChange::Gone {
                root_id: "r1".into(),
                relative: "a.txt".into()
            }]
        );
    }

    #[test]
    fn a_move_into_a_protected_folder_is_new_content() {
        let roots = [root("r1", r"C:\Protected")];
        let changes = normalise_event(
            &roots,
            &event(
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                &[r"C:\Elsewhere\a.txt", r"C:\Protected\a.txt"],
            ),
        );
        assert_eq!(
            changes,
            vec![LocalChange::Touched {
                root_id: "r1".into(),
                relative: "a.txt".into()
            }]
        );
    }

    #[test]
    fn renaming_the_protected_folder_itself_never_deletes_its_contents() {
        // The catastrophic misreading: the folder moved, so every path under
        // it "disappeared". Nothing in it was deleted, and the only safe
        // response is to go and look.
        let roots = [root("r1", r"C:\Protected")];
        let changes = normalise_event(
            &roots,
            &event(
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                &[r"C:\Protected", r"C:\Renamed"],
            ),
        );
        assert_eq!(
            changes,
            vec![LocalChange::RescanNeeded {
                root_id: "r1".into()
            }]
        );
    }

    #[test]
    fn an_unpaired_rename_away_reads_as_gone() {
        let roots = [root("r1", r"C:\Protected")];
        let changes = normalise_event(
            &roots,
            &event(
                EventKind::Modify(ModifyKind::Name(RenameMode::From)),
                &[r"C:\Protected\a.txt"],
            ),
        );
        assert_eq!(
            changes,
            vec![LocalChange::Gone {
                root_id: "r1".into(),
                relative: "a.txt".into()
            }]
        );
    }

    #[test]
    fn reading_a_file_changes_nothing() {
        // Opening a document must not queue an upload of it.
        let roots = [root("r1", r"C:\Protected")];
        let changes = normalise_event(
            &roots,
            &event(
                EventKind::Access(notify::event::AccessKind::Open(
                    notify::event::AccessMode::Read,
                )),
                &[r"C:\Protected\a.txt"],
            ),
        );
        assert!(changes.is_empty());
    }

    #[test]
    fn events_about_unprotected_paths_are_dropped() {
        let roots = [root("r1", r"C:\Protected")];
        let changes = normalise_event(
            &roots,
            &event(
                EventKind::Create(notify::event::CreateKind::File),
                &[r"C:\Somewhere\else.txt"],
            ),
        );
        assert!(changes.is_empty());
    }

    #[test]
    fn a_dropped_event_stream_asks_for_a_rescan_rather_than_guessing() {
        // Windows drops notifications when its buffer overflows. Carrying on
        // as though the surviving events were the whole story is how a client
        // silently stops backing up a busy folder.
        let roots = [root("r1", r"C:\Protected")];
        let mut overflowed = event(
            EventKind::Create(notify::event::CreateKind::File),
            &[r"C:\Protected\a.txt"],
        );
        overflowed = overflowed.set_flag(notify::event::Flag::Rescan);
        assert_eq!(
            normalise_event(&roots, &overflowed),
            vec![LocalChange::RescanNeeded {
                root_id: "r1".into()
            }]
        );
    }

    #[test]
    fn an_overflow_with_no_usable_path_rescans_everything() {
        // There is no way to know which folder it was, and assuming the
        // wrong one leaves the right one stale indefinitely.
        let roots = [root("r1", r"C:\One"), root("r2", r"C:\Two")];
        let overflowed = event(EventKind::Create(notify::event::CreateKind::Any), &[])
            .set_flag(notify::event::Flag::Rescan);
        let changes = normalise_event(&roots, &overflowed);
        assert_eq!(changes.len(), 2);
        assert!(changes
            .iter()
            .all(|c| matches!(c, LocalChange::RescanNeeded { .. })));
    }

    // --- Registrations -----------------------------------------------------

    #[test]
    fn a_manager_with_nothing_registered_reports_nothing() {
        let manager = WatcherManager::new();
        assert!(manager.registered().is_empty());
    }

    #[test]
    fn stopping_an_idle_manager_is_harmless() {
        let manager = WatcherManager::new();
        manager.stop();
        assert!(manager.registered().is_empty());
    }
}
