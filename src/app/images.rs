//! Thumbnails for Image nodes: a background decode worker plus a texture
//! cache. Decoding runs off the UI thread — a big JPEG must never stall a
//! frame; the worker wakes the UI per finished decode. Cache keys are node
//! idents (vault-relative paths), and a vault reload clears the cache
//! because the files may have changed on disk.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use eframe::egui;
use text_graph::thumb;

/// Longer side of a decoded thumbnail, px. Cards on the canvas cap below
/// this; the detail pane's preview column fits within it too.
const THUMB_PX: u32 = 256;

pub(super) enum ThumbState {
    Pending,
    Failed,
    Ready(egui::TextureHandle),
}

pub(super) struct Thumbs {
    /// Decode worker, spawned on first request (it needs a Context to wake
    /// the UI, which Viewer::new doesn't have).
    jobs: Option<Sender<(String, PathBuf)>>,
    results: Option<Receiver<(String, Result<thumb::Thumb, ()>)>>,
    pub(super) cache: HashMap<String, ThumbState>,
}

impl Thumbs {
    pub(super) fn new() -> Self {
        Thumbs {
            jobs: None,
            results: None,
            cache: HashMap::new(),
        }
    }

    /// Upload finished decodes as textures. Once per frame, before painting.
    pub(super) fn pump(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.results else { return };
        while let Ok((key, res)) = rx.try_recv() {
            let state = match res {
                Ok(t) => {
                    let img = egui::ColorImage::from_rgba_unmultiplied(
                        [t.w as usize, t.h as usize],
                        &t.rgba,
                    );
                    ThumbState::Ready(ctx.load_texture(
                        format!("thumb:{key}"),
                        img,
                        egui::TextureOptions::LINEAR,
                    ))
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
            let (job_tx, job_rx) = std::sync::mpsc::channel::<(String, PathBuf)>();
            let (res_tx, res_rx) = std::sync::mpsc::channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                while let Ok((key, path)) = job_rx.recv() {
                    let res = thumb::decode(&path, THUMB_PX)
                        .map_err(|e| eprintln!("thumbnail {key}: {e:#}"));
                    if res_tx.send((key, res)).is_err() {
                        return; // viewer is gone
                    }
                    ctx.request_repaint();
                }
            });
            self.jobs = Some(job_tx);
            self.results = Some(res_rx);
        }
        self.cache.insert(key.to_string(), ThumbState::Pending);
        if let Some(tx) = &self.jobs {
            let _ = tx.send((key.to_string(), abs));
        }
    }

    /// Width/height of the decoded thumbnail, if ready.
    pub(super) fn aspect(&self, key: &str) -> Option<f32> {
        match self.cache.get(key) {
            Some(ThumbState::Ready(tex)) => {
                let s = tex.size_vec2();
                (s.y > 0.0).then(|| s.x / s.y)
            }
            _ => None,
        }
    }

    /// Drop everything (vault reload — files may have changed on disk).
    pub(super) fn clear(&mut self) {
        self.cache.clear();
    }
}
