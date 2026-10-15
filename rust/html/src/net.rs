//! What a page loads: stylesheets, images and fonts. Blitz asks for them
//! while it parses and styles the page; they are queued and loaded between
//! its passes, from the Volume, `data:` URLs, or HTTPS through the worker.

use std::sync::{Arc, Mutex};

use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request, Url};

/// Fetches an https URL's body, or says why it can't.
pub type Fetch = Arc<dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync>;

/// A resource the page asked for that it didn't get.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct NotLoaded {
    pub url: String,
    pub reason: String,
}

pub struct Loader {
    fetch: Fetch,
    queue: Mutex<Vec<(Request, Box<dyn NetHandler>)>>,
    failed: Mutex<Vec<NotLoaded>>,
}

impl NetProvider for Loader {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        self.queue.lock().unwrap().push((request, handler));
    }
}

impl Loader {
    pub fn new(fetch: Fetch) -> Self {
        Self { fetch, queue: Mutex::default(), failed: Mutex::default() }
    }

    /// Loads what the page has asked for since the last call, and returns
    /// whether it had asked for anything.
    pub fn load_queued(&self) -> bool {
        let queued = std::mem::take(&mut *self.queue.lock().unwrap());
        let any = !queued.is_empty();
        for (request, handler) in queued {
            let url = request.url;
            match self.load(&url) {
                Ok(bytes) => handler.bytes(url.to_string(), Bytes::from(bytes)),
                Err(reason) => {
                    let mut url = url.to_string();
                    // A data: URL can be huge, and says nothing useful past its type.
                    if url.starts_with("data:") && url.len() > 60 {
                        url.truncate(url.floor_char_boundary(60));
                        url.push('…');
                    }
                    self.failed.lock().unwrap().push(NotLoaded { url, reason })
                }
            }
        }
        any
    }

    pub fn failed(&self) -> Vec<NotLoaded> {
        self.failed.lock().unwrap().clone()
    }

    fn load(&self, url: &Url) -> Result<Vec<u8>, String> {
        match url.scheme() {
            "file" => {
                let path = url.to_file_path().map_err(|()| "not a file path".to_string())?;
                std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))
            }
            "data" => {
                let data = data_url::DataUrl::process(url.as_str()).map_err(|e| format!("{e:?}"))?;
                data.decode_to_vec().map(|(body, _)| body).map_err(|e| format!("{e:?}"))
            }
            "https" => (self.fetch)(url.as_str()),
            "http" => Err("only https URLs can be loaded".to_string()),
            scheme => Err(format!("{scheme}: URLs can't be loaded")),
        }
    }
}
