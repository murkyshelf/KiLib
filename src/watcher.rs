//! Filesystem watching so the queue stays live without pressing Refresh.

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc::Sender;

use crate::logging;

/// Watches `dir` recursively, sending `()` on every create/remove/rename.
///
/// The returned watcher must be kept alive; dropping it stops the watch.
pub fn watch(dir: &Path, tx: Sender<()>) -> Result<RecommendedWatcher, String> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        match res {
            Ok(event) => {
                use notify::EventKind::*;
                // Access/metadata churn would cause pointless rescans.
                if matches!(event.kind, Create(_) | Remove(_) | Modify(_)) {
                    let _ = tx.send(());
                }
            }
            Err(e) => logging::error(format!("watch error: {e}")),
        }
    })
    .map_err(|e| format!("creating watcher: {e}"))?;

    watcher
        .watch(dir, RecursiveMode::Recursive)
        .map_err(|e| format!("watching {}: {e}", dir.display()))?;

    logging::info(format!("watching {} (recursive)", dir.display()));
    Ok(watcher)
}
