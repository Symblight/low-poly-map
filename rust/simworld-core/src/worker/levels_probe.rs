//! The archive's data levels probe (api 1.11): opens the archive in the background, falls back
//! to the levels of the disk cache offline (review H1).

use std::sync::Arc;

use crossbeam_channel::Sender;

use super::{Worker, WorkerMsg};
use crate::fetch::{FetchErrorKind, Fetcher};

impl Worker {
    /// Open the archive in the background and report its data levels ([`WorkerMsg::Levels`]):
    /// retried every `cache.open_retry_s` while unreachable (each failure reported as
    /// [`WorkerMsg::OpenFailed`] for the status); offline, the levels a warm disk cache
    /// recorded are reported instead (once). A tileset contract violation ends the probe (the
    /// fetcher reported it as an error).
    pub(super) fn spawn_levels_probe(rt: &tokio::runtime::Runtime, f: Arc<Fetcher>, tx: Sender<WorkerMsg>) {
        rt.spawn(async move {
            loop {
                match f.open_levels().await {
                    Ok(l) => {
                        let _ = tx.send(WorkerMsg::Levels(l));
                        return;
                    }
                    Err(e) if e.kind == FetchErrorKind::Contract => {
                        let _ = tx.send(WorkerMsg::OpenFailed(e));
                        return;
                    }
                    Err(e) => {
                        let cached = f.cached_levels().await;
                        if cached.is_some() {
                            let _ = tx.send(WorkerMsg::Info(format!(
                                "archive unreachable ({}); using the data levels of the disk cache",
                                e.message
                            )));
                        }
                        // Counted either way, so the status reads `degraded` (cached tiles
                        // only) rather than `ok` while the archive is unreachable.
                        let _ = tx.send(WorkerMsg::OpenFailed(e));
                        if let Some(l) = cached {
                            let _ = tx.send(WorkerMsg::Levels(l));
                            return;
                        }
                        tokio::time::sleep(f.open_retry()).await;
                    }
                }
            }
        });
    }
}
