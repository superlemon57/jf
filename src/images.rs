//! Background image loading: disk cache, decoding and textures.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use eframe::egui;

use crate::jellyfin::Client;

const WORKERS: usize = 6;
/// Beyond this, the oldest requests (cards already off screen) are dropped.
const MAX_PENDING: usize = 96;
const MAX_TEXTURES: usize = 400;

#[derive(Clone, PartialEq)]
pub struct ImageRef {
    pub id: String,
    pub kind: &'static str,
    pub width: u32,
    /// Changes when the image changes on the server, which invalidates the cache.
    pub tag: String,
}

impl ImageRef {
    fn key(&self) -> String {
        let tag: String = self.tag.chars().filter(char::is_ascii_alphanumeric).collect();
        format!("{}-{}-{}-{}", self.id, self.kind, self.width, tag)
    }
}

pub struct Loaded<'a> {
    pub texture: &'a egui::TextureHandle,
    /// Seconds since the image arrived, for the fade-in.
    pub age: f32,
}

enum Slot {
    Pending,
    Ready { texture: egui::TextureHandle, since: Instant, last_frame: u64 },
    Failed,
}

struct Job {
    key: String,
    image: ImageRef,
    client: Client,
}

type Queue = Arc<(Mutex<Vec<Job>>, Condvar)>;

pub struct Images {
    slots: HashMap<String, Slot>,
    queue: Queue,
    results: Receiver<(String, Option<egui::TextureHandle>)>,
    client: Option<Client>,
    frame: u64,
}

impl Images {
    pub fn new(ctx: &egui::Context) -> Self {
        let queue: Queue = Arc::default();
        let (sender, results) = channel();
        let directory = crate::config::cache_dir().join("images");
        let _ = std::fs::create_dir_all(&directory);
        for _ in 0..WORKERS {
            let (queue, sender, ctx, directory) = (queue.clone(), sender.clone(), ctx.clone(), directory.clone());
            std::thread::spawn(move || worker(queue, sender, ctx, directory));
        }
        Self { slots: HashMap::new(), queue, results, client: None, frame: 0 }
    }

    pub fn set_client(&mut self, client: Option<Client>) {
        self.client = client;
    }

    /// Call once per frame shown on screen.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        for (key, texture) in self.results.try_iter() {
            let slot = match texture {
                Some(texture) => Slot::Ready { texture, since: Instant::now(), last_frame: self.frame },
                None => Slot::Failed,
            };
            self.slots.insert(key, slot);
        }
        let ready = self.slots.values().filter(|slot| matches!(slot, Slot::Ready { .. })).count();
        if ready > MAX_TEXTURES {
            let limit = self.frame.saturating_sub(120);
            self.slots.retain(|_, slot| !matches!(slot, Slot::Ready { last_frame, .. } if *last_frame < limit));
        }
    }

    /// Returns the image if it is ready; otherwise starts loading it.
    pub fn get(&mut self, image: &ImageRef) -> Option<Loaded<'_>> {
        let key = image.key();
        if !self.slots.contains_key(&key) {
            let client = self.client.clone()?;
            let (jobs, wake) = &*self.queue;
            let mut jobs = jobs.lock().unwrap();
            jobs.push(Job { key: key.clone(), image: image.clone(), client });
            if jobs.len() > MAX_PENDING {
                self.slots.remove(&jobs.remove(0).key);
            }
            wake.notify_one();
            self.slots.insert(key, Slot::Pending);
            return None;
        }
        match self.slots.get_mut(&key) {
            Some(Slot::Ready { texture, since, last_frame }) => {
                *last_frame = self.frame;
                Some(Loaded { texture, age: since.elapsed().as_secs_f32() })
            }
            _ => None,
        }
    }
}

fn worker(
    queue: Queue,
    results: Sender<(String, Option<egui::TextureHandle>)>,
    ctx: egui::Context,
    directory: PathBuf,
) {
    loop {
        let job = {
            let (jobs, wake) = &*queue;
            let mut jobs = jobs.lock().unwrap();
            loop {
                // Most recent request first: it is the one on screen.
                match jobs.pop() {
                    Some(job) => break job,
                    None => jobs = wake.wait(jobs).unwrap(),
                }
            }
        };
        let texture =
            load(&job, &directory).map(|image| ctx.load_texture(&job.key, image, egui::TextureOptions::LINEAR));
        if results.send((job.key, texture)).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

fn load(job: &Job, directory: &std::path::Path) -> Option<egui::ColorImage> {
    let path = directory.join(&job.key);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => {
            let bytes = job.client.image(&job.image.id, job.image.kind, job.image.width).ok()?;
            let _ = std::fs::write(&path, &bytes);
            bytes
        }
    };
    let pixels = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let size = [pixels.width() as usize, pixels.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, pixels.as_raw()))
}
