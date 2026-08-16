//! Thumbnails for Image nodes: a background decode worker plus a texture
//! cache. Decoding runs off the UI thread — a big JPEG must never stall a
//! frame; the worker wakes the UI per finished decode. Cache keys are node
//! idents (vault-relative paths). A vault reload must NOT clear the cache
//! wholesale: reloads are frequent while agents write notes, and dropping
//! every texture made all images flicker back through their placeholders.
//! Instead each decode records the file's (mtime, len) stamp and a reload
//! only evicts entries whose stamp no longer matches the disk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::time::SystemTime;

use eframe::egui;
use text_graph::thumb;

/// Longer side of a decoded thumbnail, px. Sized for the largest consumer
/// — the hover preview popup — so canvas cards and the detail pane always
/// render at or below native.
const THUMB_PX: u32 = 512;

/// Identity of the bytes a cache entry was built from. Shared with the
/// text-preview cache (previews.rs), which evicts the same way.
pub(super) type Stamp = (SystemTime, u64);

pub(super) fn file_stamp(p: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// Is a cached entry still the file on disk? Unknown stamps never count as
/// fresh — better one spurious re-decode than a stale picture.
pub(super) fn fresh(stored: &Option<Stamp>, current: Option<Stamp>) -> bool {
    stored.is_some() && *stored == current
}

type JobId = u64;
type Job = (String, JobId, PathBuf);
type JobResult = (String, JobId, Result<(thumb::Thumb, Option<Stamp>), ()>);

pub(super) enum ThumbState {
    Pending(JobId),
    Failed,
    Ready {
        tex: egui::TextureHandle,
        stamp: Option<Stamp>,
    },
}

pub(super) struct Thumbs {
    /// Decode worker, spawned on first request (it needs a Context to wake
    /// the UI, which Viewer::new doesn't have).
    jobs: Option<Sender<Job>>,
    results: Option<Receiver<JobResult>>,
    next_job: JobId,
    pub(super) cache: HashMap<String, ThumbState>,
}

impl Thumbs {
    pub(super) fn new() -> Self {
        Thumbs {
            jobs: None,
            results: None,
            next_job: 1,
            cache: HashMap::new(),
        }
    }

    /// Upload finished decodes as textures. Once per frame, before painting.
    pub(super) fn pump(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.results else { return };
        while let Ok((key, job, res)) = rx.try_recv() {
            // Accept only results still WANTED: if retain_fresh evicted the
            // Pending entry while this decode was in flight (the file
            // changed under it), inserting would pin the stale pixels until
            // the next reload — which may never come if the vault goes
            // quiet. Dropped results are re-queued by paint-time request()
            // against the new bytes.
            if !matches!(
                self.cache.get(&key),
                Some(ThumbState::Pending(wanted)) if *wanted == job
            ) {
                continue;
            }
            let state = match res {
                Ok((t, stamp)) => {
                    let img = egui::ColorImage::from_rgba_unmultiplied(
                        [t.w as usize, t.h as usize],
                        &t.rgba,
                    );
                    ThumbState::Ready {
                        tex: ctx.load_texture(
                            format!("thumb:{key}"),
                            img,
                            egui::TextureOptions::LINEAR,
                        ),
                        stamp,
                    }
                }
                Err(()) => ThumbState::Failed,
            };
            self.cache.insert(key, state);
        }
    }

    /// Queue a decode for `key` unless already queued or done.
    pub(super) fn request(&mut self, ctx: &egui::Context, key: &str, abs: PathBuf) {
        if self.cache.contains_key(key) {
            return;
        }
        if self.jobs.is_none() {
            let (job_tx, job_rx) = std::sync::mpsc::channel::<Job>();
            let (res_tx, res_rx) = std::sync::mpsc::channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                while let Ok((key, job, path)) = job_rx.recv() {
                    // stamp BEFORE decoding: if the file changes mid-decode,
                    // the stale stamp makes the next reload re-decode it
                    let stamp = file_stamp(&path);
                    let res = thumb::decode(&path, THUMB_PX)
                        .map(|t| (t, stamp))
                        .map_err(|e| eprintln!("thumbnail {key}: {e:#}"));
                    if res_tx.send((key, job, res)).is_err() {
                        return; // viewer is gone
                    }
                    ctx.request_repaint();
                }
            });
            self.jobs = Some(job_tx);
            self.results = Some(res_rx);
        }
        let job = self.next_job;
        self.next_job = self
            .next_job
            .checked_add(1)
            .expect("thumbnail job generation overflow");
        self.cache.insert(key.to_string(), ThumbState::Pending(job));
        if let Some(tx) = &self.jobs {
            let _ = tx.send((key.to_string(), job, abs));
        }
    }

    /// Width/height of the decoded thumbnail, if ready.
    pub(super) fn aspect(&self, key: &str) -> Option<f32> {
        match self.cache.get(key) {
            Some(ThumbState::Ready { tex, .. }) => {
                let s = tex.size_vec2();
                (s.y > 0.0).then(|| s.x / s.y)
            }
            _ => None,
        }
    }

    /// Vault reload: evict only entries whose file changed (or vanished);
    /// unchanged images keep their textures so nothing flickers. Pending
    /// and Failed entries drop too — the retry is lazy and cheap.
    pub(super) fn retain_fresh(&mut self, root: &Path) {
        self.cache.retain(|key, state| match state {
            ThumbState::Ready { stamp, .. } => fresh(stamp, file_stamp(&root.join(key))),
            _ => false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn freshness_requires_a_known_matching_stamp() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let a = Some((t, 42u64));
        let b = Some((t, 43u64));
        assert!(fresh(&a, a), "same stamp is fresh");
        assert!(!fresh(&a, b), "size change evicts");
        assert!(!fresh(&a, None), "vanished file evicts");
        assert!(!fresh(&None, None), "unknown stored stamp never fresh");
        assert!(!fresh(&None, a), "unknown stored stamp never fresh");
    }

    /// A decode finishing AFTER retain_fresh evicted its key (file changed
    /// mid-decode, reload landed first) must be discarded — inserting it
    /// pinned the old pixels for the rest of the session once the vault
    /// went quiet, since request() early-returns on present keys.
    #[test]
    fn pump_discards_results_for_evicted_keys() {
        let ctx = egui::Context::default();
        let one_px = || thumb::Thumb {
            w: 1,
            h: 1,
            rgba: vec![0; 4],
        };
        let mut th = Thumbs::new();
        let (tx, rx) = std::sync::mpsc::channel();
        th.results = Some(rx);

        tx.send(("gone.png".to_string(), 1, Ok((one_px(), None))))
            .unwrap();
        th.pump(&ctx);
        assert!(
            th.cache.is_empty(),
            "orphaned result must not resurrect an evicted entry"
        );

        th.cache.insert("live.png".into(), ThumbState::Pending(2));
        tx.send(("live.png".to_string(), 2, Ok((one_px(), None))))
            .unwrap();
        th.pump(&ctx);
        assert!(
            matches!(th.cache.get("live.png"), Some(ThumbState::Ready { .. })),
            "a still-pending key accepts its decode"
        );
    }

    /// A result for an older request of the same key must not satisfy a
    /// replacement request queued after a reload.
    #[test]
    fn pump_discards_superseded_results_for_the_same_key() {
        let ctx = egui::Context::default();
        let one_px = || thumb::Thumb {
            w: 1,
            h: 1,
            rgba: vec![0; 4],
        };
        let mut th = Thumbs::new();
        let (tx, rx) = std::sync::mpsc::channel();
        th.results = Some(rx);
        th.cache
            .insert("changed.png".into(), ThumbState::Pending(2));

        tx.send(("changed.png".into(), 1, Ok((one_px(), None))))
            .unwrap();
        th.pump(&ctx);
        assert!(matches!(
            th.cache.get("changed.png"),
            Some(ThumbState::Pending(2))
        ));

        tx.send(("changed.png".into(), 2, Ok((one_px(), None))))
            .unwrap();
        th.pump(&ctx);
        assert!(matches!(
            th.cache.get("changed.png"),
            Some(ThumbState::Ready { .. })
        ));
    }

    #[test]
    fn file_stamp_tracks_content_changes() {
        let dir = std::env::temp_dir().join(format!("tg-thumb-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.png");
        std::fs::write(&p, b"one").unwrap();
        let s1 = file_stamp(&p);
        assert!(s1.is_some());
        std::fs::write(&p, b"longer content").unwrap();
        let s2 = file_stamp(&p);
        assert!(!fresh(&s1, s2), "rewrite with new length must evict");
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(file_stamp(&p), None);
    }
}
