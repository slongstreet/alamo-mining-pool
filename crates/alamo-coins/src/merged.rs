//! Combine the parent chain's template stream with aux chain templates.

use alamo_core::job::JobId;
use alamo_core::work::{MergedWork, WorkTemplate};
use std::sync::Arc;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// A template stream as published by a [`TemplateSource`](crate::TemplateSource).
pub type TemplateReceiver = watch::Receiver<Option<Arc<WorkTemplate>>>;

/// Publish a [`MergedWork`] whenever the parent or any aux template changes, until
/// `shutdown` is cancelled or the parent source goes away.
///
/// Parent mining never waits on an aux node: aux templates that have not arrived yet are
/// simply left out and a non-clean job follows once they do.
pub async fn merge(
    mut parent: TemplateReceiver,
    mut aux: Vec<TemplateReceiver>,
    tx: watch::Sender<Option<Arc<MergedWork>>>,
    shutdown: CancellationToken,
) {
    let mut last_parent: Option<JobId> = None;
    loop {
        let Some(current) = parent.borrow_and_update().clone() else {
            // No parent template yet; wait for one.
            tokio::select! {
                changed = parent.changed() => if changed.is_err() { return },
                _ = shutdown.cancelled() => return,
            }
            continue;
        };
        let aux_templates: Vec<Arc<WorkTemplate>> = aux
            .iter_mut()
            .filter_map(|rx| rx.borrow_and_update().clone())
            .collect();
        let parent_changed = last_parent != Some(current.id);
        last_parent = Some(current.id);
        tx.send_replace(Some(Arc::new(MergedWork {
            clean_jobs: parent_changed && current.clean_jobs,
            parent: current,
            aux: aux_templates,
        })));

        let aux_changed = async {
            if aux.is_empty() {
                std::future::pending::<()>().await;
            }
            let pending = aux.iter_mut().map(|rx| Box::pin(rx.changed()));
            let _ = futures::future::select_all(pending).await;
        };
        tokio::select! {
            changed = parent.changed() => if changed.is_err() { return },
            _ = aux_changed => {}
            _ = shutdown.cancelled() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(id: u64, coin: &'static str, clean: bool) -> Arc<WorkTemplate> {
        let mut w = WorkTemplate::regtest_sample(1, None);
        w.id = JobId(id);
        w.coin = coin;
        w.clean_jobs = clean;
        Arc::new(w)
    }

    #[tokio::test]
    async fn aux_changes_are_non_clean_and_parent_changes_are_clean() {
        let (parent_tx, parent_rx) = watch::channel(None);
        let (aux_tx, aux_rx) = watch::channel(None);
        let (out_tx, mut out_rx) = watch::channel(None);
        let shutdown = CancellationToken::new();
        tokio::spawn(merge(
            parent_rx,
            vec![aux_rx],
            out_tx,
            shutdown.child_token(),
        ));

        parent_tx.send_replace(Some(template(1, "LTC", true)));
        out_rx.changed().await.unwrap();
        let m = out_rx.borrow_and_update().clone().unwrap();
        assert!(m.clean_jobs);
        assert!(m.aux.is_empty());

        aux_tx.send_replace(Some(template(1, "DOGE", true)));
        out_rx.changed().await.unwrap();
        let m = out_rx.borrow_and_update().clone().unwrap();
        assert!(!m.clean_jobs, "aux tip change must not flush parent jobs");
        assert_eq!(m.aux.len(), 1);
        assert_eq!(m.aux[0].coin, "DOGE");

        parent_tx.send_replace(Some(template(2, "LTC", false)));
        out_rx.changed().await.unwrap();
        let m = out_rx.borrow_and_update().clone().unwrap();
        assert!(!m.clean_jobs);
        assert_eq!(m.parent.id, JobId(2));
        assert_eq!(m.aux.len(), 1);

        parent_tx.send_replace(Some(template(3, "LTC", true)));
        out_rx.changed().await.unwrap();
        assert!(out_rx.borrow_and_update().clone().unwrap().clean_jobs);
        shutdown.cancel();
    }
}
